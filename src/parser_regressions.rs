//! Regression coverage for LALRPOP conflicts observed in Nova 0.4.
//! Run with: cargo test parser_regressions -- --nocapture

use crate::{grammar, strip_comments};

fn parse_source(code: &str) -> crate::ast::Program {
    // The compiler strips comments before invoking the generated LALRPOP
    // parser. Integration tests must exercise that same entry path, instead
    // of passing raw .nova text containing // comments directly to LALRPOP.
    let source = strip_comments(code);
    grammar::ProgramParser::new()
        .parse(&source)
        .unwrap_or_else(|err| panic!("parser rejected a valid program: {err:?}"))
}

fn parses(name: &str, code: &str) {
    let source = strip_comments(code);
    grammar::ProgramParser::new()
        .parse(&source)
        .unwrap_or_else(|err| panic!("{name}: parser rejected a valid program: {err:?}"));
}

#[test]
fn integration_parser_ignores_comments_but_preserves_slashes_in_strings() {
    let code = r#"
        // Leading comment must not reach LALRPOP.
        fn main() -> i32 {
            auto url = "https://nova.dev/a//b"; // Trailing comment.
            return 0;
        }
    "#;
    parse_source(code);
}

#[test]
fn all_existing_examples_parse() {
    for (name, code) in [
        ("hello", include_str!("../examples/hello.nova")),
        ("arith", include_str!("../examples/arith.nova")),
        ("array", include_str!("../examples/array.nova")),
        ("pointers", include_str!("../examples/pointers.nova")),
        ("structs", include_str!("../examples/structs.nova")),
        ("generics", include_str!("../examples/generics.nova")),
        (
            "explicit-generics",
            include_str!("../examples/explicit-generics.nova"),
        ),
        ("option", include_str!("../examples/option.nova")),
    ] {
        parses(name, code);
    }
}

#[test]
fn shifts_are_distinct_from_comparisons() {
    parses(
        "shift and compare",
        r#"
        fn main() -> i32 {
            let a: i32 = 1 << 3;
            let b: i32 = a >> 1;
            if b < 5 && a >= 8 { return 0; }
            return 1;
        }
    "#,
    );
}

#[test]
fn attrs_namespaces_methods_and_type_arguments() {
    parses(
        "attributes + namespaces + methods",
        r#"
        namespace app {
            #[derive(Serialize, Clone, Debug)]
            struct Box<T> { value: T }
        }
        impl<T> app::Box<T> {
            fn get(self: app::Box<T>) -> T { return self.value; }
        }
        fn make<T>(value: T) -> app::Box<T> {
            return new app::Box<T> { value: value };
        }
        fn main() -> i32 {
            let value: app::Box<i32> = make::<i32>(42);
            return value.get() - 42;
        }
    "#,
    );
}

#[test]
fn generic_enum_and_match_arms() {
    parses(
        "option and match",
        r#"
        enum Option<T> { Some(T), None }
        fn main() -> i32 {
            let value: Option<i32> = Some(2);
            match value {
                Some(number) => { return number - 2; },
                None => { return 1; }
            }
        }
    "#,
    );
}

#[test]
fn nested_generics_require_separate_closing_tokens_for_now() {
    // Lexer currently recognizes >> as a shift token. Whitespace keeps
    // nested generic closing > tokens separate until a contextual lexer exists.
    parses(
        "nested generic closers",
        r#"
        struct Box<T> { value: T }
        fn main() -> i32 {
            let value: Box<Box<i32> > = new Box<Box<i32> > {
                value: new Box<i32> { value: 42 }
            };
            return value.value.value - 42;
        }
    "#,
    );
}

#[test]
fn no_trailing_comma_in_lists() {
    // Nova 0.4 deliberately excludes trailing commas in generic separated
    // lists to keep the LALRPOP grammar deterministic.
    let source = "struct P { x: i32, }";
    assert!(grammar::ProgramParser::new().parse(source).is_err());
}

#[test]
fn chained_else_if_desugars_into_nested_ifs() {
    use crate::ast::{Item, Stmt};
    let src = r#"fn main() -> i32 {
        let value: i32 = 17;
        if value == 0 { return 0; }
        else if value == 1 { return 1; }
        else if value > 5 { return 2; }
        else { return 3; }
    }"#;
    let parsed = grammar::ProgramParser::new().parse(src).unwrap();
    let Item::Function(f) = &parsed.items[0] else {
        panic!("missing function")
    };
    let Stmt::If {
        else_body: Some(branch),
        ..
    } = &f.body[1]
    else {
        panic!("missing if")
    };
    let [Stmt::If {
        else_body: Some(next),
        ..
    }] = branch.as_slice()
    else {
        panic!("missing second if")
    };
    let [Stmt::If {
        else_body: Some(final_branch),
        ..
    }] = next.as_slice()
    else {
        panic!("missing third if")
    };
    assert!(matches!(final_branch.as_slice(), [Stmt::Return(Some(_))]));
}

