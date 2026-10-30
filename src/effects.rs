//! Compile-time effect contracts for Nova declarations.
//!
//! `[[pure]]` is a verified restricted effect: calls must target other `pure`
//! Nova functions and it forbids extern calls, unsafe blocks and suspension.
//! `[[io]]` is an explicit informative effect marker until full effect inference
//! and polymorphism are implemented. `[[unsafe]]` remains enforced by HIR.
use crate::ast::{self, Attribute, Expr, Function, FunctionMode, Item, Stmt};
use std::collections::{HashMap, HashSet};

fn qual(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.into()
    } else {
        format!("{ns}::{name}")
    }
}
fn lookup<'a>(
    map: &'a HashMap<String, Vec<Attribute>>,
    ns: &str,
    name: &str,
) -> Option<&'a Vec<Attribute>> {
    if name.contains("::") {
        return map.get(name);
    }
    let mut here = ns;
    loop {
        if let Some(entry) = map.get(&qual(here, name)) {
            return Some(entry);
        }
        if here.is_empty() {
            return None;
        }
        here = here
            .rsplit_once("::")
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
}
fn has(attrs: &[Attribute], effect: &str) -> bool {
    attrs.iter().any(|a| a.name == effect)
}
fn attrs(attrs: &[Attribute], context: &str, mode: Option<&FunctionMode>) -> Result<(), String> {
    ast::function_is_unsafe(attrs, context)?;
    let mut seen = HashSet::new();
    for a in attrs {
        if !seen.insert(a.name.as_str()) {
            return Err(format!("duplicate effect '{}' on {context}", a.name));
        }
        if !a.args.is_empty() && !matches!(a.name.as_str(), "effects" | "meta_intrinsic" | "meta_proc_intrinsic" | "proc_derive" | "proc_attribute" | "proc_macro") {
            return Err(format!(
                "effect '{}' on {context} expects no arguments",
                a.name
            ));
        }
    }
    if (has(attrs, "pure") || has(attrs, "const") || has(attrs, "comptime"))
        && (has(attrs, "io") || has(attrs, "unsafe") || has(attrs, "async"))
    {
        return Err(format!(
            "{context}: [[pure]] cannot be combined with [[io]], [[unsafe]] or [[async]]"
        ));
    }
    if (has(attrs, "const") || has(attrs, "comptime")) && !matches!(mode, Some(FunctionMode::Normal)) {
        return Err(format!("{context}: const functions must be synchronous"));
    }
    if has(attrs, "async") && !matches!(mode, Some(FunctionMode::Async)) {
        return Err(format!(
            "{context}: [[async]] requires an async fn declaration"
        ));
    }
    Ok(())
}
#[derive(Default)]
struct Effects {
    signatures: HashMap<String, Vec<Attribute>>,
    definitions: Vec<(String, String, Function)>,
}
impl Effects {
    fn collect(&mut self, items: &[Item], namespace: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    self.collect(std::slice::from_ref(inner.as_ref()), namespace)?
                }
                Item::Namespace { name, items } => self.collect(items, &qual(namespace, name))?,
                Item::Function(f) => {
                    let id = qual(namespace, &f.name);
                    attrs(&f.attributes, &format!("function '{id}'"), Some(&f.mode))?;
                    if self
                        .signatures
                        .insert(id.clone(), f.attributes.clone())
                        .is_some()
                    {
                        return Err(format!("duplicate effect signature for '{id}'"));
                    }
                    self.definitions.push((id, namespace.into(), f.clone()));
                }
                Item::Extern(f) => {
                    let id = qual(namespace, &f.name);
                    attrs(&f.attributes, &format!("extern '{id}'"), None)?;
                    if has(&f.attributes, "pure") {
                        return Err(format!(
                            "extern '{id}' cannot claim [[pure]] without a verified implementation"
                        ));
                    }
                    self.signatures.insert(id, f.attributes.clone());
                }
                Item::Impl(block) => {
                    for f in &block.methods {
                        attrs(
                            &f.attributes,
                            &format!("method '{}'", f.name),
                            Some(&f.mode),
                        )?;
                        if has(&f.attributes, "pure") || has(&f.attributes, "effects") {
                            return Err(format!("method '{}': effect-constrained methods require resolved receiver effects (not yet implemented)",f.name));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn call(&self, name: &str, ns: &str, owner: &str) -> Result<(), String> {
        let attrs = lookup(&self.signatures, ns, name).ok_or_else(|| {
            format!("[[pure]] function '{owner}' calls unresolved or effect-unknown '{name}'")
        })?;
        if !(has(attrs, "pure") || has(attrs, "const") || has(attrs, "comptime")) {
            return Err(format!("[[pure]] function '{owner}' calls impure '{name}'; annotate its verified implementation [[pure]]"));
        }
        if self
            .signatures
            .get(owner)
            .is_some_and(|owner_attrs| has(owner_attrs, "const"))
            && !has(attrs, "const")
        {
            return Err(format!("const fn '{owner}' cannot call non-const '{name}'"));
        }
        Ok(())
    }
    fn expr(&self, expr: &Expr, ns: &str, owner: &str) -> Result<(), String> {
        match expr {
            Expr::If { condition, then_body, else_body } => {
                self.expr(condition,ns,owner)?;
                self.stmts(then_body,ns,owner)?;
                self.stmts(else_body,ns,owner)?;
            }
            Expr::Block(body) => self.stmts(body,ns,owner)?,
            Expr::Call(name,args)|Expr::GenericCall(name,_,args)=>{
                self.call(name,ns,owner)?;
                for e in args {self.expr(e,ns,owner)?;}
            },
            Expr::MethodCall(_,name,_)=>return Err(format!("[[pure]] function '{owner}' uses method '{name}' without an effect-checked receiver")),
            Expr::CallIndirect(_,_)=>return Err(format!("[[pure]] function '{owner}' uses an indirect call with unknown effects")),
            Expr::Await(_)|Expr::Yield(_)=>return Err(format!("[[pure]] function '{owner}' cannot suspend")),
            Expr::Lambda{..}=>return Err(format!("[[pure]] function '{owner}' needs effect-typed closures; nested lambda not allowed yet")),
            Expr::Binary(a,_,b)|Expr::Index(a,b)=>{self.expr(a,ns,owner)?;self.expr(b,ns,owner)?;},
            Expr::Unary(_,value)|Expr::Deref(value)|Expr::Spread(value)|Expr::Field(value,_)|
            Expr::Cast(value,_)|Expr::AddressOf{value,..}=>self.expr(value,ns,owner)?,
            Expr::Array(values)|Expr::MacroCall(_,values)=>for value in values{self.expr(value,ns,owner)?;},
            Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>for (_,e) in fields{self.expr(e,ns,owner)?;},
            _=>{},
        }
        Ok(())
    }
    fn stmts(&self, body: &[Stmt], ns: &str, owner: &str) -> Result<(), String> {
        for stmt in body {
            match stmt {
                Stmt::Let { init, defer, .. } => {
                    self.expr(init, ns, owner)?;
                    if let Some(e) = defer {
                        self.expr(e, ns, owner)?;
                    }
                }
                Stmt::Set { value, .. }
                | Stmt::DerefSet { value, .. }
                | Stmt::FieldSet { value, .. }
                | Stmt::CompoundSet { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Defer(value)
                | Stmt::Expr(value) | Stmt::Tail(value) => self.expr(value, ns, owner)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.expr(condition, ns, owner)?;
                    self.stmts(then_body, ns, owner)?;
                    if let Some(body) = else_body {
                        self.stmts(body, ns, owner)?;
                    }
                }
                Stmt::While { condition, body } => {
                    self.expr(condition, ns, owner)?;
                    self.stmts(body, ns, owner)?;
                }
                Stmt::For {
                    start, end, body, ..
                } => {
                    self.expr(start, ns, owner)?;
                    self.expr(end, ns, owner)?;
                    self.stmts(body, ns, owner)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    self.expr(iterable, ns, owner)?;
                    self.stmts(body, ns, owner)?;
                }
                Stmt::Loop(body) => self.stmts(body, ns, owner)?,
                Stmt::Match { value, arms } => {
                    self.expr(value, ns, owner)?;
                    for arm in arms {
                        self.stmts(&arm.body, ns, owner)?;
                    }
                }
                Stmt::Unsafe(_) => {
                    return Err(format!(
                        "[[pure]] function '{owner}' contains an unsafe block"
                    ))
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }
}
/// Visit the expressions that can contain a direct call, including nested
/// control-flow and block-value expressions. Only *declared* effects are used;
/// unknown indirect/method calls are deliberately rejected in strict effects
/// functions until the type checker can supply resolved target signatures.
fn effect_expr(expr:&Expr, ns:&str, owner:&str, caller:&HashSet<String>, signatures:&HashMap<String,Vec<Attribute>>)->Result<(),String>{
    match expr {
        Expr::Call(name,args)|Expr::GenericCall(name,_,args) => {
            let called=lookup(signatures,ns,name).ok_or_else(||format!("error[NOVA-EFF001]: effect-constrained '{owner}' calls unresolved '{name}'"))?;
            let declared=called.iter().find(|a|a.name=="effects");
            if let Some(attributes)=declared {
                for effect in &attributes.args {
                    if !caller.contains(effect) {
                        return Err(format!("error[NOVA-EFF002]: '{owner}' calls '{name}' requiring effect '{effect}'; add it to #[effects(...)]"));
                    }
                }
            } else if !called.iter().any(|a|a.name=="pure" || a.name=="const") {
                return Err(format!("error[NOVA-EFF003]: '{owner}' calls '{name}' with unknown effects; annotate callee #[effects(...)] or verify it #[pure]"));
            }
            for a in args {effect_expr(a,ns,owner,caller,signatures)?;}
        }
        Expr::MethodCall(_,method,_)=>return Err(format!("error[NOVA-EFF004]: '{owner}' calls method '{method}' without a resolved effect signature")),
        Expr::CallIndirect(_,_)=>return Err(format!("error[NOVA-EFF004]: '{owner}' calls an untyped function pointer")),
        Expr::Lambda{..}=>return Err(format!("error[NOVA-EFF004]: '{owner}' captures a closure with unverified effects")),
        Expr::If{condition,then_body,else_body}=>{
            effect_expr(condition,ns,owner,caller,signatures)?;
            effect_stmts(then_body,ns,owner,caller,signatures)?;
            effect_stmts(else_body,ns,owner,caller,signatures)?;
        }
        Expr::Block(body)=>effect_stmts(body,ns,owner,caller,signatures)?,
        Expr::Binary(a,_,b)|Expr::Index(a,b)=>{
            effect_expr(a,ns,owner,caller,signatures)?;
            effect_expr(b,ns,owner,caller,signatures)?;
        }
        Expr::Unary(_,x)|Expr::Cast(x,_)|Expr::AddressOf{value:x,..}|
        Expr::Field(x,_)|Expr::Spread(x)|Expr::Deref(x)|Expr::Await(x)|Expr::Yield(x)=>
            effect_expr(x,ns,owner,caller,signatures)?,
        Expr::Array(values)|Expr::MacroCall(_,values)=>for e in values {effect_expr(e,ns,owner,caller,signatures)?;},
        Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>
            for (_,e) in fields {effect_expr(e,ns,owner,caller,signatures)?;},
        _=>{}
    }
    Ok(())
}
fn effect_stmts(body:&[Stmt],ns:&str,owner:&str,caller:&HashSet<String>,signatures:&HashMap<String,Vec<Attribute>>)->Result<(),String>{
    for s in body {
        match s {
            Stmt::Let{init,defer,..}=>{
                effect_expr(init,ns,owner,caller,signatures)?;
                if let Some(defer)=defer {effect_expr(defer,ns,owner,caller,signatures)?;}
            }
            Stmt::Set{value,..}|Stmt::DerefSet{value,..}|Stmt::FieldSet{value,..}|
            Stmt::CompoundSet{value,..}|Stmt::Return(Some(value))|Stmt::Defer(value)|
            Stmt::Expr(value)|Stmt::Tail(value)=>effect_expr(value,ns,owner,caller,signatures)?,
            Stmt::If{condition,then_body,else_body}=>{
                effect_expr(condition,ns,owner,caller,signatures)?;
                effect_stmts(then_body,ns,owner,caller,signatures)?;
                if let Some(body)=else_body {effect_stmts(body,ns,owner,caller,signatures)?;}
            }
            Stmt::While{condition,body}=>{
                effect_expr(condition,ns,owner,caller,signatures)?;
                effect_stmts(body,ns,owner,caller,signatures)?;
            }
            Stmt::For{start,end,body,..}=>{
                effect_expr(start,ns,owner,caller,signatures)?;
                effect_expr(end,ns,owner,caller,signatures)?;
                effect_stmts(body,ns,owner,caller,signatures)?;
            }
            Stmt::ForEach{iterable,body,..}=>{
                effect_expr(iterable,ns,owner,caller,signatures)?;
                effect_stmts(body,ns,owner,caller,signatures)?;
            }
            Stmt::Loop(body)|Stmt::Unsafe(body)=>effect_stmts(body,ns,owner,caller,signatures)?,
            Stmt::Match{value,arms}=>{
                effect_expr(value,ns,owner,caller,signatures)?;
                for arm in arms {effect_stmts(&arm.body,ns,owner,caller,signatures)?;}
            }
            Stmt::Return(None)|Stmt::Break|Stmt::Continue=>{}
        }
    }
    Ok(())
}

pub fn validate(program: &ast::Program) -> Result<(), String> {
    let mut effects = Effects::default();
    effects.collect(&program.items, "")?;
    for (name, namespace, function) in &effects.definitions {
        if let Some(contract)=function.attributes.iter().find(|a|a.name=="effects") {
            let declared=contract.args.iter().cloned().collect::<HashSet<_>>();
            effect_stmts(&function.body,namespace,name,&declared,&effects.signatures)?;
        }
        if has(&function.attributes, "pure") || has(&function.attributes, "const") || has(&function.attributes,"comptime") || has(&function.attributes,"comptime_assert") {
            effects.stmts(&function.body, namespace, name)?;
            if (has(&function.attributes,"comptime") || has(&function.attributes,"comptime_assert"))
                && !has(&function.attributes,"meta_proc_intrinsic") {
                let allowed=|ty:&ast::TypeRef|matches!(ty,ast::TypeRef::Named(t) if matches!(t.as_str(),"i64"|"i32"|"bool"|"str"));
                if !function.params.iter().all(|p|allowed(&p.ty)) || !allowed(&function.result) {
                    return Err(format!("error[NOVA-CTFE017]: '{name}' uses a parameter or return type unsupported by the compile-time evaluator"));
                }
                if !function.generics.is_empty() {
                    return Err(format!("error[NOVA-CTFE017]: '{name}' cannot be generic until typed CTFE monomorphization is implemented"));
                }
                if has(&function.attributes,"comptime_assert") && (!function.params.is_empty() || !matches!(&function.result,ast::TypeRef::Named(t) if t=="bool")) {
                    return Err(format!("error[NOVA-CTFE017]: #[comptime_assert] '{name}' must be fn() -> bool"));
                }
            }
            if has(&function.attributes, "const") {
                for p in &function.params {
                    if !matches!(&p.ty,ast::TypeRef::Named(t) if t=="i64" || t=="i32") {
                        return Err(format!("const fn '{name}': stage-0 const evaluator supports i32/i64 parameters"));
                    }
                }
                if !matches!(&function.result,ast::TypeRef::Named(t) if t=="i64" || t=="i32") {
                    return Err(format!(
                        "const fn '{name}': stage-0 const evaluator supports i32/i64 results"
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn check(source: &str) -> Result<(), String> {
        let tree = grammar::ProgramParser::new()
            .parse(source)
            .map_err(|e| format!("{e:?}"))?;
        validate(&tree)
    }
    #[test]
    fn pure_chain_is_accepted() {
        check("[[pure]] fn twice(x:i64)->i64{return x*2;} [[pure]] fn four(x:i64)->i64{return twice(twice(x));}").unwrap();
    }
    #[test]
    fn pure_rejects_unknown_impure_and_unsafe() {
        assert!(
            check("fn io()->i64{return 0;} [[pure]] fn p()->i64{return io();}")
                .unwrap_err()
                .contains("impure")
        );
        assert!(check("[[pure]] fn p()->i32{unsafe {return 1;}}")
            .unwrap_err()
            .contains("unsafe block"));
    }
    #[test]
    fn effect_marker_validation() {
        assert!(check("[[pure]] [[io]] fn p()->i32{return 0;}")
            .unwrap_err()
            .contains("cannot be combined"));
        assert!(check("[[async]] fn p()->i32{return 0;}")
            .unwrap_err()
            .contains("requires an async"));
    }
}

#[cfg(test)]
mod meta_effect_tests {
    use super::*;
    use crate::grammar;
    fn check(src:&str)->Result<(),String>{
        let ast=grammar::ProgramParser::new().parse(src).map_err(|e|format!("{e:?}"))?;
        crate::meta::validate(&ast)?;
        validate(&ast)
    }
    #[test]
    fn source_declared_effects_check_calls() {
        let prefix="#[meta_effect] trait network {} #[effects(network)] fn request()->i32{return 1;} ";
        check(&format!("{prefix} #[effects(network)] fn main()->i32{{return request();}}" )).unwrap();
        assert!(check(&format!("{prefix} #[effects(database)] fn main()->i32{{return request();}}" )).unwrap_err().contains("NOVA-META008"));
        assert!(check(&format!("{prefix} #[meta_effect] trait database {{}} #[effects(database)] fn main()->i32{{return request();}}" )).unwrap_err().contains("NOVA-EFF002"));
        assert!(check(&format!("{prefix} fn other()->i32{{return 1;}} #[effects(network)] fn main()->i32{{return other();}}" )).unwrap_err().contains("NOVA-EFF003"));
    }
}
