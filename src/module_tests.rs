//! Run: cargo test module_tests -- --nocapture
use crate::{codegen, diagnostics, grammar, hir, mir, modules, package, strip_comments, surface};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("nova_modules_{}_{}", std::process::id(), index));
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(
            path.join("Nova.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\n",
        )
        .unwrap();
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        let file = self.0.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    fn root(&self) -> &Path {
        &self.0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn parses_new_module_syntax() {
    let source = "pub mod utils; use utils::math::sum; pub fn main() -> i32 { return sum(1, 2); }";
    grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .unwrap();
}
#[test]
fn example_compiles_across_three_files() {
    let (program, _) = package::load_detailed(Path::new("examples/packages/modules_demo")).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(llvm.contains("@string_example__hello"));
    assert!(llvm.contains("@utils__format__double"));
    assert!(llvm.contains("@main"));
}
#[test]
fn sibling_module_can_reference_public_type_in_public_module() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "pub mod lexer; mod parser; fn main() -> i32 { return parser::read(lexer::scan()); }",
    );
    f.write(
        "src/lexer.nova",
        "pub struct Scan { value: i32 } pub fn scan() -> Scan { return new Scan { value: 42 }; }",
    );
    f.write(
        "src/parser.nova",
        "pub fn read(value: lexer::Scan) -> i32 { return value.value; }",
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let _mir = mir::lower(&typed).unwrap();
}
#[test]
fn private_sibling_module_error_points_to_enclosing_mod_not_type() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod lexer; mod parser; fn main() -> i32 { return 0; }",
    );
    f.write("src/lexer.nova", "pub struct Scan { value: i32 }");
    f.write(
        "src/parser.nova",
        "pub fn read(value: lexer::Scan) -> i32 { return 0; }",
    );
    let err = package::load_detailed(f.root()).unwrap_err();
    assert!(err.contains("NOVA-M008"), "{err}");
    assert!(err.contains("enclosing module 'lexer' is private"), "{err}");
    assert!(err.contains("pub mod lexer;"), "{err}");
}
#[test]
fn module_calls_resolve_without_use() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod math; fn main() -> i32 { return math::sum(20,22)-42; }",
    );
    f.write(
        "src/math.nova",
        "pub fn sum(a:i32,b:i32)->i32 {return a+b;}",
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    assert!(surface::lower(&program).is_ok());
}
#[test]
fn can_import_directly_into_scope() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod math; use math::sum; fn main() -> i32 { return sum(20,22)-42; }",
    );
    f.write(
        "src/math.nova",
        "pub fn sum(a:i32,b:i32)->i32 {return a+b;}",
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    assert!(surface::lower(&program).is_ok());
}
#[test]
fn private_function_gets_source_snippet() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod math;\nuse math::secret;\nfn main()->i32 {return secret();}\n",
    );
    f.write("src/math.nova", "fn secret()->i32 {return 1;}");
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M008"), "{error}");
    assert!(error.contains("main.nova:2:"), "{error}");
    assert!(error.contains("use math::secret;"), "{error}");
    assert!(error.contains('^'), "{error}");
}
#[test]
fn missing_module_gets_precise_error() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "fn helper()->i32 {return 1;}\nmod missing;\nfn main()->i32{return 0;}\n",
    );
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M005"), "{error}");
    assert!(error.contains("main.nova:2:5"), "{error}");
    assert!(error.contains("mod missing;"), "{error}");
}
#[test]
fn syntax_error_gets_line_column_and_surroundings() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "fn ok()->i32{return 0;}\nfn main()->i32 {\n  let x: i32 = ;\n  return 0;\n}\n",
    );
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-P001"), "{error}");
    assert!(error.contains("main.nova:3:"), "{error}");
    assert!(error.contains("let x: i32 = ;"), "{error}");
    assert!(error.contains("return 0;"), "{error}");
    assert!(error.contains('^'), "{error}");
}
#[test]
fn unresolved_use_has_location() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "use no_such_module::foo;\nfn main()->i32{return 0;}",
    );
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M011"), "{error}");
    assert!(error.contains("no_such_module::foo"), "{error}");
}
#[test]
fn nested_directory_mod_resolves() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod util; use util::math::answer; fn main()->i32{return answer()-42;}",
    );
    f.write("src/util/mod.nova", "pub mod math;");
    f.write("src/util/math.nova", "pub fn answer()->i32{return 42;}");
    let (program, _) = package::load_detailed(f.root()).unwrap();
    assert!(surface::lower(&program).is_ok());
}
#[test]
fn direct_access_to_private_function_is_rejected() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod lib; fn main()->i32{return lib::private_fn();}",
    );
    f.write("src/lib.nova", "fn private_fn()->i32 {return 1;}");
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M008"), "{error}");
    assert!(error.contains("private_fn"), "{error}");
}
#[test]
fn conflicting_file_module_resolution_is_reported() {
    let f = Fixture::new();
    f.write("src/main.nova", "mod util; fn main()->i32{return 0;}");
    f.write("src/util.nova", "pub fn value()->i32 {return 0;}");
    f.write("src/util/mod.nova", "pub fn value()->i32 {return 0;}");
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M006"), "{error}");
}
#[test]
fn type_from_other_file_must_be_public() {
    let f = Fixture::new();
    f.write("src/main.nova","mod lib; fn main()->i32 {let x: lib::Private = new lib::Private {value: 42}; return x.value-42;}");
    f.write("src/lib.nova", "struct Private {value: i32}");
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M008"), "{error}");
}
#[test]
fn public_struct_and_nested_type_resolve() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "mod lib; use lib::Point; fn main()->i32 {let p:Point = new Point {x:42};return p.x-42;}",
    );
    f.write("src/lib.nova", "pub struct Point {x: i32}");
    let (program, _) = package::load_detailed(f.root()).unwrap();
    assert!(surface::lower(&program).is_ok());
}
#[test]
fn semantic_exact_if_unique_identifier() {
    let source = modules::SourceFile {
        path: "test.nova".into(),
        text: "fn main()->i32 { return missing(); }".into(),
        namespace: "".into(),
    };
    let rendered = diagnostics::semantic_with_sources("unknown function 'missing'", &[source]);
    assert!(rendered.contains("test.nova:1:25"), "{rendered}");
    assert!(rendered.contains('^'));
}

