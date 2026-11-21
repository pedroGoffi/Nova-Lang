//! NB executable HTTP/DI modules: package-loading and compiler integration
//! tests. These require cargo test to execute; no network server is started.
use crate::{ast::{Item,Function}, package, proc_macros, meta};

fn methods<'a>(items:&'a [Item], target:&str)->Vec<&'a Function> {
    fn walk<'a>(items:&'a [Item],wanted:&str, methods:&mut Vec<&'a Function>) {
        for item in items {
            match item {
                Item::Namespace{items,..}=>walk(items,wanted,methods),
                Item::Public(inner)=>walk(std::slice::from_ref(inner.as_ref()),wanted,methods),
                Item::Impl(block) if matches!(&block.target,crate::ast::TypeRef::Named(name) if name==wanted || name.rsplit("::").next()==Some(wanted))=>{
                    methods.extend(block.methods.iter());
                }
                _=>{}
            }
        }
    }
    let mut found=vec![];
    walk(items,target,&mut found);
    found
}

#[test]
fn nb_backend_loads_with_use_glob_and_new_modules() {
    let parsed=package::load(std::path::Path::new("examples/packages/nb_backend"))
        .expect("NB backend demo must load alongside nova_http and std");
    let expanded=proc_macros::expand(&parsed)
        .expect("NB macros and attributes must expand");
    meta::validate(&expanded).expect("all NB metadata must be declared");
    let controller=methods(&expanded.items,"UserController");
    for method in ["nb_route_count","nb_path_find","nb_verb_find","nb_path_create","nb_verb_create"] {
        assert!(controller.iter().any(|m|m.name==method),"missing {method}");
    }
    let services=methods(&expanded.items,"UserService");
    assert!(services.iter().any(|m|m.name=="nb_scope"));
    let beans=methods(&expanded.items,"BackendBeans");
    assert!(beans.iter().any(|m|m.name=="nb_bean_count"));
    assert!(beans.iter().any(|m|m.name=="nb_bean_factory_service"));
}

#[test]
fn nb_backend_typechecks_with_runtime_http_adapters() {
    let parsed=package::load(std::path::Path::new("examples/packages/nb_backend"))
        .expect("load NB backend example");
    // The embedded STD + nova_http + NB integration fixture has a deep, finite
    // monomorphization workload. A Rust test-harness worker can have a
    // smaller stack than the compiler's main thread, making this test
    // susceptible to intermittent stack overflow. Run only this
    // large fixture on a dedicated stack; preserve the full typecheck and
    // propagate both panics and compiler diagnostics to the test runner.
    // Runtime registration is created by @BootApplication during procedural
    // expansion. Checking only the loaded program used to miss invalid types
    // inside the generated nb_register_controllers method.
    let expanded=proc_macros::expand(&parsed)
        .expect("expand NB controller autodiscovery before typechecking");
    std::thread::Builder::new()
        .name("nova-nb-backend-typecheck".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let lowered = crate::surface::lower(&expanded)?;
            crate::hir::lower(&lowered).map(|_| ())
        })
        .expect("spawn NB integration typechecker")
        .join()
        .expect("NB integration typechecker panicked")
        .expect("NB HTTP request/response/beans must pass Surface and typed HIR");
}

#[test]
fn nb_local_static_method_resolves_from_another_impl() {
    // Regression: RouteTemplate::valid in NB::web::Group::put used to
    // produce NOVA-T004 because `qualify` treated every `::` as absolute.
    let source = r#"
        namespace NB {
            namespace web {
                pub struct RouteTemplate {}
                impl RouteTemplate {
                    static fn valid(path: str) -> bool { return path != ""; }
                }
                pub struct Group {}
                impl Group {
                    static fn put(path: str) -> i32 {
                        if !RouteTemplate::valid(path) { return -1; }
                        return 0;
                    }
                }
            }
        }
        fn main() -> i32 { return NB::web::Group::put("/api"); }
    "#;
    let parsed = crate::grammar::ProgramParser::new()
        .parse(source)
        .expect("parse sibling static method invocation");
    crate::surface::lower(&parsed)
        .expect("resolve RouteTemplate::valid relative to NB::web");
}

