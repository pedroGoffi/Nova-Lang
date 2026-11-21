//! Pure AST-level helper dispatch. Does not introduce a native ABI.
use crate::ast::{self, Expr, Item, Stmt};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HelperMode {
    #[default]
    Macro,
    Function,
}

/// Only standard library *syntactic helpers* are dispatched here. The
/// implementations remain in .nova. User macros always go through the normal
/// hygienic macro engine and cannot be silently reclassified as functions.
fn helper_function(name: &str) -> Option<&'static str> {
    match name {
        "spawn" | "std::old::helpers::spawn" => Some("std::old::thread::spawn"),
        "yield_now" | "std::old::helpers::yield_now" => Some("std::old::thread::yield_now"),
        _ => None,
    }
}
fn visit_expr(expr: &mut Expr, mode: HelperMode, shadowed: &std::collections::HashSet<String>) {
    match expr {
        Expr::MacroCall(name, args) => {
            for arg in args.iter_mut() { visit_expr(arg, mode, shadowed); }
            if mode == HelperMode::Function {
                if !shadowed.contains(name) {
                    if let Some(func) = helper_function(name) {
                        let args = std::mem::take(args);
                        *expr = Expr::Call(func.into(), args);
                    }
                }
            }
        }
        Expr::Call(_, args) | Expr::GenericCall(_, _, args) | Expr::Array(args) =>
            for arg in args {visit_expr(arg,mode,shadowed);},
        Expr::MethodCall(target, _, args, _) => {
            visit_expr(target,mode,shadowed);
            for arg in args {visit_expr(arg,mode,shadowed);}
        }
        Expr::CallIndirect(target, args) => {
            visit_expr(target,mode,shadowed);
            for arg in args {visit_expr(arg,mode,shadowed);}
        }
        Expr::Binary(left,_,right) | Expr::Index(left,right) => {
            visit_expr(left,mode,shadowed);visit_expr(right,mode,shadowed);
        }
        Expr::Unary(_,val) | Expr::Deref(val) | Expr::Yield(val)
        | Expr::Await(val) | Expr::Spread(val) | Expr::Cast(val,_)
        | Expr::Field(val,_) | Expr::AddressOf{value:val,..} => visit_expr(val,mode,shadowed),
        Expr::StructInit(_,entries) | Expr::GenericStructInit(_,_,entries) =>
            for (_,val) in entries {visit_expr(val,mode,shadowed);},
        Expr::If{condition,then_body,else_body} => {
            visit_expr(condition,mode,shadowed);visit_stmts(then_body,mode,shadowed);visit_stmts(else_body,mode,shadowed);
        }
        Expr::Block(body) | Expr::Lambda{body,..} => visit_stmts(body,mode,shadowed),
        _=>{}
    }
}
fn visit_stmts(stmts: &mut [Stmt], mode: HelperMode, shadowed: &std::collections::HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Let{init,defer,..} => {
                visit_expr(init,mode,shadowed);
                if let Some(defer)=defer {visit_expr(defer,mode,shadowed);}
            }
            Stmt::Set{value,..} | Stmt::DerefSet{value,..} | Stmt::FieldSet{value,..}
            | Stmt::CompoundSet{value,..} | Stmt::Defer(value)
            | Stmt::Expr(value) | Stmt::Tail(value) | Stmt::Return(Some(value)) => visit_expr(value,mode,shadowed),
            Stmt::If{condition,then_body,else_body} => {
                visit_expr(condition,mode,shadowed);
                visit_stmts(then_body,mode,shadowed);
                if let Some(else_body)=else_body {visit_stmts(else_body,mode,shadowed);}
            }
            Stmt::While{condition,body} => {visit_expr(condition,mode,shadowed);visit_stmts(body,mode,shadowed);}
            Stmt::Loop(body) | Stmt::Unsafe(body) => visit_stmts(body,mode,shadowed),
            Stmt::For{start,end,body,..} => {
                visit_expr(start,mode,shadowed);visit_expr(end,mode,shadowed);visit_stmts(body,mode,shadowed);
            }
            Stmt::ForEach{iterable,body,..} => {visit_expr(iterable,mode,shadowed);visit_stmts(body,mode,shadowed);}
            Stmt::Match{value,arms} => {
                visit_expr(value,mode,shadowed);
                for arm in arms {visit_stmts(&mut arm.body,mode,shadowed);}
            }
            _=>{}
        }
    }
}
fn visit_items(items: &mut [Item], mode: HelperMode, shadowed: &std::collections::HashSet<String>) {
    for item in items {
        match item {
            Item::Public(inner) => visit_items(std::slice::from_mut(inner.as_mut()),mode,shadowed),
            Item::Namespace{items,..} => visit_items(items,mode,shadowed),
            Item::Function(f) => visit_stmts(&mut f.body,mode,shadowed),
            Item::Impl(block) => for f in &mut block.methods {visit_stmts(&mut f.body,mode,shadowed)},
            Item::CompileTimeConst(c) => visit_expr(&mut c.value,mode,shadowed),
            _=>{}
        }
    }
}
pub fn lower(program: &ast::Program, mode: HelperMode) -> ast::Program {
    if mode == HelperMode::Macro { return program.clone(); }
    let mut transformed = program.clone();
    let mut shadowed=std::collections::HashSet::new();
    fn collect_user_macros(items:&[Item], ns:&str, found:&mut std::collections::HashSet<String>) {
        for item in items {
            match item {
                Item::Public(inner)=>collect_user_macros(std::slice::from_ref(inner.as_ref()),ns,found),
                Item::Namespace{name,items}=>{
                    let next=if ns.is_empty(){name.clone()}else{format!("{ns}::{name}")};
                    collect_user_macros(items,&next,found);
                }
                Item::Macro(def) if !ns.starts_with("std::old::helpers") => {
                    found.insert(def.name.clone());
                    if !ns.is_empty(){found.insert(format!("{ns}::{}",def.name));}
                }
                _=>{}
            }
        }
    }
    collect_user_macros(&program.items,"",&mut shadowed);
    visit_items(&mut transformed.items,mode,&shadowed);
    transformed
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn function_mode_rewrites_only_std_helpers() {
        let mut call=Expr::MacroCall("spawn".into(),vec![Expr::Var("worker".into())]);
        visit_expr(&mut call, HelperMode::Function, &Default::default());
        assert!(matches!(call, Expr::Call(name,_) if name=="std::old::thread::spawn"));
        let mut local=Expr::MacroCall("user_macro".into(),vec![]);
        visit_expr(&mut local, HelperMode::Function, &Default::default());
        assert!(matches!(local, Expr::MacroCall(_, _)));
    }
}