// Callback method parameters declared with an unqualified type inside their
// own module must accept the same type passed from another module, including
// lifted lambdas and named functions. This must not bypass type checking.
#[test]
fn namespaced_method_callback_types_are_resolved_before_matching() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        r#"
        pub mod api;
        fn handler(req: api::Request) -> i32 { return req.value; }
        fn main() -> i32 {
            let app: api::App = new api::App { port: 7888 };
            let one: i32 = app.run(handler);
            let two: i32 = app.run(fn(req: api::Request) -> i32 {
                return req.value;
            });
            return one + two - 42;
        }
    "#,
    );
    f.write(
        "src/api.nova",
        r#"
        pub struct Request { value: i32 }
        pub struct App { port: i32 }
        impl App {
            fn run(self: App, cb: fn(Request) -> i32) -> i32 {
                return cb(new Request { value: 21 });
            }
        }
    "#,
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(
        llvm.contains("api__Request"),
        "missing qualified callback type"
    );
}

#[test]
fn nexo_async_counter_typechecks_and_generates_llvm() {
    let path = Path::new("examples/packages/nexo_async_counter");
    let (program, _sources) = package::load_detailed(path).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let ir = codegen::generate(&blocks);
    assert!(
        ir.contains("@__nova_std__net____tcp_listen"),
        "missing generic TCP listener"
    );
    assert!(
        ir.contains("@nexo__response__send_raw"),
        "missing Nova HTTP responder"
    );
    assert!(
        ir.contains("@__nova_std__net____tcp_read"),
        "missing generic TCP receive"
    );
    assert!(
        ir.contains("@__nova_std__json____release_text"),
        "missing owned JSON cleanup"
    );
    assert!(
        ir.contains("@__nova_std__generator____next"),
        "missing generator sequence"
    );
    assert!(
        ir.contains("@__nova_std__task____block_on"),
        "missing async task scheduling"
    );
}

#[test]
fn nexo_async_live_typechecks() {
    let (p, _) = package::load_detailed(Path::new("examples/packages/nexo_async_live")).unwrap();
    let lowered = surface::lower(&p).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    mir::lower(&typed).unwrap();
}

#[test]
fn lambda_callbacks_are_typed_and_indirectly_called() {
    let (program, _) =
        package::load_detailed(Path::new("examples/packages/lambda_callbacks")).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let mir = mir::lower(&typed).unwrap();
    let ir = codegen::generate(&mir);
    assert!(
        ir.contains("@doubled"),
        "named function pointer must be emitted"
    );
    assert!(
        ir.contains("call i64 %"),
        "indirect callback invocation must compile"
    );
}

#[test]
fn stdlib_owners_have_typed_cleanup_and_generate_llvm() {
    let (program, _) =
        package::load_detailed(Path::new("examples/packages/stdlib_owners")).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(
        llvm.contains("@__nova_std__vec____int64_vector_destroy"),
        "missing managed vector cleanup"
    );
    assert!(
        llvm.contains("@__nova_std__memory____buffer_drop"),
        "missing managed buffer cleanup"
    );
    assert!(
        llvm.contains("@__nova_std__arena____drop"),
        "missing arena cleanup"
    );
    assert!(
        llvm.contains("@__nova_std__string____string_destroy"),
        "missing string cleanup"
    );
}

