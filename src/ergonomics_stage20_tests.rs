//! Regression tests for refutable local bindings compiled to ordinary enum matches.
use crate::{ast, codegen, grammar, hir, mir, strip_comments, surface};

fn compile(source: &str) -> Result<String, String> {
    let parsed = grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .map_err(|error| format!("parse: {error:?}"))?;
    let typed = surface::lower(&parsed)?;
    let hir = hir::lower(&typed)?;
    let mir = mir::lower(&hir)?;
    Ok(codegen::generate(&mir))
}

#[test]
fn let_else_binds_enum_payload_in_remainder_of_block() {
    let result = compile(r#"
        enum Maybe { Some(i32), None }
        fn main() -> i32 {
            let opt: Maybe = Some(42);
            let Some(value) = opt else { return 1; };
            return value - 42;
        }
    "#).unwrap();
    assert!(result.contains("@main"));
}

#[test]
fn multiple_let_else_bindings_compose_in_same_scope() {
    let result = compile(r#"
        enum Maybe { Some(i32), None }
        fn main() -> i32 {
            let a: Maybe = Some(20);
            let b: Maybe = Some(22);
            let Some(left) = a else { return 1; };
            let Some(right) = b else { return 2; };
            return left + right - 42;
        }
    "#).unwrap();
    assert!(result.contains("@main"));
}

#[test]
fn works_with_other_variants_not_only_some() {
    let result = compile(r#"
        enum Outcome { Ok(i32), Err(i32) }
        fn main() -> i32 {
            let answer: Outcome = Ok(42);
            let Ok(value) = answer else { return 1; };
            return value - 42;
        }
    "#).unwrap();
    assert!(result.contains("@main"));
}

#[test]
fn missing_early_exit_rejected_before_uninitialized_use() {
    let error = compile(r#"
        enum Maybe { Some(i32), None }
        fn main() -> i32 {
            let opt: Maybe = None;
            let Some(value) = opt else { let ignored = 0; };
            return value;
        }
    "#).unwrap_err();
    assert!(error.contains("let-else requires its else branch"), "{error}");
}

#[test]
fn let_else_evaluates_subject_once_in_ast() {
    let parsed = grammar::ProgramParser::new().parse(r#"
        enum Maybe { Some(i32), None }
        fn make() -> Maybe { return Some(42); }
        fn main() -> i32 {
            let Some(value) = make() else { return 1; };
            return value - 42;
        }
    "#).unwrap();
    let main = parsed.items.iter().find_map(|item| match item {
        ast::Item::Function(function) if function.name == "main" => Some(function),
        _ => None,
    }).unwrap();
    let ast::Stmt::Match { value, arms } = &main.body[0] else { panic!("expected match lowering") };
    assert!(matches!(value, ast::Expr::Call(name, args) if name == "make" && args.is_empty()));
    assert!(matches!(&arms[0].pattern, ast::Pattern::Variant { path, bindings }
        if path == "Some" && bindings.len() == 1 && bindings[0] == "value"));
    assert!(matches!(arms[0].body.last(), Some(ast::Stmt::Return(_))));
}
