use crate::{ast::{self, Expr, Item, Stmt}, entrypoint, grammar};

fn program(src: &str) -> ast::Program {
    grammar::ProgramParser::new().parse(src).expect("valid Nova syntax")
}
fn select(src: &str, path: &str) -> Result<ast::Program, String> {
    entrypoint::generate(&program(src), path)
}
fn generated_entry(program: &ast::Program) -> &ast::Function {
    match program.items.last().unwrap() {
        Item::Function(f) if f.name == "main" => f,
        other => panic!("expected generated global main, got {other:?}"),
    }
}

#[test]
fn package_selected_main_is_forwarded_as_a_normal_call() {
    let expanded = select(r#"
        class DB {}
        impl DB {
            static fn main() -> i32 { return 0; }
        }
    "#, "DB::main").unwrap();
    let entry = generated_entry(&expanded);
    assert!(entry.params.is_empty());
    assert!(matches!(&entry.body[0], Stmt::Return(Some(Expr::Call(name, args))) if name == "DB::main" && args.is_empty()));
}

#[test]
fn static_main_is_not_an_entry_point_without_package_selection() {
    let original = program(r#"
        class DB {}
        impl DB { static fn main() -> i32 { return 0; } }
    "#);
    // The language treats static methods, and NB annotations, as ordinary code.
    let lowered = crate::surface::lower(&original).expect("static method is valid by itself");
    assert!(!lowered.items.iter().any(|item| matches!(item, Item::Function(f) if f.name == "main")));
}

#[test]
fn old_global_main_still_works_without_package_selection() {
    let original = program("fn main() -> i32 { return 0; }");
    let lowered = crate::surface::lower(&original).unwrap();
    assert!(lowered.items.iter().any(|item| matches!(item, Item::Function(f) if f.name == "main")));
}

#[test]
fn annotations_do_not_choose_the_entry_point() {
    let selected = select(r#"
        @NB::BootApplication class First {}
        impl First { static fn main() -> i32 {return 0;} }
        @NB::BootApplication class Second {}
        impl Second { static fn main() -> i32 {return 0;} }
    "#, "Second::main").unwrap();
    assert!(matches!(&generated_entry(&selected).body[0], Stmt::Return(Some(Expr::Call(name,_))) if name == "Second::main"));
}

#[test]
fn selected_main_and_global_main_cannot_coexist() {
    let error = select(r#"
        class DB {}
        impl DB { static fn main() -> i32 {return 0;} }
        fn main() -> i32 {return 1;}
    "#, "DB::main").unwrap_err();
    assert!(error.contains("NOVA-ENTRY003"),"{error}");
}

#[test]
fn missing_or_nonstatic_entry_reports_error() {
    let missing = select("class DB {}", "DB::main").unwrap_err();
    assert!(missing.contains("NOVA-ENTRY004"),"{missing}");
    let not_static = select(r#"
        class DB {}
        impl DB { fn main(self: &Self) -> i32 {return 0;} }
    "#, "DB::main").unwrap_err();
    assert!(not_static.contains("NOVA-ENTRY005"),"{not_static}");
    let invalid = select("class DB {}", "main").unwrap_err();
    assert!(invalid.contains("NOVA-ENTRY001"),"{invalid}");
}

#[test]
fn selected_entry_method_requires_i32_no_parameters() {
    let wrong_result = select(r#"
        class DB {}
        impl DB { static fn main() -> i64 {return 0;} }
    "#, "DB::main").unwrap_err();
    assert!(wrong_result.contains("NOVA-ENTRY006"),"{wrong_result}");
    let wrong_arity = select(r#"
        class DB {}
        impl DB { static fn main(port: i32) -> i32 {return port;} }
    "#, "DB::main").unwrap_err();
    assert!(wrong_arity.contains("NOVA-ENTRY007"),"{wrong_arity}");
}

#[test]
fn selected_entry_can_have_an_arbitrary_static_name_and_namespace() {
    let selected = select(r#"
        namespace app {
            class Server {}
            impl Server { static fn start() -> i32 {return 0;} }
        }
    "#, "app::Server::start").unwrap();
    assert!(matches!(&generated_entry(&selected).body[0], Stmt::Return(Some(Expr::Call(name,_))) if name == "app::Server::start"));
}

#[test]
fn manifest_selects_unannotated_static_method() {
    let root = std::env::temp_dir().join(format!("nova_explicit_main_{}_{}", std::process::id(), "unit"));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Nova.toml"),
        "[package]\nname = \"plain_app\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\nmain = \"App::start\"\n").unwrap();
    std::fs::write(root.join("src/main.nova"),
        "class App {} impl App { static fn start() -> i32 {return 0;} }\n").unwrap();
    let result = (|| -> Result<(), String> {
        let parsed = crate::package::load(&root)?;
        if !matches!(&generated_entry(&parsed).body[0], Stmt::Return(Some(Expr::Call(name,_))) if name == "App::start") {
            return Err("package selected an incorrect entry point".into());
        }
        let lowered = crate::surface::lower(&parsed)?;
        let high = crate::hir::lower(&lowered)?;
        super::require_main(&high)
    })();
    let _ = std::fs::remove_dir_all(&root);
    result.expect("Nova.toml must select plain static method without NB");
}

#[test]
fn nb_demo_package_chooses_main_explicitly() {
    let parsed = crate::package::load(std::path::Path::new("examples/packages/nb_demo"))
        .expect("load Nova Boot example");
    assert!(matches!(&generated_entry(&parsed).body[0], Stmt::Return(Some(Expr::Call(name,_))) if name=="DB::main"));
    let lowered = crate::surface::lower(&parsed).expect("compile package-selected class entry");
    let high = crate::hir::lower(&lowered).expect("class main must typecheck");
    super::require_main(&high).expect("package-generated global main has executable ABI");
}

#[test]
fn nb_core_check_emits_executable_main_from_package_config() {
    let parsed = crate::package::load(std::path::Path::new("examples/packages/nb_core_check"))
        .expect("load class entry example");
    let lowered = crate::surface::lower(&parsed).expect("lower class main");
    let high = crate::hir::lower(&lowered).expect("HIR class main");
    let mir = crate::mir::lower(&high).expect("MIR class main");
    let llvm = crate::codegen::generate(&mir);
    assert!(llvm.contains("define i32 @main("),"{llvm}");
}

#[test]
fn boot_application_deferred_main_is_forwarded_before_proc_expansion() {
    // Package loading must not demand a method that @BootApplication only
    // generates during subsequent procedural macro expansion.
    for annotation in ["@BootApplication", "@NB::BootApplication"] {
        let source = format!("{annotation} class DB {{}}\n");
        let parsed = select(&source, "DB::main")
            .expect("an annotated application may generate its entry later");
        assert!(matches!(&generated_entry(&parsed).body[0],
            Stmt::Return(Some(Expr::Call(name, args)))
            if name == "DB::main" && args.is_empty()));
    }
}

#[test]
fn deferred_entry_requires_boot_application_and_selected_main() {
    for source in [
        "class DB {}",
        "@Service class DB {}",
        "@Other::BootApplication class DB {}",
        "@BootApplication struct DB {}",
    ] {
        let err = select(source, "DB::main").unwrap_err();
        assert!(err.contains("NOVA-ENTRY004"), "{source}: {err}");
    }
    let err = select("@BootApplication class DB {}", "DB::start").unwrap_err();
    assert!(err.contains("NOVA-ENTRY004"), "{err}");
}

#[test]
fn nb_backend_package_loads_generated_boot_entry() {
    let program = crate::package::load(std::path::Path::new("examples/packages/nb_backend"))
        .expect("the entry bridge must accept a main generated by @BootApplication");
    assert!(matches!(&generated_entry(&program).body[0],
        Stmt::Return(Some(Expr::Call(name, _))) if name == "DB::main"));
    let expanded = crate::proc_macros::expand(&program)
        .expect("@BootApplication must generate the actual entry method");
    assert!(expanded.items.iter().any(|item| match item {
        Item::Impl(block) => matches!(&block.target, ast::TypeRef::Named(owner) if owner == "DB")
            && block.methods.iter().any(|method| method.name == "main"
                && method.attributes.iter().any(|attr| attr.name == "static")),
        _ => false,
    }), "generated DB::main should exist after procedural expansion");
}