#[test]
fn nova_012_native_iterators_ast_macros_and_generator_keywords_reach_llvm() {
    let project = Path::new("examples/packages/language_features_012");
    let (program, _) = package::load_detailed(project).unwrap();
    let expanded = surface::lower(&program).unwrap();
    let typed = hir::lower(&expanded).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(
        llvm.contains("@NumberRange__begin"),
        "missing user-defined begin()"
    );
    assert!(
        llvm.contains("@NumberRange__end"),
        "missing user-defined end()"
    );
    assert!(
        llvm.contains("@NumberCursor__next"),
        "missing user-defined next()"
    );
    assert!(
        llvm.contains("@NumberCursor__equals"),
        "missing user-defined equals()"
    );
    assert!(
        llvm.contains("@__nova_std__generator____next"),
        "keyword `next` did not lower"
    );
    assert!(
        llvm.contains("@__nova_std__generator____done"),
        "keyword `finished` did not lower"
    );
}

#[test]
fn nova_012_std_vec_supports_native_for_in() {
    let fixture = Fixture::new();
    fixture.write("Nova.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\n[dependencies]\nstd = { builtin = \"std\" }\n");
    fixture.write(
        "src/main.nova",
        r#"
        fn main()->i32 {
            let numbers=std::vec::new_int64_vector();
            numbers.append(10);
            numbers.append(20);
            let mut total:i64=0;
            for number in numbers {total += number;}
            if total != 30 {return 1;}
            return 0;
        }
    "#,
    );
    let (program, _) = package::load_detailed(fixture.root()).unwrap();
    let expanded = surface::lower(&program).unwrap();
    let typed = hir::lower(&expanded).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(
        llvm.contains("@std__vec__Int64Vector__begin"),
        "missing standard vector begin"
    );
    assert!(
        llvm.contains("@std__vec__Int64Cursor__next"),
        "missing standard vector cursor next"
    );
}

#[test]
fn trait_alias_and_capturing_closure_compile_into_mir_and_llvm() {
    let source = r#"
        typedef Integer = i32;
        trait Numeric {fn doubled(self: Self)->Self;}
        trait<T in i8, i16, i32, i64> IntegerOperations extends Numeric {
            fn plus(self: T, additional: T)->T;
        }
        impl IntegerOperations for Integer {
            fn doubled(self: Integer)->Integer {return self+self;}
            fn plus(self: Integer, additional: Integer)->Integer{return self+additional;}
        }
        fn main()->Integer {
            let increment: Integer = 2;
            let operation = fn(value: Integer)->Integer {return value+increment;};
            let answer: Integer = operation(40);
            return answer.plus(0)-42;
        }
    "#;
    let parsed = grammar::ProgramParser::new().parse(source).unwrap();
    let lowered = surface::lower(&parsed).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let verified = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&verified);
    assert!(llvm.contains("__nova_closure_"));
    assert!(llvm.contains("i32__plus"));
}

#[test]
fn drop_trait_is_a_checked_raii_contract() {
    let source = r#"
        trait Drop {fn drop(self: Self)->();}
        struct Resource {value: i32}
        impl Drop for Resource {fn drop(self: Resource)->() {return;}}
        fn main()->i32 {
            let resource = new Resource {value: 42};
            return 0;
        }
    "#;
    let parsed = grammar::ProgramParser::new().parse(source).unwrap();
    let lowered = surface::lower(&parsed).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let verified = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&verified);
    assert!(llvm.contains("Resource__drop"));
}

#[test]
fn default_named_arguments_and_trait_defaults_reach_llvm() {
    let source = r#"
        trait Scalar {fn twice(self:Self)->Self{return self+self;}}
        impl Scalar for i32 {}
        fn compute(base:i32,multiplier:i32=2,offset:i32=3)->i32{return base*multiplier+offset;}
        fn main()->i32 {let result=compute(5,offset:9);return result.twice()-38;}
    "#;
    let parsed = grammar::ProgramParser::new().parse(source).unwrap();
    let lowered = surface::lower(&parsed).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let verified = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&verified);
    assert!(llvm.contains("@i32__twice"));
    assert!(llvm.contains("@compute"));
}

#[test]
fn atomic_ffi_has_typed_nova_wrappers() {
    let (parsed, _) =
        package::load_detailed(Path::new("examples/packages/language_features_014")).unwrap();
    let lowered = surface::lower(&parsed).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let verified = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&verified);
    assert!(llvm.contains("__nova_std__atomic____fetch_add_i64"));
    assert!(llvm.contains("__nova_std__atomic____compare_exchange_i64"));
}