#[test]
fn nb_static_method_qualification_preserves_global_and_parent_scope() {
    let source = r#"
        namespace pkg {
            pub struct Validator {}
            impl Validator { static fn valid() -> i32 { return 42; } }
            namespace child {
                pub struct Group {}
                impl Group {
                    static fn check() -> i32 { return Validator::valid(); }
                }
            }
        }
        fn main() -> i32 {
            return pkg::child::Group::check() - pkg::Validator::valid();
        }
    "#;
    let parsed = crate::grammar::ProgramParser::new()
        .parse(source)
        .expect("parse parent and fully qualified static method calls");
    crate::surface::lower(&parsed)
        .expect("resolve parent-relative and absolute static method calls");
}

#[test]
fn nb_postgres_is_optional_and_has_independent_manifest() {
    let root=std::path::Path::new("packages/NB");
    let content=std::fs::read_to_string(root.join("Nova.toml")).unwrap();
    assert!(!content.contains("nova_postgres"),"core NB must not force native pg deps");
    let postgres=std::fs::read_to_string("packages/NBPostgres/Nova.toml").unwrap();
    assert!(postgres.contains("nova_postgres"));
}

#[test]
fn nb_http_dependency_is_consolidated_and_paths_use_shared_mux() {
    let manifest=std::fs::read_to_string("packages/NB/Nova.toml").unwrap();
    assert!(manifest.contains("nova_http ="));
    assert!(!manifest.contains("nexo =") && !manifest.contains("nexoframe"));
    let backend=std::fs::read_to_string("examples/packages/nb_backend/Nova.toml").unwrap();
    assert!(!backend.contains("nexo =") && !backend.contains("nova_http =") && !backend.contains("nexoframe"));
    let example=std::fs::read_to_string("examples/packages/nb_backend/src/user_controller.nova").unwrap();
    assert!(example.contains("@GetMapping(\"/{id}\")"));
    assert!(example.contains("use NB::http::Request;"));
    assert!(example.contains("res:&Response,req:&Request"));
}

#[test]
fn nb_core_smoke_package_typechecks() {
    let parsed=package::load(std::path::Path::new("examples/packages/nb_core_check"))
        .expect("load NB deterministic test application");
    crate::surface::lower(&parsed).expect("NB generics, routes and beans should typecheck");
}

#[test]
fn nb_annotation_before_static_method_is_valid_syntax() {
    let source=r#"
        @RestController(path="/api") class Demo {}
        impl Demo {
            @GetMapping("/{id}") static fn get(req:i64)->i64{return req;}
            static @PostMapping("/") fn create(req:i64)->i64{return req;}
        }
    "#;
    crate::grammar::ProgramParser::new().parse(source)
        .expect("both positions of a method annotation should parse");
}

#[test]
fn nb_impl_first_public_api_has_typed_methods() {
    let expected = [
        ("packages/NB/src/app.nova", "impl Application", "static fn from_env"),
        ("packages/NB/src/beans.nova", "impl<T> Singleton<T>", "static fn new"),
        ("packages/NB/src/beans.nova", "impl<T> Prototype<T>", "fn create"),
        ("packages/NB/src/web.nova", "impl Group", "static fn new"),
        ("packages/NB/src/http.nova", "impl RequestData", "static fn body"),
        ("packages/NB/src/response.nova", "impl Reply", "static fn created"),
        ("packages/NB/src/config.nova", "impl Properties", "static fn load"),
        ("packages/NB/src/health.nova", "impl Health", "static fn install"),
        ("packages/NB/src/security.nova", "impl Bearer", "fn allows"),
        ("packages/NBPostgres/src/lib.nova", "impl Database", "static fn transaction"),
    ];
    for (path, block, operation) in expected {
        let source=std::fs::read_to_string(path).unwrap();
        assert!(source.contains(block), "missing {block} in {path}");
        assert!(source.contains(operation), "missing {operation} in {path}");
    }
    let demo=std::fs::read_to_string("examples/packages/nb_backend/src/main.nova").unwrap();
    assert!(demo.contains("@BootApplication"));
    assert!(!demo.contains("impl DB"),"@BootApplication should generate main");
    assert!(!demo.lines().any(|line| line.starts_with("fn main")));
    assert!(!demo.contains("Routes::install"));
    let application=std::fs::read_to_string("packages/NB/src/app.nova").unwrap();
    assert!(application.contains("Application::from_env"));
    assert!(application.contains("pub fn boot("));
    assert!(!demo.contains("bootstrap.run(Routes::install)"));
    assert!(!demo.contains("Group::new"),"route grouping is generated from annotations");
    let controller=std::fs::read_to_string("examples/packages/nb_backend/src/user_controller.nova").unwrap();
    assert!(controller.contains("Reply::json"));
    assert!(controller.contains("RequestData::body::<CreateUser>"));
    assert!(demo.contains("pub mod user_controller;"));
    assert!(demo.contains("pub mod site_controller;"));
}

