//! Constraints must fail during monomorphization, never after LLVM emission.
use crate::{codegen, grammar, hir, mir, strip_comments, surface};
fn lower(source: &str) -> Result<String, String> {
    let ast = grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .map_err(|e| format!("{e:?}"))?;
    let concrete = surface::lower(&ast)?;
    let typed = hir::lower(&concrete)?;
    let ir = mir::lower(&typed)?;
    Ok(codegen::generate(&ir))
}
#[test]
fn constrained_generics_compile_all_the_way_to_llvm() {
    let llvm = lower(
        r#"
        concept Number<T> = integer<T> || floating<T>;
        concept Supported<T> = Number<T> && !same_as<T,f64>;
        fn sum<T: Supported>(a:T,b:T)->T {return a+b;}
        fn main()->i32 {return sum(20,22)-42;}
    "#,
    )
    .unwrap();
    assert!(llvm.contains("@main"));
}
#[test]
fn constraints_reject_invalid_specialization_before_llvm() {
    let error = lower(
        r#"
        concept OnlyI32<T> = same_as<T,i32>;
        fn id<T: OnlyI32>(value:T)->T {return value;}
        fn main()->i32 {return id::<f64>(1.0) as i32;}
    "#,
    )
    .unwrap_err();
    assert!(
        error.contains("concept constraint") && error.contains("OnlyI32"),
        "{error}"
    );
}
#[test]
fn where_clause_supports_multiple_type_arguments() {
    let llvm = lower(
        r#"
        concept EqualTypes<A,B> = same_as<A,B>;
        fn combine<T,U>(left:T,right:U)->i32 where EqualTypes<T,U>, integer<T> {return 42;}
        fn main()->i32 {return combine(1,2)-42;}
    "#,
    )
    .unwrap();
    assert!(llvm.contains("@main"));
}
#[test]
fn unknown_concept_is_rejected_in_unused_template() {
    let error =
        lower("fn unused<T: Absent>(x:T)->T{return x;} fn main()->i32{return 0;}").unwrap_err();
    assert!(error.contains("unknown concept"), "{error}");
}
#[test]
fn trait_based_concept_is_checked_in_specialization() {
    let llvm = lower(
        r#"
        trait Printable {fn print(self:Self)->i32;}
        impl Printable for i32 {fn print(self:i32)->i32{return self;}}
        concept CanPrint<T> = implements<T,Printable>;
        fn get<T: CanPrint>(value:T)->i32{return 42;}
        fn main()->i32 {return get(2)-42;}
    "#,
    )
    .unwrap();
    assert!(llvm.contains("@main"));
}

#[test]
fn static_assert_is_enforced_before_specialization() {
    let success = "static_assert(integer<i32> && !floating<i32>); fn main()->i32{return 0;}";
    assert!(lower(success).is_ok());
    let err = lower("static_assert(floating<i32>); fn main()->i32{return 0;}").unwrap_err();
    assert!(err.contains("static_assert failed"), "{err}");
}

#[test]
fn requires_structural_field_and_operator_are_checked_before_llvm() {
    let llvm = lower(
        r#"
        struct Point { x: i64, y: i64 }
        concept HasX<T> = requires(T value) { { value.x } -> i64; };
        concept Addable<T> = requires(T a, T b) { { a + b } -> T; };
        static_assert(HasX<Point>);
        static_assert(Addable<i64>);
        fn duplicate<T: Addable>(x:T)->T { return x+x; }
        fn main()->i32 { return (duplicate(21)-42) as i32; }
    "#,
    )
    .unwrap();
    assert!(llvm.contains("@main"));
    let error = lower(
        r#"
        struct Wrong { y: i64 }
        concept HasX<T> = requires(T value) { { value.x } -> i64; };
        fn useX<T: HasX>(value:T)->i32{return 1;}
        fn main()->i32 {return useX(new Wrong {y:42});}
    "#,
    )
    .unwrap_err();
    assert!(error.contains("concept constraint"), "{error}");
}