#[test]
fn attributes_before_pub_are_preserved_on_struct_and_enum() {
    use crate::ast::Item;
    let code = r#"
        #[derive(Serialize, Clone)]
        pub struct Sample { value: i64 }
        #[derive(Debug)]
        pub enum State { Ready, Busy(i32) }
        pub #[derive(Clone)] struct Legacy { value: i32 }
    "#;
    let program = grammar::ProgramParser::new()
        .parse(code)
        .expect("attributes + pub parse");
    assert_eq!(program.items.len(), 3);
    match &program.items[0] {
        Item::Public(inner) => match inner.as_ref() {
            Item::Struct(s) => {
                assert_eq!(s.name, "Sample");
                assert_eq!(s.attributes.len(), 1);
                assert_eq!(s.attributes[0].name, "derive");
                assert_eq!(s.attributes[0].args, vec!["Serialize", "Clone"]);
            }
            other => panic!("expected public struct, got {other:?}"),
        },
        other => panic!("expected visibility marker, got {other:?}"),
    }
    match &program.items[1] {
        Item::Public(inner) => match inner.as_ref() {
            Item::Enum(e) => {
                assert_eq!(e.name, "State");
                assert_eq!(e.attributes.len(), 1);
                assert_eq!(e.attributes[0].args, vec!["Debug"]);
            }
            other => panic!("expected public enum, got {other:?}"),
        },
        other => panic!("expected visibility marker, got {other:?}"),
    }
    match &program.items[2] {
        Item::Public(inner) => match inner.as_ref() {
            Item::Struct(s) => assert_eq!(s.attributes[0].args, vec!["Clone"]),
            other => panic!("expected public legacy struct, got {other:?}"),
        },
        other => panic!("expected visibility marker, got {other:?}"),
    }
}

#[test]
fn function_attributes_are_parsed_and_invalid_derives_are_rejected_semantically() {
    // Marker attributes on functions were introduced in Nova 0.15. The
    // obsolete parse-failure expectation must not reject supported syntax.
    let code = "#[derive(Debug)] pub fn f() -> i32 { return 0; }";
    let program = grammar::ProgramParser::new()
        .parse(code)
        .expect("function attributes parse");
    let crate::ast::Item::Public(inner) = &program.items[0] else {
        panic!("expected public function")
    };
    let crate::ast::Item::Function(function) = inner.as_ref() else {
        panic!("expected function")
    };
    let err = crate::ast::function_is_unsafe(&function.attributes, "f").unwrap_err();
    assert!(
        err.contains("unsupported function attribute 'derive'"),
        "{err}"
    );
}

#[test]
fn parses_auto_numeric_cast_and_user_macros() {
    parses(
        "auto/cast/macro",
        include_str!("../examples/advanced/ui_components.nova"),
    );
    parses(
        "numeric promotions",
        include_str!("../examples/advanced/numeric_casts.nova"),
    );
}