#[test]
fn nb_boot_uses_properties_as_port_source() {
    let application=std::fs::read_to_string("packages/NB/src/app.nova").unwrap();
    assert!(application.contains("static fn from_config("));
    assert!(application.contains("Application::from_config(port_environment,8080)"));
    assert!(application.contains("std::old::env::read(\"NB_PROPERTIES\")"));
    assert!(application.contains("properties.integer(\"server.port\",fallback)"));
    let cli=std::fs::read_to_string("src/main.rs").unwrap();
    assert!(cli.contains("process.env(\"NB_PROPERTIES\", absolute)"));
    let smoke=std::fs::read_to_string("examples/packages/nb_core_check/src/main.nova").unwrap();
    assert!(smoke.contains("Application::from_properties"));
    assert!(smoke.contains("nb_core_check/application.properties"));
}

#[test]
fn nova_implicit_numeric_widening_is_lossless_only() {
    let good = r#"
        fn main() -> i64 {
            let count: u32 = 12;
            let widened: i64 = count;
            return widened;
        }
    "#;
    let parsed = crate::grammar::ProgramParser::new().parse(good)
        .expect("parse lossless widening example");
    crate::surface::lower(&parsed)
        .expect("u32 to i64 must remain an implicit lossless widening");

    let unsafe_cast = r#"
        fn main() -> u64 {
            let signed: i64 = -1;
            let index: u64 = signed;
            return index;
        }
    "#;
    let parsed = crate::grammar::ProgramParser::new().parse(unsafe_cast)
        .expect("parse signed-to-unsigned example");
    assert!(crate::surface::lower(&parsed).is_err(),
        "implicit i64 to u64 would silently reinterpret negative values");
}

#[test]
fn nova_mutable_borrow_automatically_coerces_to_shared() {
    let source = r#"
        fn read(value: &i32) -> i32 { return *value; }
        fn generic_read<T>(value: &T) -> i32 { return 7; }
        struct Point { value: i32 }
        impl Point {
            fn peek(self: &Point) -> i32 { return self.value; }
        }
        fn main() -> i32 {
            let mut x: i32 = 12;
            let one: i32 = read(&mut x);
            let two: i32 = generic_read(&mut x);
            let shared: &i32 = &mut x;
            let three: i32 = read(shared);
            let mut p: Point = new Point { value: 4 };
            let borrowed: &mut Point = &mut p;
            let four: i32 = borrowed.peek();
            return one + two + three + four;
        }
    "#;
    let parsed = crate::grammar::ProgramParser::new().parse(source).unwrap();
    let lowered = crate::surface::lower(&parsed).expect("safe mut-to-shared reborrow must typecheck");
    let typed = crate::hir::lower(&lowered).expect("shared reborrow must reach typed HIR");
    let mir = crate::mir::lower(&typed).expect("shared reborrow must reach MIR");
    let llvm = crate::codegen::generate(&mir);
    assert!(llvm.contains("@main"));
}

#[test]
fn nova_shared_borrow_cannot_be_upgraded_to_mutable() {
    let source = r#"
        fn update(value: &mut i32) -> () { *value = 1; }
        fn main() -> i32 {
            let mut x: i32 = 4;
            let shared: &i32 = &x;
            update(shared);
            return x;
        }
    "#;
    let parsed = crate::grammar::ProgramParser::new().parse(source).unwrap();
    // Surface lowering constructs concrete calls; typed HIR is the final
    // enforcement point for argument types.  Check the whole type pipeline,
    // not just the preparatory surface pass.
    let result = crate::surface::lower(&parsed)
        .and_then(|lowered| crate::hir::lower(&lowered).map(|_| ()));
    assert!(result.is_err(),
        "a shared borrow cannot be passed to a mutable parameter");
}

