//! Bootstrap lowering for Nova's stackful generators, coroutines and futures.
//!
//! The surface syntax is first transformed into ordinary, typed Nova functions:
//!   coro fn f(x:i64)->i64 {yield x;return x;}
//! becomes a handle-returning factory, a body function, and a compiler-synthesized
//! FFI factory. The LLVM backend generates a trampoline for this FFI factory.
//!
//! This is intentionally a restricted Linux backend (only i64 ABI for coroutine
//! parameters, yielded values and final results); it does NOT pretend to provide
//! Rust-style Future<T>, pinning, ownership or a general-purpose borrow checker.
use crate::ast::{self, Expr, FunctionMode, Item, Stmt, TypeRef};

fn u64_ty() -> TypeRef {
    TypeRef::Named("u64".into())
}

fn expression(value: &mut Expr, mode: &FunctionMode) -> Result<(), String> {
    match value {
        Expr::Yield(inner) => {
            if !matches!(mode, FunctionMode::Generator | FunctionMode::Coroutine) {
                return Err("'yield' is only legal inside 'gen fn' or 'coro fn'".into());
            }
            expression(inner, mode)?;
            *value = Expr::Call(
                "std::coro::native_yield_value".into(),
                vec![(**inner).clone()],
            );
        }
        Expr::Await(inner) => {
            if !matches!(mode, FunctionMode::Async) {
                return Err("'await' is only legal inside 'async fn'".into());
            }
            expression(inner, mode)?;
            *value = Expr::Call(
                "std::task::native_await_value".into(),
                vec![(**inner).clone()],
            );
        }
        Expr::Binary(left, _, right) => {
            expression(left, mode)?;
            expression(right, mode)?;
        }
        Expr::Unary(_, v)
        | Expr::Spread(v)
        | Expr::Cast(v, _)
        | Expr::Deref(v)
        | Expr::Field(v, _)
        | Expr::AddressOf { value: v, .. } => expression(v, mode)?,
        Expr::CallIndirect(fun, args) => {
            expression(fun, mode)?;
            for v in args {
                expression(v, mode)?;
            }
        }
        Expr::Lambda { body, .. } => statements(body, &FunctionMode::Normal)?,
        Expr::Call(_, args) | Expr::GenericCall(_, _, args) | Expr::Array(args) => {
            for v in args {
                expression(v, mode)?;
            }
        }
        Expr::MethodCall(obj, _, args) => {
            expression(obj, mode)?;
            for v in args {
                expression(v, mode)?;
            }
        }
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
            for (_, value) in fields {
                expression(value, mode)?;
            }
        }
        Expr::Index(array, index) => {
            expression(array, mode)?;
            expression(index, mode)?;
        }
        _ => {}
    }
    Ok(())
}
fn statements(body: &mut [Stmt], mode: &FunctionMode) -> Result<(), String> {
    for stmt in body {
        match stmt {
            Stmt::Defer(init)
            | Stmt::Set { value: init, .. }
            | Stmt::FieldSet { value: init, .. }
            | Stmt::DerefSet { value: init, .. }
            | Stmt::CompoundSet { value: init, .. }
            | Stmt::Expr(init)
            | Stmt::Return(Some(init)) => expression(init, mode)?,
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                expression(condition, mode)?;
                statements(then_body, mode)?;
                if let Some(other) = else_body {
                    statements(other, mode)?;
                }
            }
            Stmt::While { condition, body } => {
                expression(condition, mode)?;
                statements(body, mode)?;
            }
            Stmt::For {
                start, end, body, ..
            } => {
                expression(start, mode)?;
                expression(end, mode)?;
                statements(body, mode)?;
            }
            Stmt::ForEach { .. } => {
                return Err("compiler bug: for-in must be desugared before coroutines".into())
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => statements(body, mode)?,
            Stmt::Match { value, arms } => {
                expression(value, mode)?;
                for arm in arms {
                    statements(&mut arm.body, mode)?;
                }
            }
            Stmt::Let { init, defer, .. } => {
                expression(init, mode)?;
                if let Some(d) = defer {
                    expression(d, mode)?;
                }
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}
fn expand(items: Vec<Item>) -> Result<Vec<Item>, String> {
    let mut output = Vec::new();
    for item in items {
        let (item, public) = match item {
            Item::Public(inner) => (*inner, true),
            x => (x, false),
        };
        match item {
            Item::Namespace { name, items } => {
                let ns = Item::Namespace {
                    name,
                    items: expand(items)?,
                };
                output.push(if public {
                    Item::Public(Box::new(ns))
                } else {
                    ns
                });
            }
            Item::Function(mut function) => {
                let mode = function.mode.clone();
                statements(&mut function.body, &mode)?;
                if matches!(mode, FunctionMode::Normal) {
                    let item = Item::Function(function);
                    output.push(if public {
                        Item::Public(Box::new(item))
                    } else {
                        item
                    });
                    continue;
                }
                if function.variadic {
                    return Err(format!(
                        "{}: variadic coroutine functions are not supported by the 0.8 bootstrap",
                        function.name
                    ));
                }
                if !function.generics.is_empty() {
                    return Err(format!(
                        "{}: coroutine generics are not supported by the 0.8 bootstrap backend",
                        function.name
                    ));
                }
                if !matches!(&function.result,TypeRef::Named(n) if n=="i64") {
                    return Err(format!(
                        "{}: gen/coro/async return type must be i64 in Nova 0.8 bootstrap",
                        function.name
                    ));
                }
                if function
                    .params
                    .iter()
                    .any(|p| !matches!(&p.ty,TypeRef::Named(n) if n=="i64" || n=="u64"))
                {
                    return Err(format!(
                        "{}: coroutine parameters must be i64 or u64 in Nova 0.8 bootstrap",
                        function.name
                    ));
                }
                if function.name.starts_with("__nova_") {
                    return Err("coroutine names starting with '__nova_' are reserved".into());
                }
                let name = function.name.clone();
                let factory = format!("__nova_make_{name}");
                let bodyname = format!("__nova_body_{name}");
                let params = function.params.clone();
                let args = params
                    .iter()
                    .map(|p| Expr::Var(p.name.clone()))
                    .collect::<Vec<_>>();
                function.name = bodyname;
                function.mode = FunctionMode::Normal;
                let function_attributes = function.attributes.clone();
                let function_bounds = function.concept_bounds.clone();
                // 1. real stackful body: yield/await are C ABI suspension calls.
                output.push(Item::Function(function));
                // 2. compiler-synthesized FFI symbol: LLVM backend materializes it.
                output.push(Item::Extern(ast::ExternFunction {
                    name: factory.clone(),
                    params: params.clone(),
                    result: u64_ty(),
                    c_variadic: false,
                    attributes: vec![],
                }));
                // 3. user-visible factory returning an opaque owned handle.
                let wrapper = Item::Function(ast::Function {
                    mode: FunctionMode::Normal,
                    name,
                    generics: vec![],
                    concept_bounds: function_bounds,
                    params,
                    variadic: false,
                    result: u64_ty(),
                    body: vec![Stmt::Return(Some(Expr::Call(factory, args)))],
                    is_virtual: false,
                    is_override: false,
                    attributes: function_attributes,
                });
                output.push(if public {
                    Item::Public(Box::new(wrapper))
                } else {
                    wrapper
                });
            }
            Item::Impl(mut block) => {
                for method in &mut block.methods {
                    if !matches!(method.mode, FunctionMode::Normal) {
                        return Err(format!("method '{}': coroutine methods are not supported in Nova 0.8 bootstrap",method.name));
                    }
                    statements(&mut method.body, &FunctionMode::Normal)?;
                }
                let item = Item::Impl(block);
                output.push(if public {
                    Item::Public(Box::new(item))
                } else {
                    item
                });
            }
            other => output.push(if public {
                Item::Public(Box::new(other))
            } else {
                other
            }),
        }
    }
    Ok(output)
}
// The keyword lowering below uses stable runtime ABI symbols. Programs with
// `std = { builtin = "std" }` already declare those symbols. A standalone Nova
// file is also allowed to use coroutine keywords without importing std, so
// synthesize ONLY missing runtime externs actually referenced by the lowered
// AST. Never duplicate an existing declaration or permit arbitrary std calls.
fn runtime_intrinsic(
    name: &str,
) -> Option<(
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
)> {
    match name {
        "std::coro::native_yield_value" => {
            Some(("coro", "native_yield_value", &[("value", "i64")], "i64"))
        }
        "std::coro::native_resume" => Some((
            "coro",
            "native_resume",
            &[("handle", "u64"), ("input", "i64")],
            "i64",
        )),
        "std::coro::native_drop" => Some(("coro", "native_drop", &[("handle", "u64")], "()")),
        "std::generator::native_next" => {
            Some(("generator", "native_next", &[("handle", "u64")], "i64"))
        }
        "std::generator::native_done" => {
            Some(("generator", "native_done", &[("handle", "u64")], "bool"))
        }
        "std::task::native_await_value" => {
            Some(("task", "native_await_value", &[("handle", "u64")], "i64"))
        }
        "std::task::native_block_on" => {
            Some(("task", "native_block_on", &[("handle", "u64")], "i64"))
        }
        "std::task::native_spawn" => Some(("task", "native_spawn", &[("handle", "u64")], "()")),
        _ => None,
    }
}
fn calls_in_expr(e: &Expr, out: &mut std::collections::BTreeSet<String>) {
    match e {
        Expr::Call(name, args) | Expr::MacroCall(name, args) | Expr::GenericCall(name, _, args) => {
            if runtime_intrinsic(name).is_some() {
                out.insert(name.clone());
            }
            for e in args {
                calls_in_expr(e, out);
            }
        }
        Expr::CallIndirect(callee, args) => {
            calls_in_expr(callee, out);
            for e in args {
                calls_in_expr(e, out);
            }
        }
        Expr::MethodCall(receiver, _, args) => {
            calls_in_expr(receiver, out);
            for e in args {
                calls_in_expr(e, out);
            }
        }
        Expr::Binary(left, _, right) | Expr::Index(left, right) => {
            calls_in_expr(left, out);
            calls_in_expr(right, out);
        }
        Expr::Unary(_, inner)
        | Expr::Spread(inner)
        | Expr::Cast(inner, _)
        | Expr::Deref(inner)
        | Expr::Field(inner, _)
        | Expr::AddressOf { value: inner, .. }
        | Expr::Yield(inner)
        | Expr::Await(inner) => calls_in_expr(inner, out),
        Expr::Lambda { body, .. } => calls_in_statements(body, out),
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
            for (_, value) in fields {
                calls_in_expr(value, out);
            }
        }
        Expr::Array(items) => {
            for e in items {
                calls_in_expr(e, out);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Unit
        | Expr::Var(_)
        | Expr::FunctionRef(_)
        | Expr::Zero(_) => {}
    }
}
fn calls_in_statements(statements: &[Stmt], out: &mut std::collections::BTreeSet<String>) {
    for stmt in statements {
        match stmt {
            Stmt::Let { init, defer, .. } => {
                calls_in_expr(init, out);
                if let Some(e) = defer {
                    calls_in_expr(e, out);
                }
            }
            Stmt::Expr(e)
            | Stmt::Defer(e)
            | Stmt::Set { value: e, .. }
            | Stmt::DerefSet { value: e, .. }
            | Stmt::FieldSet { value: e, .. }
            | Stmt::CompoundSet { value: e, .. }
            | Stmt::Return(Some(e)) => calls_in_expr(e, out),
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                calls_in_expr(condition, out);
                calls_in_statements(then_body, out);
                if let Some(other) = else_body {
                    calls_in_statements(other, out);
                }
            }
            Stmt::While { condition, body } => {
                calls_in_expr(condition, out);
                calls_in_statements(body, out);
            }
            Stmt::For {
                start, end, body, ..
            } => {
                calls_in_expr(start, out);
                calls_in_expr(end, out);
                calls_in_statements(body, out);
            }
            Stmt::ForEach { iterable, body, .. } => {
                calls_in_expr(iterable, out);
                calls_in_statements(body, out);
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => calls_in_statements(body, out),
            Stmt::Match { value, arms } => {
                calls_in_expr(value, out);
                for arm in arms {
                    calls_in_statements(&arm.body, out);
                }
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}
fn runtime_calls_and_declarations(
    items: &[Item],
    ns: &str,
    calls: &mut std::collections::BTreeSet<String>,
    declarations: &mut std::collections::BTreeSet<String>,
) {
    for item in items {
        match item {
            Item::Public(inner) => runtime_calls_and_declarations(
                std::slice::from_ref(inner.as_ref()),
                ns,
                calls,
                declarations,
            ),
            Item::Namespace { name, items } => {
                let qualified = if ns.is_empty() {
                    name.clone()
                } else {
                    format!("{ns}::{name}")
                };
                runtime_calls_and_declarations(items, &qualified, calls, declarations);
            }
            Item::Function(f) => {
                declarations.insert(if ns.is_empty() {
                    f.name.clone()
                } else {
                    format!("{ns}::{}", f.name)
                });
                calls_in_statements(&f.body, calls);
            }
            Item::Extern(f) => {
                declarations.insert(if ns.is_empty() {
                    f.name.clone()
                } else {
                    format!("{ns}::{}", f.name)
                });
            }
            Item::Impl(block) => {
                for method in &block.methods {
                    calls_in_statements(&method.body, calls);
                }
            }
            _ => {}
        }
    }
}
fn add_runtime_externs(items: &mut Vec<Item>) {
    let mut calls = std::collections::BTreeSet::new();
    let mut declarations = std::collections::BTreeSet::new();
    runtime_calls_and_declarations(items, "", &mut calls, &mut declarations);
    let mut by_module = std::collections::BTreeMap::<String, Vec<Item>>::new();
    for name in calls.difference(&declarations) {
        let Some((module, symbol, params, result)) = runtime_intrinsic(name) else {
            continue;
        };
        by_module
            .entry(module.into())
            .or_default()
            .push(Item::Extern(ast::ExternFunction {
                name: symbol.into(),
                c_variadic: false,
                attributes: vec![],
                params: params
                    .iter()
                    .map(|(name, ty)| ast::Param {
                        name: (*name).into(),
                        ty: TypeRef::Named((*ty).into()),
                        default: None,
                    })
                    .collect(),
                result: TypeRef::Named(result.into()),
            }));
    }
    if !by_module.is_empty() {
        items.push(Item::Namespace {
            name: "std".into(),
            items: by_module
                .into_iter()
                .map(|(name, items)| Item::Namespace { name, items })
                .collect(),
        });
    }
}
pub fn desugar(program: &ast::Program) -> Result<ast::Program, String> {
    let mut items = expand(program.items.clone())?;
    add_runtime_externs(&mut items);
    Ok(ast::Program { items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn parse(src: &str) -> ast::Program {
        grammar::ProgramParser::new().parse(src).unwrap()
    }
    #[test]
    fn synthesized_intrinsics_match_private_std_runtime_names() {
        // Every compiler-generated call must bind to the same Nova-facing `native_`
        // declaration exposed by std and exported by the C runtime.
        let expected = [
            (
                "std::coro::native_yield_value",
                "coro",
                "native_yield_value",
            ),
            ("std::coro::native_resume", "coro", "native_resume"),
            ("std::coro::native_drop", "coro", "native_drop"),
            ("std::generator::native_next", "generator", "native_next"),
            ("std::generator::native_done", "generator", "native_done"),
            (
                "std::task::native_await_value",
                "task",
                "native_await_value",
            ),
            ("std::task::native_block_on", "task", "native_block_on"),
            ("std::task::native_spawn", "task", "native_spawn"),
        ];
        for (qualified, module, symbol) in expected {
            let (actual_module, actual_symbol, _, _) = runtime_intrinsic(qualified)
                .unwrap_or_else(|| panic!("missing compiler intrinsic: {qualified}"));
            assert_eq!(actual_module, module);
            assert_eq!(actual_symbol, symbol);
        }
    }
    #[test]
    fn nested_yield_and_await_are_rewritten() {
        let p = desugar(&parse(
            "gen fn seq(limit:i64)->i64 { for i in 0..3 { yield i; } return 0; }",
        ))
        .unwrap();
        assert!(p.items.len() >= 3);
        assert!(p
            .items
            .iter()
            .any(|item| matches!(item,Item::Namespace{name,..} if name=="std")));
        let p = desugar(&parse(
            "async fn wait(h:u64)->i64 { let x:i64=await h; return x; }",
        ));
        let p = p.unwrap();
        assert!(p.items.len() >= 3);
        assert!(p
            .items
            .iter()
            .any(|item| matches!(item,Item::Namespace{name,..} if name=="std")));
    }
    #[test]
    fn builtin_runtime_symbols_work_without_std_import_and_are_not_duplicated() {
        let standalone = parse(
            r#"
            gen fn numbers(n:i64)->i64 { yield n; return 0; }
            fn main()->i32 {
                let handle:u64=numbers(12) defer dispose handle;
                let value:i64=next handle;
                let completed:bool=finished handle;
                return 0;
            }
        "#,
        );
        let lowered = crate::surface::lower(&standalone).expect("standalone coroutine intrinsics");
        let hir = crate::hir::lower(&lowered).expect("builtin coroutine ABI is typed");
        let mir = crate::mir::lower(&hir).expect("builtin coroutine ABI lowers to MIR");
        let llvm = crate::codegen::generate(&mir);
        assert!(llvm.contains("@__nova_std__coro____yield_value"), "{llvm}");
        assert!(llvm.contains("@__nova_std__generator____next"), "{llvm}");
        assert!(llvm.contains("@__nova_std__generator____done"), "{llvm}");
        assert!(llvm.contains("@__nova_std__coro____drop"), "{llvm}");

        let explicit = parse(
            r#"
            namespace std { namespace coro {
                extern fn native_yield_value(value:i64)->i64;
            }}
            gen fn numbers(n:i64)->i64 { yield n; return 0; }
        "#,
        );
        let expanded = desugar(&explicit).unwrap();
        let mut refs = std::collections::BTreeSet::new();
        let mut decls = std::collections::BTreeSet::new();
        runtime_calls_and_declarations(&expanded.items, "", &mut refs, &mut decls);
        assert_eq!(
            decls
                .iter()
                .filter(|s| s.as_str() == "std::coro::native_yield_value")
                .count(),
            1
        );
    }
    #[test]
    fn syntax_to_typed_mir_to_llvm_trampoline() {
        let src = r#"
            namespace std { namespace coro {
                extern fn native_yield_value(value: i64) -> i64;
            } }
            gen fn numbers(n:i64) -> i64 { yield n; return 0; }
            fn main() -> i32 { let c:u64=numbers(12); return 0; }
        "#;
        let ast = parse(src);
        let surface = crate::surface::lower(&ast).unwrap();
        let typed = crate::hir::lower(&surface).unwrap();
        let checked = crate::mir::lower(&typed).unwrap();
        let ir = crate::codegen::generate(&checked);
        assert!(ir.contains("define i64 @__nova_make_numbers"));
        assert!(ir.contains("define i64 @__nova_entry_numbers"));
        assert!(ir.contains("@__nova_coro_create(ptr @__nova_entry_numbers"));
        assert!(ir.contains("define i64 @__nova_body_numbers"));
    }
    #[test]
    fn suspension_outside_declared_mode_is_rejected() {
        let p = desugar(&parse("fn main()->i32 { yield 4; return 0; }"));
        assert!(p.unwrap_err().contains("yield"));
        let p = desugar(&parse("gen fn x()->i64 { let y=await 1; return 0; }"));
        assert!(p.unwrap_err().contains("await"));
    }
}