#[test]
fn cast_type_delimiters_do_not_conflict_with_comparisons_or_generics() {
    // Casts are postfix operations. The `as (Type)` spelling isolates
    // generic/pointer types from `<`, `>` and `>>` expression tokens.
    parses(
        "cast precedence and generic call arguments",
        r#"
        fn identity<T>(value: T) -> T { return value; }
        struct Pair<T> { value: T }
        macro wrap(v) => identity::<i64>(v);
        fn main() -> i32 {
            auto x = 3 as i64;
            auto y = wrap!(x as (i64));
            auto z = identity::<i64>(y as i64);
            let boxed: Pair<i64> = new Pair<i64> { value: z as i64 };
            if (boxed.value as i64) < 7 { return (z as i32) + 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn inference_cast_macro_pipeline_generates_llvm_casts() {
    let src = include_str!("../examples/advanced/ui_components.nova");
    let ast = parse_source(src);
    let lowered = crate::surface::lower(&ast).unwrap();
    let typed = crate::hir::lower(&lowered).unwrap();
    let mir = crate::mir::lower(&typed).unwrap();
    let ir = crate::codegen::generate(&mir);
    assert!(ir.contains("sext i32") || ir.contains("sitofp i32"));
    assert!(ir.contains("fptosi double") || ir.contains("fptoui double"));
    assert!(!ir.contains("component!"));
}

#[test]
fn numeric_widening_and_explicit_narrowing() {
    let src = include_str!("../examples/advanced/numeric_casts.nova");
    let ast = parse_source(src);
    let surface = crate::surface::lower(&ast).unwrap();
    let typed = crate::hir::lower(&surface).unwrap();
    let mir = crate::mir::lower(&typed).unwrap();
    let ir = crate::codegen::generate(&mir);
    assert!(ir.contains("sext i32") || ir.contains("fptosi double"));
}

#[test]
fn macro_cycles_and_bad_arity_fail_before_typecheck() {
    let src = "macro component(x) => component!(x); fn main()->i32 {return component!(3);}";
    let ast = grammar::ProgramParser::new().parse(src).unwrap();
    assert!(crate::surface::lower(&ast)
        .unwrap_err()
        .contains("recursive"));
    let src = "macro component(x) => x; fn main()->i32 {return component!(3,4);}";
    let ast = grammar::ProgramParser::new().parse(src).unwrap();
    assert!(crate::surface::lower(&ast).unwrap_err().contains("expects"));
}

#[test]
fn casts_do_not_implicitly_allow_unsafe_narrowing() {
    let src = "fn main()->i32 {let big:i64=100; let little:i32=big; return little;}";
    let ast = grammar::ProgramParser::new().parse(src).unwrap();
    assert!(crate::surface::lower(&ast).is_err());
    let src = "fn main()->i32 {auto big=100; return big as str;}";
    let ast = grammar::ProgramParser::new().parse(src).unwrap();
    assert!(crate::surface::lower(&ast).is_err());
}

#[test]
fn class_inline_defer_and_scope_unwind_lower_to_mir() {
    let src = include_str!("../examples/advanced/raii_defer.nova");
    let parsed = parse_source(src);
    let surface = crate::surface::lower(&parsed).unwrap();
    let h = crate::hir::lower(&surface).unwrap();
    let cleanup_count = h
        .functions
        .iter()
        .flat_map(|f| &f.body)
        .filter(|s| matches!(s, crate::hir::Stmt::Defer { .. }))
        .count();
    assert!(
        cleanup_count >= 1,
        "class RAII should register at least one destructor"
    );
    let m = crate::mir::lower(&h).unwrap();
    let proc = m.functions.iter().find(|f| f.name == "process").unwrap();
    // With RAII drop flags, cleanup calls live in guarded predecessor
    // blocks, not necessarily in the same block as Return.
    let cleanup_calls = proc
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                crate::mir::Instruction::Eval(crate::mir::Rvalue::Call(..))
            )
        })
        .count();
    let has_returns = proc
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, Some(crate::mir::Terminator::Return(..))));
    let has_guard = proc.blocks.iter().any(|block| {
        matches!(
            block.terminator,
            Some(crate::mir::Terminator::Branch { .. })
        )
    });
    assert!(
        cleanup_calls > 0 && has_returns && has_guard,
        "RAII cleanup calls must survive in guarded CFG blocks before returns"
    );
}

#[test]
fn typed_builder_and_string_parse() {
    parses(
        "standard library builder",
        include_str!("../std/src/builder.nova"),
    );
    parses(
        "standard library string",
        include_str!("../std/src/string.nova"),
    );
}

#[test]
fn owning_local_transfer_is_a_move_and_lowers_with_drop_flags() {
    let src = r#"
        class Owned { value:i32 }
        impl Owned { fn drop(self:Owned)->() {return;} }
        fn main()->i32 {
            auto first = new Owned {value:3};
            auto second = first;
            return 0;
        }
    "#;
    let ast = grammar::ProgramParser::new().parse(src).unwrap();
    let surface = crate::surface::lower(&ast).unwrap();
    let typed = crate::hir::lower(&surface).unwrap();
    let low = crate::mir::lower(&typed).unwrap();
    assert!(crate::codegen::generate(&low).contains("Owned__drop"));
}