#[test]
fn custom_struct_add_operator_dispatches_to_trait_impl() {
    let source = r#"
        trait Add {fn add(self:Self,other:Self)->Self;}
        struct Pair {number:i32}
        impl Add for Pair {
            fn add(self:Pair,other:Pair)->Pair {
                return new Pair {number:self.number+other.number};
            }
        }
        fn main()->i32 {
            let first=new Pair {number:16};
            let second=new Pair {number:26};
            let sum=first+second;
            return sum.number-42;
        }
    "#;
    let parsed = grammar::ProgramParser::new().parse(source).unwrap();
    let lowered = surface::lower(&parsed).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let verified = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&verified);
    assert!(llvm.contains("Pair__add"));
}

// Bootstrap 0.16: typed asynchronous owners must survive package loading,
// generic monomorphization, HIR borrow checking, MIR and LLVM generation.
#[test]
fn typed_task_future_generator_and_coroutine_reach_llvm() {
    let fixture = Fixture::new();
    fixture.write("Nova.toml", "[package]\nname=\"typed_async\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd = { builtin = \"std\" }\n");
    fixture.write(
        "src/main.nova",
        r#"
        async fn compute(value:i64)->i64 {return value+1;}
        gen fn values()->i64 {yield 10;return 0;}
        fn main()->i32 {
            let task:std::task::Task<i64> = std::task::manage(compute(41));
            let answer:i64=task.wait();
            let future:std::task::Future<i64> = std::task::future(compute(2));
            let other:i64=future.await_result();
            let generator:std::generator::Generator<i64> = std::generator::manage(values());
            let yielded:i64=generator.next();
            let co:std::coro::Coroutine<i64> = std::coro::manage(values());
            let first:i64=co.resume(0);
            return (answer + other + yielded + first - 65) as i32;
        }
    "#,
    );
    let program = package::load(fixture.root()).expect("typed async package loads");
    let concrete = surface::lower(&program).expect("typed async generics specialize");
    let checked = hir::lower(&concrete).expect("typed async owners pass ownership checks");
    let mir = mir::lower(&checked).expect("typed async owners lower into MIR");
    mir::verify(&mir).expect("typed async MIR is valid");
    let llvm = codegen::generate(&mir);
    assert!(llvm.contains("__nova_std__task____block_on"), "{llvm}");
    assert!(llvm.contains("__nova_std__task____await_value"), "{llvm}");
    assert!(llvm.contains("__nova_std__generator____next"), "{llvm}");
    assert!(llvm.contains("__nova_std__coro____drop"), "{llvm}");
}

#[test]
fn typed_async_abi_rejects_struct_results_instead_of_reinterpreting_memory() {
    let fixture = Fixture::new();
    fixture.write("Nova.toml", "[package]\nname=\"typed_async_rejection\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd = { builtin = \"std\" }\n");
    fixture.write(
        "src/main.nova",
        r#"
        struct Payload { number:i64 }
        fn main()->i32 {
            let bad:std::task::Task<Payload> = std::task::manage_typed::<Payload>(1);
            return 0;
        }
    "#,
    );
    let ast = package::load(fixture.root()).expect("generic Task<Payload> parses");
    let error =
        surface::lower(&ast).expect_err("arbitrary T is not compatible with i64 coroutine ABI");
    assert!(error.contains("stage-0 coroutine ABI"), "{error}");
}

#[test]
fn unsafe_typed_handle_factory_cannot_be_called_from_safe_code() {
    let fixture = Fixture::new();
    fixture.write("Nova.toml", "[package]\nname=\"typed_unsafe\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd = { builtin = \"std\" }\n");
    fixture.write(
        "src/main.nova",
        r#"
        fn main()->i32 {
            let task:std::task::Task<u64> = std::task::manage_typed::<u64>(1);
            return 0;
        }
    "#,
    );
    let ast = package::load(fixture.root()).unwrap();
    let lowered = surface::lower(&ast).unwrap();
    let error = hir::lower(&lowered).expect_err("an unverified raw handle needs unsafe");
    assert!(error.contains("unsafe block"), "{error}");
}

#[test]
fn typed_async_bootstrap_example_compiles_to_llvm() {
    let program = package::load(Path::new("examples/packages/typed_async_016"))
        .expect("Nova typed async package must load");
    let specialized = surface::lower(&program).expect("typed await/join/next/resume specialize");
    let typed = hir::lower(&specialized).expect("typed async example respects ownership");
    let lowered = mir::lower(&typed).expect("typed async example lowers");
    mir::verify(&lowered).expect("typed async MIR verifies");
    let ir = codegen::generate(&lowered);
    assert!(ir.contains("@__nova_std__task____await_value"), "{ir}");
    assert!(ir.contains("@__nova_std__task____block_on"), "{ir}");
    assert!(ir.contains("@__nova_std__generator____next"), "{ir}");
    assert!(ir.contains("@__nova_std__coro____resume"), "{ir}");
}