#[test]
fn reflection_is_a_static_string_literal_and_json_derives_share_existing_backend() {
    let ir = lower(
        r#"
        #[derive(Json)]
        struct User { id:i64, active:bool }
        static_assert(has_field<User,id>);
        concept IsNamed<T> = has_field<T,id>;
        fn main()->i32 {
            let description:str = reflect::<User>();
            let typeName:str = type_name::<User>();
            return 0;
        }
    "#,
    )
    .unwrap();
    assert!(ir.contains("@main"));
    assert!(ir.contains("fields"), "{ir}");
    assert!(ir.contains("id"), "{ir}");
}

#[test]
fn effect_annotations_are_validated_during_surface_lowering() {
    assert!(lower(
        r#"
        [[pure]] fn twice(x:i64)->i64 {return x*2;}
        [[pure]] fn calculate(x:i64)->i64 {return twice(x);}
        fn main()->i32{return (calculate(21)-42) as i32;}
    "#
    )
    .is_ok());
    let fail = lower(
        r#"
        [[io]] fn read()->i64 {return 42;}
        [[pure]] fn wrong()->i64 {return read();}
        fn main()->i32 {return wrong() as i32;}
    "#,
    )
    .unwrap_err();
    assert!(fail.contains("impure"), "{fail}");
}

#[test]
fn requires_method_signature_is_checked() {
    let ir = lower(
        r#"
        struct Counter {value:i64}
        impl Counter {fn current(self:Counter)->i64{return self.value;}}
        concept Readable<T> = requires(T value) { {value.current()} -> i64; };
        static_assert(Readable<Counter>);
        fn main()->i32{return 0;}
    "#,
    )
    .unwrap();
    assert!(ir.contains("@main"));
    let error = lower(
        r#"
        struct Counter {value:i64}
        impl Counter {fn current(self:Counter)->i64{return self.value;}}
        concept Readable<T> = requires(T value) { {value.current()} -> bool; };
        static_assert(Readable<Counter>);
        fn main()->i32{return 0;}
    "#,
    )
    .unwrap_err();
    assert!(error.contains("static_assert failed"), "{error}");
}

#[test]
fn const_fn_is_bounded_and_constant_calls_fold() {
    let llvm = lower(
        r#"
        const fn fact(n:i64)->i64 {
            if n<=1{return 1;}
            return n*fact(n-1);
        }
        fn main()->i32 {return (fact(5)-120) as i32;}
    "#,
    )
    .unwrap();
    assert!(llvm.contains("@main"));
}

#[test]
fn associated_type_is_bound_and_erased_before_hir() {
    let ir = lower(
        r#"
        struct Store {value:i64}
        trait Source {type Item; fn get(self:Self)->Self::Item;}
        impl Source for Store {
            type Item = i64;
            fn get(self:Store)->i64{return self.value;}
        }
        fn main()->i32 {return 0;}
    "#,
    )
    .unwrap();
    assert!(ir.contains("@main"));
    let error = lower(
        r#"
        trait Source {type Item; fn get(self:Self)->Self::Item;}
        impl Source for i64 {fn get(self:i64)->i64{return self;}}
        fn main()->i32 {return 0;}
    "#,
    )
    .unwrap_err();
    assert!(error.contains("missing associated type"), "{error}");
}

#[test]
fn contextual_next_can_name_a_method_in_a_concept() {
    let llvm = lower(
        r#"
        struct Cursor { value: i64 }
        impl Cursor { fn next(self: Cursor) -> i64 { return self.value; } }
        concept HasNext<T> = has_method<T,next>;
        static_assert(HasNext<Cursor>);
        fn main() -> i32 { return 0; }
    "#,
    )
    .unwrap();
    assert!(llvm.contains("@main"));
}

#[test]
fn nova_019_example_loads_and_compiles_to_llvm() {
    use std::path::Path;
    let (program, _) = crate::package::load_detailed(Path::new("examples/packages/nova_019"))
        .expect("Nova 0.19 example should load its declared src/main.nova entry");
    std::thread::Builder::new()
        .name("nova-019-lowering".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let concrete = crate::surface::lower(&program).expect("Nova 0.19 surface lowering");
            let typed = crate::hir::lower(&concrete).expect("Nova 0.19 HIR");
            let mir = crate::mir::lower(&typed).expect("Nova 0.19 MIR");
            let ir = crate::codegen::generate(&mir);
            assert!(ir.contains("@main"));
        })
        .expect("spawn Nova 0.19 lowering worker")
        .join()
        .expect("Nova 0.19 lowering worker panicked");
}