#[test]
fn defer_runs_lifo_and_evaluates_return_before_cleanup() {
    use crate::mir::{Instruction, Operand, Rvalue, Terminator};
    let src = r#"
        extern fn tick(n: i32) -> ();
        fn main()->i32 {
            auto value = 42;
            defer tick(1);
            defer tick(2);
            return value;
        }
    "#;
    let ast = grammar::ProgramParser::new().parse(src).unwrap();
    let surface = crate::surface::lower(&ast).unwrap();
    let h = crate::hir::lower(&surface).unwrap();
    let m = crate::mir::lower(&h).unwrap();
    let f = m.functions.iter().find(|f| f.name == "main").unwrap();
    let instructions = &f.blocks[0].instructions;
    let calls = instructions
        .iter()
        .filter_map(|ins| match ins {
            Instruction::Eval(Rvalue::Call(_, args)) => match args.first() {
                Some(Operand::Int(i, _)) => Some(*i),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, vec![2, 1]);
    assert!(matches!(
        &f.blocks[0].terminator,
        Some(Terminator::Return(_))
    ));
    let snapshot = instructions
        .iter()
        .position(|ins| {
            matches!(
                ins,
                Instruction::Assign {
                    value: Rvalue::Use(Operand::Local(_)),
                    ..
                }
            )
        })
        .unwrap();
    let cleanup = instructions
        .iter()
        .position(|ins| matches!(ins, Instruction::Eval(Rvalue::Call(..))))
        .unwrap();
    assert!(
        snapshot < cleanup,
        "return value must be saved before running deferred actions"
    );
}

#[test]
fn template_class_inheritance_override_super_and_mir() {
    let src = include_str!("../examples/advanced/class_inheritance.nova");
    let ast = parse_source(src);
    let lowered = crate::surface::lower(&ast).unwrap();
    assert!(lowered.items.iter().any(|item|matches!(item,crate::ast::Item::Function(f) if f.name.starts_with("Derived__get__i32"))));
    assert!(lowered
        .items
        .iter()
        .any(|item| matches!(item,crate::ast::Item::Function(f) if f.name.contains("__super__"))));
    let typed = crate::hir::lower(&lowered).unwrap();
    let mir = crate::mir::lower(&typed).unwrap();
    let ir = crate::codegen::generate(&mir);
    assert!(ir.contains("%struct.Derived__i32"));
    assert!(
        ir.contains("Derived__super__get__i32")
            || ir.contains("Derived____super__get__i32")
            || ir.contains("__super__")
    );
}

#[test]
fn inherited_generic_methods_and_struct_templates() {
    let code = r#"
        struct Container<T> { content: T }
        impl<T> Container<T> {
            fn get(self: Container<T>) -> T { return self.content; }
        }
        struct Extended<T> : Container<T> { extra: i32 }
        fn main() -> i32 {
            auto obj = new Extended<i32> { content: 11, extra: 30 };
            return obj.get() + obj.extra - 41;
        }
    "#;
    let lowered = crate::surface::lower(&parse_source(code)).unwrap();
    crate::mir::lower(&crate::hir::lower(&lowered).unwrap()).unwrap();
}

#[test]
fn class_override_violations_and_cyclic_hierarchy() {
    for (code,expected) in [
        ("class A : B {} class B : A {} fn main()->i32{return 0;}","cyclic"),
        ("class A {} impl A {override fn go(self:A)->i32{return 0;}} fn main()->i32{return 0;}","override"),
        ("class A{} impl A{virtual fn n(self:A)->i32{return 1;}} class B:A{} impl B{fn n(self:B)->i32{return 2;}} fn main()->i32{return 0;}","override"),
        ("class A{x:i32} class B:A{x:i32} fn main()->i32 {return 0;}","duplicate inherited field"),
        ("class A<T>{x:T} class B:A<i32>{} impl B{override fn missing(self:B)->i32{return 1;}} fn main()->i32{return 0;}","no inherited method"),
    ] {
        let err=crate::surface::lower(&parse_source(code)).unwrap_err();
        assert!(err.contains(expected),"expected '{expected}' in '{err}'");
    }
}

#[test]
fn virtual_base_method_must_preserve_signature() {
    let source = r#"
        class A<T> { x: T }
        impl<T> A<T> { virtual fn read(self: A<T>, offset: i32) -> T {return self.x;} }
        class B<T> : A<T> { y: T }
        impl<T> B<T> { override fn read(self: B<T>, offset: i64) -> T {return self.y;} }
        fn main()->i32{return 0;}
    "#;
    assert!(crate::surface::lower(&parse_source(source))
        .unwrap_err()
        .contains("preserve"));
}

#[test]
fn runtime_polymorphism_via_tagged_enum_compiles() {
    let src = include_str!("../examples/advanced/polymorphism_tagged_enum.nova");
    let surface = crate::surface::lower(&parse_source(src)).unwrap();
    let typed = crate::hir::lower(&surface).unwrap();
    crate::mir::lower(&typed).unwrap();
}

#[test]
fn inheriting_drop_is_safe_but_replacing_it_is_rejected() {
    let ok = r#"
        class Resource { token:i32 }
        impl Resource {fn drop(self:Resource)->(){return;}}
        class Child : Resource { more:i32 }
        fn main()->i32 {
            auto child = new Child { token:1, more:2 };
            return child.more - 2;
        }
    "#;
    let compiled = crate::surface::lower(&parse_source(ok)).unwrap();
    let typed = crate::hir::lower(&compiled).unwrap();
    crate::mir::lower(&typed).unwrap();
    let bad = r#"
        class Resource { token:i32 }
        impl Resource {fn drop(self:Resource)->(){return;}}
        class Child : Resource { more:i32 }
        impl Child {fn drop(self:Child)->(){return;}}
        fn main()->i32{return 0;}
    "#;
    assert!(crate::surface::lower(&parse_source(bad))
        .unwrap_err()
        .contains("cannot replace inherited"));
}

#[test]
fn override_keeps_virtual_contract_for_grandchildren() {
    let source = r#"
        class First { x:i32 }
        impl First { virtual fn number(self:First)->i32 {return self.x;} }
        class Second : First { y:i32 }
        impl Second { override fn number(self:Second)->i32 {return self.y;} }
        class Third : Second { z:i32 }
        impl Third { override fn number(self:Third)->i32 {return self.z;} }
        fn main()->i32 {
            auto v=new Third { x:1, y:2, z:3 };
            return v.number() - 3;
        }
    "#;
    let lower = crate::surface::lower(&parse_source(source)).unwrap();
    let hir = crate::hir::lower(&lower).unwrap();
    crate::mir::lower(&hir).unwrap();
}

#[test]
fn implicit_numeric_widening_is_lossless_and_emitted_in_llvm() {
    let source = r#"
        fn identity(value: i64) -> i64 { return value; }
        fn promote(value: i32) -> f64 { return value; }
        fn main() -> i32 {
            let small: i32 = 17;
            let wide: i64 = small;
            let via_call: i64 = identity(small);
            let via_float: f64 = promote(small);
            let unsigned: u8 = 3;
            let signed: i16 = unsigned;
            if wide != 17 as i64 { return 1; }
            if via_call != 17 as i64 { return 2; }
            if via_float != 17.0 { return 3; }
            if signed != 3 as i16 { return 4; }
            return 0;
        }
    "#;
    let ast = parse_source(source);
    let surface = crate::surface::lower(&ast).expect("safe implicit widening must lower");
    let typed = crate::hir::lower(&surface).expect("safe implicit widening must typecheck");
    let mir = crate::mir::lower(&typed).expect("safe implicit widening must verify in MIR");
    let ir = crate::codegen::generate(&mir);
    assert!(ir.contains("sext i32") && ir.contains("sitofp i32"));
    assert!(ir.contains("zext i8"));
}

#[test]
fn implicit_numeric_narrowing_and_signedness_change_are_rejected() {
    for source in [
        "fn main() -> i32 { let wide: i64 = 10; let small: i32 = wide; return 0; }",
        "fn main() -> i32 { let negative: i32 = 1; let unsigned: u64 = negative; return 0; }",
        "fn main() -> i32 { let big: u64 = 1; let floating: f64 = big; return 0; }",
        "fn main() -> i32 { let floating: f64 = 1.0; let integer: i32 = floating; return 0; }",
    ] {
        let ast = parse_source(source);
        assert!(
            crate::surface::lower(&ast)
                .and_then(|s| crate::hir::lower(&s).map(|_| s))
                .is_err(),
            "lossy implicit cast was unexpectedly accepted: {source}"
        );
    }
}

#[test]
fn pointer_mutability_is_part_of_generic_type_unification() {
    let source = r#"
        fn same<T>(a: *const T, b: *mut T) -> i32 { return 0; }
        fn main() -> i32 {
            let mut value: i32 = 12;
            unsafe {
                let shared: *const i32 = &value;
                let another: *const i32 = &value;
                return same(shared, another);
            }
        }
    "#;
    let ast = parse_source(source);
    let err = crate::surface::lower(&ast).expect_err("*const must not unify with *mut");
    assert!(err.contains("mutability"), "unexpected diagnostic: {err}");
}

#[test]
fn typed_std_result_option_helpers_parse() {
    parses("std option", include_str!("../std/src/option.nova"));
    parses("std result", include_str!("../std/src/result.nova"));
    parses("std error", include_str!("../std/src/error.nova"));
}

#[test]
fn mir_verifier_rejects_mixed_width_binary_operands() {
    let ast = parse_source("fn main() -> i32 { return 0; }");
    let surface = crate::surface::lower(&ast).unwrap();
    let typed = crate::hir::lower(&surface).unwrap();
    let mut mir = crate::mir::lower(&typed).unwrap();
    let i32_ty = mir.types.i32();
    let i64_ty = mir.types.i64();
    let main = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
    let dst = main.locals.len();
    main.locals.push(i32_ty);
    main.blocks[0]
        .instructions
        .push(crate::mir::Instruction::Assign {
            dst,
            value: crate::mir::Rvalue::Binary(
                crate::ast::BinaryOp::Add,
                crate::mir::Operand::Int(1, i32_ty),
                crate::mir::Operand::Int(2, i64_ty),
            ),
        });
    let error = crate::mir::verify(&mir).expect_err("mixed-width LLVM arithmetic must be rejected");
    assert!(
        error.contains("mismatched binary operands"),
        "unexpected error: {error}"
    );
}

#[test]
fn mir_verifier_rejects_wrong_call_parameter_types() {
    let ast =
        parse_source("fn echo(value: i64) -> i64 { return value; } fn main() -> i32 { return 0; }");
    let surface = crate::surface::lower(&ast).unwrap();
    let typed = crate::hir::lower(&surface).unwrap();
    let mut mir = crate::mir::lower(&typed).unwrap();
    let i32_ty = mir.types.i32();
    let callee = mir.functions.iter().position(|f| f.name == "echo").unwrap();
    let main = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
    main.blocks[0]
        .instructions
        .push(crate::mir::Instruction::Eval(crate::mir::Rvalue::Call(
            callee,
            vec![crate::mir::Operand::Int(9, i32_ty)],
        )));
    let error =
        crate::mir::verify(&mir).expect_err("call mismatching the callee ABI must be rejected");
    assert!(error.contains("arg 1"), "unexpected error: {error}");
}

#[test]
fn mutable_field_assignment_reaches_typed_mir_and_llvm() {
    let source = r#"
        struct User { id: u64, active: bool }
        impl User {
            fn set(self: *mut User, value: u64) -> () {
                unsafe { self.id = value; }
                return;
            }
        }
        fn main() -> i32 {
            let mut user = new User { id: 42, active: true };
            user.id = 20;
            user.id += 2;
            unsafe {
                let pointer: *mut User = &mut user;
                pointer.set(77);
            }
            if user.id != 77 as u64 { return 1; }
            return 0;
        }
    "#;
    let parsed = parse_source(source);
    let surfaced = crate::surface::lower(&parsed).unwrap();
    let typed = crate::hir::lower(&surfaced).unwrap();
    let mir = crate::mir::lower(&typed).unwrap();
    let n = mir
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|bb| bb.instructions.iter())
        .filter(|ins| matches!(ins, crate::mir::Instruction::StoreField { .. }))
        .count();
    assert!(n >= 3, "expected writes to local and pointer-backed fields");
    let llvm = crate::codegen::generate(&mir);
    assert!(llvm.contains("getelementptr inbounds %struct.User"));
}

#[test]
fn immutable_field_and_unsafe_pointer_writes_are_rejected() {
    fn fails(source: &str, part: &str) {
        let parsed = parse_source(source);
        // The surface pass can reject an invalid write early (for example,
        // a field write through `*const`). Other mutability/unsafe errors are
        // caught by typed HIR. Both are valid compile-time diagnostics.
        let error = match crate::surface::lower(&parsed) {
            Err(error) => error,
            Ok(surface) => {
                crate::hir::lower(&surface).expect_err("invalid field assignment must be rejected")
            }
        };
        assert!(error.contains(part), "expected {part:?}, got {error:?}");
    }
    fails(
        r#"
        struct User { id: u64 }
        fn main() -> i32 {
            let user = new User { id: 1 };
            user.id = 2;
            return 0;
        }
    "#,
        "immutable local",
    );
    fails(
        r#"
        struct User { id: u64 }
        fn edit(ptr: *mut User) -> () {
            ptr.id = 42;
            return;
        }
        fn main() -> i32 { return 0; }
    "#,
        "unsafe block",
    );
    fails(
        r#"
        struct User { id: u64 }
        fn edit(ptr: *const User) -> () {
            unsafe { ptr.id = 42; }
            return;
        }
        fn main() -> i32 { return 0; }
    "#,
        "const pointer",
    );
}

#[test]
fn field_assignment_disambiguates_expression_prefixes() {
    // Regression: the dedicated Ident "." Ident prefix conflicted with
    // PostfixExpr on `.`, breaking the LALRPOP parser generation.
    let source = r#"
        struct User { id: u64 }
        impl User {
            fn get(self: User) -> u64 { return self.id; }
            fn set(self: *mut User, value: u64) -> () {
                unsafe { self.id = value; }
                return;
            }
        }
        fn main() -> i32 {
            let mut user = new User { id: 2 };
            user.id = 3;
            user.id += 4;
            user.id -= 1;
            user.id *= 2;
            user.id /= 2;
            user.id %= 5;
            if user.id < 3 { return 1; }
            user.get();
            let value = user.id;
            value += 1;
            return 0;
        }
    "#;
    let parsed = parse_source(source);
    let lowered = crate::surface::lower(&parsed).expect("valid assignments should lower");
    assert!(!lowered.items.is_empty());
}

#[test]
fn unsupported_assignment_target_gets_clear_error() {
    let source = r#"
        struct U { id: u64 }
        fn main() -> i32 {
            let mut a = new U { id: 1 };
            let mut b = new U { id: 2 };
            (a.id).id = b.id;
            return 0;
        }
    "#;
    let parsed = parse_source(source);
    let error = crate::surface::lower(&parsed).expect_err("nested target unsupported");
    assert!(
        error.contains("invalid assignment target"),
        "unexpected error: {error}"
    );
}

#[test]
fn derived_json_class_and_instance_syntax_reaches_llvm() {
    let parsed = parse_source(include_str!("../examples/advanced/json_methods_096.nova"));
    let surface = crate::surface::lower(&parsed).expect("JSON class methods must resolve");
    let typed = crate::hir::lower(&surface).expect("derived methods must be typed");
    let ir = crate::mir::lower(&typed).expect("derived methods must lower");
    crate::mir::verify(&ir).expect("derived methods must pass MIR verification");
    let llvm = crate::codegen::generate(&ir);
    assert!(
        llvm.contains("@User_from_json"),
        "missing generated class decoder"
    );
    assert!(
        llvm.contains("@User_to_json"),
        "missing generated class encoder"
    );
    assert!(
        llvm.contains("@__nova_json_append_Address"),
        "nested encoder missing"
    );
    assert!(
        llvm.contains("@__nova_json_builder_finish"),
        "encoder did not return str"
    );
}

#[test]
fn json_class_methods_require_correct_derives() {
    for (source, expected) in [
        (
            r#"
            struct U { id: i32 }
            fn main() -> i32 { let u = U::fromJson("{\"id\":1}"); return 0; }
        "#,
            "derive Deserialize",
        ),
        (
            r#"
            #[derive(Deserialize)] struct U { id: i32 }
            fn main() -> i32 {
                let u = U::fromJson("{\"id\":1}");
                u.toJson();
                return 0;
            }
        "#,
            "derive Serialize",
        ),
        (
            r#"
            #[derive(Serialize)] struct U { id: i32 }
            fn main() -> i32 {
                let u = new U { id: 1 };
                U::fromJson("{\"id\":1}");
                return 0;
            }
        "#,
            "derive Deserialize",
        ),
    ] {
        let parsed = parse_source(source);
        let err = crate::surface::lower(&parsed).expect_err("missing derive must reject method");
        assert!(err.contains(expected), "expected {expected:?}, got {err:?}");
    }
}

#[test]
fn json_instance_to_json_requires_zero_explicit_arguments() {
    let parsed = parse_source(
        r#"
        #[derive(Serialize)] struct U { id: i32 }
        fn main() -> i32 {
            let u = new U { id: 1 };
            u.toJson(42);
            return 0;
        }
    "#,
    );
    let err = crate::surface::lower(&parsed).expect_err("toJson argument must be rejected");
    assert!(
        err.contains("takes no arguments"),
        "unexpected error: {err}"
    );
}

// The `use` keyword is also the conventional Nexo middleware method.
// Exercise both forms in the same file to prevent a future lexer regression.
#[test]
fn nexo_use_method_and_module_import_both_parse() {
    parses(
        "contextual use keyword for Nexo",
        r#"
        use utils::middleware;
        struct App { port: i32 }
        impl App {
            fn use(self: App, enabled: bool) -> i32 {
                if enabled { return 0; }
                return 1;
            }
        }
        fn main() -> i32 {
            let app: App = new App { port: 7888 };
            return app.use(true);
        }
    "#,
    );
}

#[test]
fn nexo_generators_async_and_field_mutation_parse() {
    for (module, source) in [
        ("nexo root", include_str!("../packages/nexo/src/lib.nova")),
        (
            "nexo request",
            include_str!("../packages/nexo/src/request.nova"),
        ),
        (
            "nexo response",
            include_str!("../packages/nexo/src/response.nova"),
        ),
        (
            "nexo methods",
            include_str!("../packages/nexo/src/method.nova"),
        ),
        ("nexo paths", include_str!("../packages/nexo/src/path.nova")),
        (
            "nexo query",
            include_str!("../packages/nexo/src/query.nova"),
        ),
        (
            "nexo headers",
            include_str!("../packages/nexo/src/headers.nova"),
        ),
        (
            "nexo routing",
            include_str!("../packages/nexo/src/routing.nova"),
        ),
        (
            "nexo middleware",
            include_str!("../packages/nexo/src/middleware.nova"),
        ),
        ("nexo app", include_str!("../packages/nexo/src/app.nova")),
        (
            "nexo server",
            include_str!("../packages/nexo/src/server.nova"),
        ),
        (
            "nexo connection",
            include_str!("../packages/nexo/src/connection.nova"),
        ),
        (
            "nexo config",
            include_str!("../packages/nexo/src/config.nova"),
        ),
    ] {
        parses(module, source);
    }
    parses(
        "lambda callback example",
        include_str!("../examples/packages/lambda_callbacks/src/main.nova"),
    );
    parses(
        "nexo bounded demo",
        include_str!("../examples/packages/nexo_async_counter/src/main.nova"),
    );
    parses(
        "nexo long-running demo",
        include_str!("../examples/packages/nexo_async_live/src/main.nova"),
    );
    parses(
        "nexo modular smoke",
        include_str!("../examples/packages/nexo_modular/src/main.nova"),
    );
}

#[test]
fn clean_stdlib_size_of_is_abi_aligned() {
    let source = r#"
        struct Record { enabled: bool, callback: fn(i64)->i64, count: i64 }
        fn main() -> i32 {
            let bytes: u64 = std::raw::size_of::<Record>();
            let alignment: u64 = std::raw::align_of::<Record>();
            if bytes != 24 || alignment != 8 { return 1; }
            return 0;
        }
    "#;
    let parsed = parse_source(source);
    let lowered = crate::surface::lower(&parsed).expect("layout intrinsic must resolve");
    let typed = crate::hir::lower(&lowered).expect("layout must be typed as u64");
    let ir = crate::mir::lower(&typed).expect("layout must lower to constants");
    crate::mir::verify(&ir).expect("layout must verify");
}

#[test]
fn clean_stdlib_pointer_casts_require_unsafe() {
    let source = r#"
        struct Entry { count: i64 }
        fn main() -> i32 {
            let mut entry: Entry = new Entry { count: 42 };
            unsafe {
                let pointer: *mut Entry = &mut entry;
                let erased: *mut u8 = pointer as (*mut u8);
                let recovered: *mut Entry = erased as (*mut Entry);
                if recovered.count != 42 { return 1; }
            }
            return 0;
        }
    "#;
    let parsed = parse_source(source);
    let lowered = crate::surface::lower(&parsed).expect("unsafe pointer casts must lower");
    let typed = crate::hir::lower(&lowered).expect("unsafe casts must typecheck");
    let ir = crate::mir::lower(&typed).expect("pointer casts must lower");
    crate::mir::verify(&ir).expect("pointer casts must verify");
}

#[test]
fn clean_stdlib_nexo_stores_typed_callbacks() {
    let routes = include_str!("../packages/nexo/src/routing.nova");
    let middleware = include_str!("../packages/nexo/src/middleware.nova");
    let source = format!("{routes}\n{middleware}");
    assert!(routes.contains("callback: fn(Request, Response) -> i32"));
    assert!(middleware.contains("callback: fn(Request, Response) -> bool"));
    assert!(!source.contains("callback as u64"));
    assert!(!source.contains("callback as i64"));
    assert!(!source.contains("cb as u64"));
    assert!(!source.contains("fn_id"));
}

#[test]
fn nova_012_native_generator_control_keywords_parse() {
    parses(
        "generator lifecycle keywords",
        r#"
        gen fn counter(value:i64)->i64 {yield value;return 0;}
        fn main()->i32 {
            let stream:u64=counter(5) defer dispose stream;
            if !(finished stream) {
                let current:i64=next stream;
            }
            return 0;
        }
    "#,
    );
    parses(
        "coroutine resume and async keywords",
        r#"
        fn run(handle:u64)->i64 {
            let first:i64=resume handle;
            let second:i64=resume(handle,15);
            let third:i64=join handle;
            spawn handle;
            return first+second+third;
        }
    "#,
    );
}

#[test]
fn nova_012_macro_ast_fragments_and_foreach_parse() {
    parses(
        "typed macro and native foreach",
        r#"
        macro twice($value:expr) => $value + $value;
        macro guard($condition:expr) => { if !$condition {return 1;} }
        macro define($name:ident, $value:literal) => {let $name=$value;}
        fn main()->i32 {
            for number in numbers {
                guard!(number > 0);
                define!(current, 10);
            }
            return twice!(0);
        }
    "#,
    );
}

#[test]
fn borrowed_methods_can_be_called_repeatedly_without_moving_an_owner() {
    let src = r#"
        struct Resource { handle: u64 }
        impl Resource {
            fn value(self: &Resource) -> u64 { return self.handle; }
            fn drop(self: Resource) -> () { return; }
        }
        fn main() -> i32 {
            let item = new Resource { handle: 42 };
            if item.value() != 42 { return 1; }
            if item.value() != 42 { return 2; }
            return 0;
        }
    "#;
    let ast = parse_source(src);
    let lowered = crate::surface::lower(&ast).expect("auto-borrow receiver must lower");
    let typed = crate::hir::lower(&lowered).expect("typed borrow must pass ownership checker");
    let mir = crate::mir::lower(&typed).expect("owned local must lower with a drop flag");
    crate::mir::verify(&mir).expect("borrowed methods must preserve valid MIR");
    let llvm = crate::codegen::generate(&mir);
    assert!(llvm.contains("Resource__drop"));
}

#[test]
fn coroutine_keyword_next_remains_a_legal_struct_field_name() {
    let src = r#"
        struct Node { next: *mut Node, count: i64 }
        fn main() -> i32 {
            let mut head: Node = new Node {next: unsafe_null_pointer(), count: 1};
            return 0;
        }
    "#;
    // Syntax-only: the name must be legal in a field declaration and init.
    let parsed = grammar::ProgramParser::new().parse(src);
    assert!(parsed.is_ok(), "{:?}", parsed.err());
}

#[test]
fn typed_async_template_stdlib_is_parseable() {
    for (name, source) in [
        ("std::task", include_str!("../std/src/task.nova")),
        ("std::generator", include_str!("../std/src/generator.nova")),
        ("std::coro", include_str!("../std/src/coro.nova")),
    ] {
        parses(name, source);
    }
}

#[test]
fn typed_async_bootstrap_example_parses() {
    parses(
        "typed async example",
        include_str!("../examples/packages/typed_async_016/src/main.nova"),
    );
}

#[test]
fn async_attribute_parses_then_checks_declared_mode() {
    let code = "[[async]] fn compute()->i64{return 0;}";
    let program = grammar::ProgramParser::new()
        .parse(code)
        .expect("async marker should parse");
    let error = crate::effects::validate(&program).unwrap_err();
    assert!(
        error.contains("requires an async fn declaration"),
        "{error}"
    );
    let valid = "[[async]] async fn compute()->i64{return 0;}";
    let program = grammar::ProgramParser::new()
        .parse(valid)
        .expect("async mode plus async marker should parse");
    crate::effects::validate(&program).expect("async marker matches declared mode");
}