#[test]
fn stdlib_017_borrowed_resource_facades_lower_to_llvm() {
    let (program, _) =
        package::load_detailed(Path::new("examples/packages/stdlib_contracts_017")).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let ir = codegen::generate(&blocks);
    assert!(
        ir.contains("@__nova_std__string____string_clone"),
        "owned string clone must link"
    );
    assert!(
        ir.contains("@__nova_std__memory____buffer_copy"),
        "borrowed buffer copies must link"
    );
    assert!(
        ir.contains("@__nova_std__map____string_int64_map_destroy"),
        "nested symbol-table owner must drop"
    );
}

#[test]
fn stdlib_017_json_view_does_not_consume_its_owner() {
    let source = r#"
      struct JsonText { value: str }
      impl JsonText {
          fn as_str(self: &JsonText) -> str { return self.value; }
          fn drop(self: JsonText) -> () { return; }
      }
      fn main() -> i32 {
          let text: JsonText = new JsonText {value: "hello"};
          let first: str = text.as_str();
          let second: str = text.as_str();
          return 0;
      }
    "#;
    let parsed = grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .unwrap();
    let lowered = surface::lower(&parsed).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    mir::lower(&typed).unwrap();
}

#[test]
fn cross_module_concept_import_checks_instantiations() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        r#"
        pub mod constraints;
        use constraints::Small;
        fn increment<T: Small>(value:T)->T {return value+1;}
        fn main()->i32 {return increment(41)-42;}
    "#,
    );
    f.write(
        "src/constraints.nova",
        r#"
        pub concept Small<T> = integer<T> && !same_as<T,u64>;
    "#,
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let _ir = mir::lower(&typed).unwrap();
}

#[test]
fn private_concept_must_not_escape_its_module() {
    let f = Fixture::new();
    f.write(
        "src/main.nova",
        "pub mod constraints; use constraints::Hidden; fn main()->i32{return 0;}",
    );
    f.write("src/constraints.nova", "concept Hidden<T> = integer<T>;");
    let err = package::load_detailed(f.root()).unwrap_err();
    assert!(err.contains("NOVA-M008"), "{err}");
}

// The HTTP router is a separately resolved Nova dependency, not an embedded
// std module nor a hidden linker dependency on application-specific C code.
#[test]
fn nexo_package_is_independent_of_std_http() {
    assert!(!crate::modules::EMBEDDED_STD
        .iter()
        .any(|(name, _)| *name == "express.nova" || *name == "http.nova"));
    let (program, _) = package::load_detailed(Path::new("examples/packages/nexo_async_counter"))
        .expect("Nexo package must load by its own manifest");
    let lowered = surface::lower(&program).expect("Nexo module surface");
    let typed = hir::lower(&lowered).expect("Nexo typed functions");
    let blocks = mir::lower(&typed).expect("Nexo MIR");
    let llvm = codegen::generate(&blocks);
    assert!(llvm.contains("nexo__app__App"), "{llvm}");
    assert!(!llvm.contains("@std__nexo__"), "{llvm}");
    assert!(!llvm.contains("@std__http__"), "{llvm}");
}

#[test]
fn modern_std_vector_and_symbols_reach_private_runtime_abi() {
    let (program, _) = package::load_detailed(Path::new("examples/packages/modern_std"))
        .expect("modern STD example must load");
    let lowered = surface::lower(&program).expect("modern STD surface");
    let typed = hir::lower(&lowered).expect("modern STD typecheck");
    let blocks = mir::lower(&typed).expect("modern STD MIR");
    let llvm = codegen::generate(&blocks);
    assert!(
        llvm.contains("@__nova_std__vec____byte_vector_create"),
        "{llvm}"
    );
    assert!(
        llvm.contains("@__nova_std__vec____byte_vector_set"),
        "{llvm}"
    );
    assert!(
        llvm.contains("@__nova_std__vec____byte_vector_pop"),
        "{llvm}"
    );
    assert!(
        llvm.contains("@__nova_std__map____string_int64_map_insert"),
        "{llvm}"
    );
    assert!(!llvm.contains("@std__vec__u8_get"), "{llvm}");
}