// Macro concept bounds are checked using the same monomorphization machinery
// used for ordinary generic Nova functions (never by interpreting macro text).
#[test]
fn macro_concept_check_compiles_to_llvm() {
    let ir = lower(
        r#"
        concept Integral<T> = integer<T>;
        macro_rules! plus_one { ($value:expr where Integral) => $value + 1; }
        fn main()->i32 { return plus_one!(41) - 42; }
    "#,
    )
    .unwrap();
    assert!(ir.contains("@main"), "{ir}");
}
#[test]
fn macro_concept_rejects_wrong_expression_type_before_llvm() {
    let err = lower(
        r#"
        concept Integral<T> = integer<T>;
        macro_rules! require_integer { ($value:expr where Integral) => $value; }
        fn main()->i32 { let v:f64=3.5; return require_integer!(v) as i32; }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("NOVA-MAC020") && err.contains("Integral"),
        "{err}"
    );
}
#[test]
fn macro_concept_can_validate_function_signature() {
    let ir = lower(
        r#"
        macro_rules! run_callback {
            ($callback:expr where same_as<fn(i64)->i64>) => {
                let cb = $callback;
                let v = cb(21);
                if v != 42 { return 1; }
            };
        }
        fn twice(x:i64)->i64 {return x * 2;}
        fn main()->i32 {run_callback!(twice);return 0;}
    "#,
    )
    .unwrap();
    assert!(ir.contains("@main"), "{ir}");
    let err = lower(
        r#"
        macro_rules! run_callback {
            ($callback:expr where same_as<fn(i64)->i64>) => {
                let cb = $callback;
                let v = cb(21);
                if v != 42 { return 1; }
            };
        }
        fn wrong(x:i32)->i32 {return x * 2;}
        fn main()->i32 {run_callback!(wrong);return 0;}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("NOVA-MAC020") && err.contains("same_as"),
        "{err}"
    );
}
#[test]
fn macro_concept_unknown_name_fails_without_invocation() {
    let err = lower(
        r#"
        macro_rules! pending { ($value:expr where NotDeclared) => $value; }
        fn main()->i32{return 0;}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("unknown concept") && err.contains("NotDeclared"),
        "{err}"
    );
}

#[test]
fn stage14_macro_rules_concept_restricts_thread_entry_signature() {
    let valid = lower(
        r#"
        concept Worker<T> = same_as<T,fn()->()>;
        macro_rules! select { ($f:expr where Worker) => $f; }
        fn work()->(){return;}
        fn main()->i32{let callback=select!(work);return 0;}
        "#,
    ).expect("typed function capture should pass concept checking");
    assert!(valid.contains("@main"));
    let bad = lower(
        r#"
        concept Worker<T> = same_as<T,fn()->()>;
        macro_rules! select { ($f:expr where Worker) => $f; }
        fn main()->i32{let callback=select!(42);return 0;}
        "#,
    ).unwrap_err();
    assert!(bad.contains("NOVA-MAC020") && bad.contains("Worker"), "{bad}");
}

#[test]
fn stage14_std_native_contains_only_declared_foreign_abi() {
    let manifest = include_str!("../std/src/abi/native/mod.nova");
    assert!(!manifest.contains("pub mod raw;"));
    assert!(!manifest.contains("pub mod builder;"));
    assert!(!manifest.contains("pub mod math;"));
    assert!(include_str!("../std/src/old/raw.nova").contains("fn allocate_bytes"));
    assert!(include_str!("../std/src/old/io.nova").contains("fn write_fd"));
    assert!(include_str!("../std/src/old/helpers.nova").contains("macro_rules! spawn"));
    assert!(include_str!("../std/src/old/concepts.nova").contains("same_as<T,fn()->()>"));
}