#[test]
fn nb_controllers_generate_auto_registration_and_logger() {
    let parsed=package::load(std::path::Path::new("examples/packages/nb_backend"))
        .expect("load NB executable backend");
    let expanded=proc_macros::expand(&parsed)
        .expect("expand automatic route and logging decorators");
    let controller=methods(&expanded.items,"UserController");
    assert!(controller.iter().any(|m|m.name=="nb_path_hello"));
    assert!(!controller.iter().any(|m|m.name=="nb_register_routes"),
        "per-controller manual registration must not be necessary");
    let boot=methods(&expanded.items,"DB");
    let generated_main=boot.iter().find(|m|m.name=="main")
        .expect("@BootApplication must generate main if none was supplied");
    assert!(generated_main.attributes.iter().any(|a|a.name=="static"));
    let auto_register=boot.iter().find(|m|m.name=="nb_register_controllers")
        .expect("@BootApplication must generate the global controller registry");
    // The procedural generator runs after module alias resolution. Its newly
    // created signature must match the already-linked NB health API exactly.
    let health=methods(&expanded.items,"Health");
    let health_install=health.iter().find(|method|method.name=="install")
        .expect("NB::health::Health::install must be available");
    assert_eq!(auto_register.params[0].ty,health_install.params[0].ty,
        "generated route registry must use the canonical NB Router identity");
    let body=format!("{:?}",auto_register.body);
    for marker in ["register_GET", "register_POST", "register_PUT", "register_DELETE",
        "/api/users/hello", "/api/users/{id}", "/api/users/stats",
        "/assets/app.js", "/assets/app.css", "NB::health::Health::install"] {
        assert!(body.contains(marker),"missing {marker} in auto registry: {body}");
    }
    for class in ["UserController","UserService","DB"] {
        let class_methods=methods(&expanded.items,class);
        assert!(class_methods.iter().any(|m|m.name=="logger"),"missing @Logger method on {class}");
    }
}

#[test]
fn nb_legacy_metadata_only_handlers_are_not_automatically_registered() {
    let parsed=package::load(std::path::Path::new("examples/packages/nb_demo"))
        .expect("load legacy NB metadata example");
    let expanded=proc_macros::expand(&parsed).expect("legacy decorators expand");
    let controller=methods(&expanded.items,"UserController");
    assert!(controller.iter().any(|m|m.name=="nb_path_get"));
    assert!(!controller.iter().any(|m|m.name=="nb_register_routes"),
        "methods without the borrowed HTTP callback contract must never be registered as HTTP handlers");
}

#[test]
fn nb_fullstack_demo_assets_and_modules_are_present() {
    let base=std::path::Path::new("examples/packages/nb_backend");
    let main=std::fs::read_to_string(base.join("src/main.nova")).unwrap();
    for module in ["paths","user_model","user_repository","user_service",
        "user_configuration","user_controller","site_controller"] {
        assert!(main.contains(&format!("pub mod {module};")),"missing module {module}");
        assert!(base.join(format!("src/{module}.nova")).exists(),"missing module source {module}");
    }
    assert!(main.contains("@BootApplication"));
    assert!(!main.contains("impl DB"),"automatic boot must own the entrypoint");
    let user=std::fs::read_to_string(base.join("src/user_controller.nova")).unwrap();
    for marker in ["@GetMapping(\"/\")","@GetMapping(\"/{id}\")","@PostMapping(\"/\")",
        "@PutMapping(\"/{id}\")","@DeleteMapping(\"/{id}\")","RequestData::body::<CreateUser>"] {
        assert!(user.contains(marker),"missing REST endpoint: {marker}");
    }
    let page=std::fs::read_to_string(base.join("frontend/index.html")).unwrap();
    let js=std::fs::read_to_string(base.join("frontend/app.js")).unwrap();
    let styles=std::fs::read_to_string(base.join("frontend/app.css")).unwrap();
    assert!(page.contains("/assets/app.css") && page.contains("/assets/app.js"));
    assert!(page.contains("id=\"user-form\"") && styles.contains(".sidebar"));
    for http_method in ["'POST'","'PUT'","'DELETE'"] {
        assert!(js.contains(http_method),"missing browser HTTP action {http_method}");
    }
    let initial=std::fs::read_to_string(base.join("data/users.json")).unwrap();
    assert!(initial.starts_with('[') && initial.trim_end().ends_with(']'));
}