#[test]
fn nova_ffi_names_are_readable_but_link_to_private_runtime_symbols() {
    let (program, _) = package::load_detailed(Path::new("examples/packages/modern_std"))
        .expect("modern std package is loadable");
    let lowered = surface::lower(&program).expect("Nova-facing FFI aliases resolve");
    let typed = hir::lower(&lowered).expect("typed FFI functions resolve");
    let llvm = codegen::generate(&mir::lower(&typed).expect("FFI lowers to MIR"));
    assert!(
        llvm.contains("@__nova_std__vec____byte_vector_create"),
        "{llvm}"
    );
    assert!(
        llvm.contains("@__nova_std__vec____byte_vector_get"),
        "{llvm}"
    );
    assert!(
        !llvm.contains("@std__vec__native_byte_vector_get"),
        "{llvm}"
    );
    assert!(
        !llvm.contains("@__nova_std__vec__native_byte_vector_get"),
        "{llvm}"
    );
    let source = fs::read_to_string("std/src/vec.nova").unwrap();
    assert!(source.contains("extern fn native_byte_vector_get"));
    assert!(!source.contains("extern fn __byte_vector_get"));
}

// Nexo is a file-based package, not a single large embedded source file.
#[test]
fn nexo_modular_modules_typecheck_and_generate_llvm() {
    let root = Path::new("packages/nexo/src");
    let lib = fs::read_to_string(root.join("lib.nova")).unwrap();
    assert!(
        lib.lines().count() <= 24,
        "lib.nova must remain a small module manifest"
    );
    for module in [
        "request",
        "response",
        "method",
        "path",
        "query",
        "headers",
        "routing",
        "middleware",
        "app",
        "server",
        "connection",
        "config",
    ] {
        assert!(
            lib.contains(&format!("pub mod {module};")),
            "module not exported: {module}"
        );
        assert!(
            root.join(format!("{module}.nova")).is_file(),
            "module not found: {module}"
        );
    }
    let (p, sources) = package::load_detailed(Path::new("examples/packages/nexo_modular"))
        .expect("Nexo module namespace tree must resolve");
    assert!(sources
        .iter()
        .any(|s| s.namespace.ends_with("nexo::routing")));
    let lowered = surface::lower(&p).expect("Nexo modular surface");
    let typed = hir::lower(&lowered).expect("Nexo modular HIR");
    let blocks = mir::lower(&typed).expect("Nexo modular MIR");
    mir::verify(&blocks).expect("Nexo modular MIR verifier");
    let llvm = codegen::generate(&blocks);
    assert!(llvm.contains("nexo__headers__content_length"));
    assert!(llvm.contains("nexo__response__normalize_status"));
}

#[test]
fn nexo_close_consumes_app_and_lets_generated_drop_release_fields() {
    let source = fs::read_to_string("packages/nexo/src/app.nova").unwrap();
    assert!(source.contains("fn close(self: App) -> () { return; }"));
    assert!(!source.contains("self.routes.drop();"));
    assert!(!source.contains("self.middlewares.drop();"));
}

// Nova 0.20 refuses assignment to an owning String field through *mut T.
// Nexo keeps the copied route path in one managed handle, initialized once
// at node construction and destroyed exactly once by RouteCollection::drop.
#[test]
fn nexo_route_pattern_handle_has_single_owner_and_no_field_overwrite() {
    let source = fs::read_to_string("packages/nexo/src/routing.nova").unwrap();
    assert!(source.contains("path_handle: u64"));
    assert!(source.contains("native_string_create_from_text(path)"));
    assert!(source.contains("native_string_destroy(current.path_handle)"));
    assert!(source.contains("native_string_destroy(sentinel.path_handle)"));
    assert!(!source.contains("route.path ="));
    let (program, _) = package::load_detailed(Path::new("examples/packages/nexo_modular")).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let mir = mir::lower(&typed).unwrap();
    mir::verify(&mir).unwrap();
}

#[test]
fn std_021_internal_runtime_not_visible_to_application() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write("src/main.nova", "import std; fn main() -> i32 { let handle = std::internal::map::native_int64_map_create(); return 0; }");
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(
        error.contains("NOVA-M015") && error.contains("runtime layer is private"),
        "{error}"
    );
}
#[test]
fn std_021_typed_numeric_map_generates_llvm() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write(
        "src/main.nova",
        r#"
        import std;
        fn main() -> i32 {
            let mut values = std::map::new_map::<i64,i32>();
            values.insert(42,7);
            if !values.contains(42) {return 1;}
            if values.get_or(42,0) != 7 {return 2;}
            return 0;
        }
    "#,
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(
        llvm.contains("@__nova_std__internal__map____int64_map_create"),
        "{llvm}"
    );
}
#[test]
fn std_021_typed_map_rejects_nonintegral_values() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write(
        "src/main.nova",
        "import std; fn main()->i32 { let m=std::map::new_map::<i64,f64>(); return 0; }",
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let error = surface::lower(&program).unwrap_err();
    assert!(
        error.contains("constraint") && error.contains("MapValue"),
        "{error}"
    );
}