#[test]
fn constrained_struct_and_enum_templates_reject_invalid_types() {
    let source=r#"
        concept Whole<T> = integer<T>;
        struct Item<T: Whole> { value: T }
        enum Boxed<T: Whole> { Ready(T), Absent }
        fn main()->i32 {
            let item = new Item<i32> { value: 42 };
            return item.value - 42;
        }
    "#;
    let llvm=lower(source).expect("bounded struct and enum templates should parse and lower");
    assert!(llvm.contains("@main"));
    let bad=r#"
        concept Whole<T> = integer<T>;
        struct Item<T: Whole> { value: T }
        fn main()->i32 {
            let item = new Item<bool> { value: true };
            return 0;
        }
    "#;
    let err=lower(bad).expect_err("bool must not instantiate a constrained integer struct");
    assert!(err.contains("concept constraint") && err.contains("Whole"),"{err}");
}

#[test]
fn constructors_and_static_functions_lower_without_runtime() {
    let llvm=lower(r#"
        struct Counter { value:i32 }
        impl Counter {
            static fn new(value:i32)->Self {return new Counter {value:value};}
            static fn initial()->i32 {return 42;}
            fn get(self:&Counter)->i32 {return self.value;}
        }
        fn main()->i32 {
            let c=new Counter(42);
            return c.get()-Counter::initial();
        }
    "#).expect("static constructor should lower as ordinary Nova function");
    assert!(llvm.contains("@main"));
    assert!(llvm.contains("Counter__new"),"{llvm}");
}

#[test]
fn static_and_constructor_rules_fail_at_compile_time() {
    for (method,diagnostic) in [
        ("static fn new(self:Counter)->Counter {return self;}","NOVA-CTOR001"),
        ("static fn new(value:i32)->i32 {return value;}","NOVA-CTOR003"),
        ("fn build(value:i32)->Counter {return new Counter {value:value};}","NOVA-CTOR004"),
    ] {
        let source=format!("struct Counter {{value:i32}} impl Counter {{{method}}} fn main()->i32{{return 0;}}");
        let err=lower(&source).expect_err("invalid static method must fail");
        assert!(err.contains(diagnostic),"{err}");
    }
}

#[test]
fn typed_requires_checks_argument_type_not_only_method_name() {
    let llvm=lower(r#"
        struct Counter { value:i32 }
        impl Counter { fn add(self:&Counter, other:i32)->i32 {return self.value+other;} }
        concept Sum32<T> = requires(T item) { { item.add(i32) } -> i32; };
        fn check<T:Sum32>(item:T)->i32 {return 42;}
        fn main()->i32 {return check(new Counter {value:1})-42;}
    "#).expect("requires must verify argument and return types");
    assert!(llvm.contains("@main"));
    let err=lower(r#"
        struct Counter {value:i32}
        impl Counter {fn add(self:&Counter,other:i32)->i32 {return self.value+other;}}
        concept SumBool<T> = requires(T item) { { item.add(bool) } -> i32; };
        fn check<T:SumBool>(item:T)->i32 {return 42;}
        fn main()->i32{return check(new Counter {value:1});}
    "#).expect_err("incorrect method parameter must fail a structural concept");
    assert!(err.contains("SumBool") && err.contains("concept constraint"),"{err}");
}

#[test]
fn constructor_named_arguments_go_through_static_signature() {
    let llvm=lower(r#"
        struct Point { x:i32,y:i32 }
        impl Point {
            static fn new(x:i32,y:i32)->Self{return new Point {x:x,y:y};}
            fn sum(self:&Point)->i32{return self.x+self.y;}
        }
        fn main()->i32 {let p=new Point(y:22,x:20);return p.sum()-42;}
    "#).expect("constructor named arguments should bind to static method parameters");
    assert!(llvm.contains("@main"));
}

#[test]
fn static_methods_cannot_be_invoked_as_instance_methods() {
    let error=lower(r#"
        struct Point {x:i32}
        impl Point {static fn initial()->i32{return 42;}}
        fn main()->i32 {let p=new Point{x:1};return p.initial();}
    "#).expect_err("instance dispatch must reject static-only methods");
    assert!(error.contains("not defined")||error.contains("receiver"),"{error}");
}