#[test]
fn std_021_generic_map_has_automatic_drop_glue() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write(
        "src/main.nova",
        r#"
        import std;
        fn main()->i32 {
            let values = std::map::new_map::<i64,i64>();
            return 0;
        }
    "#,
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let lowered = surface::lower(&program).unwrap();
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(llvm.contains("std__map__Map__i64__i64__drop"), "{llvm}");
    assert!(
        llvm.contains("@__nova_std__internal__map____int64_map_destroy"),
        "{llvm}"
    );
}

#[test]
fn std_021_renamed_builtin_alias_cannot_bypass_internal_seal() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\ncore={ builtin=\"std\" }\n");
    f.write("src/main.nova", "import core; fn main()->i32 {let raw=core::internal::map::native_int64_map_create();return 0;}");
    let error = package::load_detailed(f.root()).unwrap_err();
    assert!(error.contains("NOVA-M015"), "{error}");
}

#[test]
fn std_022_numeric_collection_constraints_reject_unsigned_word_reinterpretation() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nentry=\"src/main.nova\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write(
        "src/main.nova",
        "import std; fn main()->i32 {let m=std::map::new_map::<u64,i64>();return 0;}",
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let error = surface::lower(&program).unwrap_err();
    assert!(
        error.contains("constraint") && error.contains("MapKey"),
        "{error}"
    );
}

#[test]
fn std_022_map_and_set_insert_arity_resolves_from_receiver() {
    // Map.insert(key, value) and Set.insert(value) coexist in the same
    // program. The AST named/default argument pass must not choose the map
    // signature for set.insert(value) before method dispatch is typed.
    let (program, _) =
        package::load_detailed(Path::new("examples/packages/std_containers_021")).unwrap();
    let lowered = surface::lower(&program).expect("surface must resolve insert via receiver type");
    let typed = hir::lower(&lowered).expect("map/set calls must typecheck");
    let blocks = mir::lower(&typed).expect("map/set calls must lower to MIR");
    mir::verify(&blocks).expect("map/set calls must produce valid MIR");
    let llvm = codegen::generate(&blocks);
    assert!(llvm.contains("@main"));
}

#[test]
fn std_027_unqualified_enum_variants_follow_expected_type_across_namespaces() {
    // A dependency can embed its own STD (e.g. nexo::std), duplicating
    // Option::Some/None declarations. Return types must disambiguate them.
    let source = r#"
        namespace left {
            enum Choice<T> { Some(T), None }
            fn missing() -> Choice<i64> { return None; }
        }
        namespace right {
            enum Choice<T> { Some(T), None }
            fn present() -> Choice<i64> { return Some(42); }
        }
        fn main() -> i32 { return 0; }
    "#;
    let parsed = grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .expect("parse namespace-local enum variants");
    let lowered =
        surface::lower(&parsed).expect("resolve Some and None using concrete enum return types");
    let typed = hir::lower(&lowered).expect("type-check contextually resolved enum variants");
    let _mir = mir::lower(&typed).expect("lower both contextual variants to MIR");
}

#[test]
fn print_macros_037_reach_typed_llvm_with_std() {
    let (program, _) = package::load_detailed(Path::new("examples/packages/print_macros_037"))
        .expect("print macros demo package must resolve embedded STD");
    let lowered = surface::lower(&program).expect("typed macro printing must specialize");
    let typed = hir::lower(&lowered).expect("typed printing must pass HIR checks");
    let verified = mir::lower(&typed).expect("typed printing must pass MIR checks");
    mir::verify(&verified).expect("typed printing must produce valid MIR");
    let llvm = codegen::generate(&verified);
    for native in [
        "__nova_std__io____print",
        "__nova_std__io____print_i64",
        "__nova_std__io____print_bool",
        "__nova_std__io____print_f64",
        "__nova_std__io____print_debug_str",
        "__nova_std__io____eprint_i64",
    ] {
        assert!(
            llvm.contains(native),
            "missing {native} from output: {llvm}"
        );
    }
}

#[test]
fn print_macros_reject_wrong_format_at_ast_expansion() {
    for (source, code) in [
        (
            "fn main()->i32 {println!(\"{} {}\", 1);return 0;}",
            "NOVA-FMT002",
        ),
        ("fn main()->i32 {println!(\"{\");return 0;}", "NOVA-FMT003"),
        (
            "fn main()->i32 {println!(\"{:x}\", 3);return 0;}",
            "NOVA-FMT004",
        ),
        ("fn main()->i32 {println!(42);return 0;}", "NOVA-FMT001"),
    ] {
        let parsed = grammar::ProgramParser::new().parse(source).unwrap();
        let error = crate::macros::expand(&parsed).unwrap_err();
        assert!(
            error.contains(code),
            "expected {code} for {source}: {error}"
        );
    }
}

#[test]
fn print_macros_reject_precision_on_non_float_before_llvm() {
    let fixture = Fixture::new();
    fixture.write("Nova.toml", "[package]\nname=\"fmt_check\"\nversion=\"0.1.0\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    fixture.write(
        "src/main.nova",
        "import std; fn main()->i32 {let value:i64=42;println!(\"{:.2}\",value);return 0;}",
    );
    let (program, _) = package::load_detailed(fixture.root()).unwrap();
    let error = surface::lower(&program).unwrap_err();
    assert!(error.contains("NOVA-FMT004"), "{error}");
}

#[test]
fn print_macros_are_valid_inside_declarative_ast_macros() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"nested_print\"\nversion=\"0.1.0\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write(
        "src/main.nova",
        r#"
        import std;
        macro_rules! show {
            ($value:expr) => { println!("item: {}", $value); };
        }
        fn main()->i32 { let value:i64=42;show!(value);return 0; }
    "#,
    );
    let (program, _) = package::load_detailed(f.root()).unwrap();
    let concrete = surface::lower(&program).expect("nested print macro should typecheck");
    let typed = hir::lower(&concrete).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    mir::verify(&blocks).unwrap();
    let llvm = codegen::generate(&blocks);
    assert!(llvm.contains("__nova_std__io____print_i64"), "{llvm}");
}

#[test]
fn format_macros_038_return_owned_string_and_reach_llvm() {
    let (program, _) = package::load_detailed(Path::new("examples/packages/format_macros_038"))
        .expect("format! example with embedded STD must load");
    let lowered = surface::lower(&program).expect("format! must typecheck");
    let typed = hir::lower(&lowered).expect("format! must lower to typed HIR");
    let blocks = mir::lower(&typed).expect("format! must lower to MIR");
    mir::verify(&blocks).expect("format! must produce valid MIR");
    let llvm = codegen::generate(&blocks);
    for symbol in [
        "__nova_std__string____format_append_text",
        "__nova_std__string____format_append_i64",
        "__nova_std__string____format_append_f64",
        "__nova_std__string____format_append_debug_str",
        "__nova_std__string____string_destroy",
    ] {
        assert!(
            llvm.contains(symbol),
            "expected formatter/RAII symbol {symbol} in LLVM"
        );
    }
}

#[test]
fn format_macros_038_reject_invalid_format_and_type() {
    let fixture = Fixture::new();
    fixture.write("Nova.toml", "[package]\nname=\"fmt_expr\"\nversion=\"0.1.0\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    for (source, error_code) in [
        ("let s = format!(\"{} {}\", 1);", "NOVA-FMT002"),
        ("let s = format!(\"{\");", "NOVA-FMT003"),
        ("let s = format!(\"{:.2}\", 42);", "NOVA-FMT004"),
        ("let s = format!(\"{:x}\", 1);", "NOVA-FMT004"),
    ] {
        fixture.write(
            "src/main.nova",
            &format!("import std; fn main()->i32 {{ {source} return 0; }}"),
        );
        let (parsed, _) = package::load_detailed(fixture.root()).unwrap();
        let err = surface::lower(&parsed).unwrap_err();
        assert!(err.contains(error_code), "{source}: {err}");
    }
}

#[test]
fn format_macros_038_can_be_used_as_expression_statement_and_return_value() {
    let f = Fixture::new();
    f.write("Nova.toml", "[package]\nname=\"fmt_nested\"\nversion=\"0.1.0\"\n[dependencies]\nstd={ builtin=\"std\" }\n");
    f.write(
        "src/main.nova",
        r#"
        import std;
        fn build() -> std::string::String {
            return format!("Nested {}", format!("{}", 42));
        }
        fn main() -> i32 {
            format!("discard {}", 17);
            let result = build();
            println!("{}", result);
            // Named Strings must remain borrowed and usable after printing.
            println!("{}", result);
            // Temporary String values are consumed by a named STD helper.
            println!("{}", format!("temporary {}", 18));
            println!("{:?}", format!("debug {}", 19));
            eprintln!("{}", format!("stderr {}", 20));
            let another = format!("debug {:?}", format!("nested {}", 21));
            println!("{}", another);
            return 0;
        }
    "#,
    );
    let (parsed, _) = package::load_detailed(f.root()).unwrap();
    let lowered =
        surface::lower(&parsed).expect("format! supports statement, nested expression and return");
    let typed = hir::lower(&lowered).unwrap();
    let blocks = mir::lower(&typed).unwrap();
    mir::verify(&blocks).unwrap();
    assert!(codegen::generate(&blocks).contains("__nova_std__string____format_append_text"));
}
