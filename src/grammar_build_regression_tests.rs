//! Focused parser regressions for the NB/annotation syntax that previously
//! caused excessive LALRPOP regeneration and ambiguous method prefixes.
use crate::{ast::Item, grammar};

fn parse(source: &str) -> crate::ast::Program {
    grammar::ProgramParser::new()
        .parse(source)
        .unwrap_or_else(|error| panic!("failed to parse Nova source: {error:?}\n{source}"))
}

#[test]
fn nb_imports_marker_annotations_and_class_entry() {
    let ast = parse(r#"
        use NB::*;
        use nexo::request::Request;
        use nexo::response::Response;
        @BootApplication
        class DB {}
        impl DB {
            static fn main() -> i32 { return 0; }
        }
    "#);
    assert_eq!(ast.items.len(), 5);
    assert!(matches!(&ast.items[0], Item::Use { path, .. } if path == "NB::*"));
    assert!(matches!(&ast.items[3], Item::Struct(ty) if ty.attributes.iter().any(|a| a.name == "BootApplication")));
}

#[test]
fn static_methods_support_both_annotation_orders() {
    let ast = parse(r#"
        @RestController(path = "/api")
        class Controller {}
        impl Controller {
            @GetMapping("/users/{id}")
            static fn find(req: Request, res: Response) -> i32 { return 0; }
            static @PostMapping("/users")
            fn create(req: Request, res: Response) -> i32 { return 1; }
            static fn health() -> i32 { return 200; }
        }
    "#);
    let Some(Item::Impl(block)) = ast.items.get(1) else { panic!("missing impl"); };
    assert_eq!(block.methods.len(), 3);
    for method in &block.methods {
        assert!(method.attributes.iter().any(|a| a.name == "static"), "{}", method.name);
    }
    assert!(block.methods[0].attributes.iter().any(|a| a.name == "GetMapping"));
    assert!(block.methods[1].attributes.iter().any(|a| a.name == "PostMapping"));
}

#[test]
fn out_let_and_comptime_annotations_remain_parseable() {
    let ast = parse(r#"
        comptime fn Above(x: i64, y: i64) -> bool { return x > y; }
        class Product { price: @Above(y = 10) i64 }
        fn generate(out amount: i64) -> bool { amount = 42; return true; }
        fn check() -> i64 {
            let ok = generate(out let value);
            if ok { return value; }
            return 0;
        }
    "#);
    assert_eq!(ast.items.len(), 4);
}

#[test]
fn dyn_trait_types_are_accepted_in_function_signatures() {
    use crate::ast::{Item, TypeRef};
    let ast = parse(r#"
        trait Display { fn show(self: Self) -> str; }
        fn render(value: &dyn Display) -> str { return ""; }
        fn consume(value: dyn Display) -> () { return; }
    "#);
    let Some(Item::Function(render)) = ast.items.get(1) else { panic!("missing render"); };
    let TypeRef::Reference { inner, .. } = &render.params[0].ty else { panic!("expected borrowed dyn trait"); };
    assert_eq!(inner.as_ref(), &TypeRef::Named("dyn Display".into()));
    let Some(Item::Function(consume)) = ast.items.get(2) else { panic!("missing consume"); };
    assert_eq!(consume.params[0].ty, TypeRef::Named("dyn Display".into()));
}

#[test]
fn impl_trait_is_accepted_as_a_return_type() {
    use crate::ast::{Item, TypeRef};
    let ast = parse(r#"
        trait Display { fn show(self: Self) -> str; }
        fn render() -> impl Display { return 1; }
    "#);
    let Some(Item::Function(render)) = ast.items.get(1) else { panic!("missing render"); };
    assert_eq!(render.result, TypeRef::Named("impl Display".into()));
}

// A compact closure is an expression form, not an atom. Its body must own
// operators to the right of the closing `|` (LALRPOP ambiguity regression).
#[test]
fn compact_closure_body_respects_binary_precedence() {
    use crate::ast::{BinaryOp, Expr, Stmt};
    let ast = parse(r#"
        fn main() -> i32 {
            let add = |x: i32| x + 1 * 2;
            let check = |x: i32| x > 0 && x < 100;
            let flags = |x: i32| x & 3 | 1 ^ 2;
            let diff = |x: i32| x - 3;
            return 0;
        }
    "#);
    let Some(Item::Function(function)) = ast.items.first() else { panic!("missing main"); };
    for stmt in &function.body[..4] {
        let Stmt::Let { init:Expr::Lambda {body,..}, .. } = stmt else {
            panic!("expected an expression-bodied closure, got {stmt:?}");
        };
        assert!(matches!(body.as_slice(), [Stmt::Return(Some(Expr::Binary(..)))]));
    }
    let Stmt::Let {init:Expr::Lambda {body,..},..} = &function.body[0] else { unreachable!() };
    assert!(matches!(body.as_slice(),[Stmt::Return(Some(Expr::Binary(_,BinaryOp::Add,_)))]));
    let Stmt::Let {init:Expr::Lambda {body,..},..} = &function.body[1] else { unreachable!() };
    assert!(matches!(body.as_slice(),[Stmt::Return(Some(Expr::Binary(_,BinaryOp::And,_)))]));
    let Stmt::Let {init:Expr::Lambda {body,..},..} = &function.body[2] else { unreachable!() };
    assert!(matches!(body.as_slice(),[Stmt::Return(Some(Expr::Binary(_,BinaryOp::BitOr,_)))]));
}

#[test]
fn compact_closures_parse_in_calls_arrays_and_nested_bodies() {
    let ast = parse(r#"
        fn invoke(call: fn(i32)->i32) -> i32 {return 1;}
        fn main() -> i32 {
            let outer = |x: i32| |y: i32| x + y;
            let normal = (|x: i32| x + 1);
            let typed = |x: i32| -> i32 { return x * 2; };
            let n = invoke(|x: i32| x + 10);
            return n;
        }
    "#);
    assert_eq!(ast.items.len(), 2);
}

// Regression for the LALRPOP conflicts reported after compact closures were
// moved to expression precedence. Lambda parameters cannot use defaults:
// otherwise a closing `|` is also a candidate bitwise-or in `= Expr`.
#[test]
fn compact_lambda_delimiter_does_not_compete_with_parameter_defaults() {
    use crate::ast::{Expr, Stmt};
    let ast = parse(r#"
        fn default_in_named_function(x: i64 = 10) -> i64 { return x; }
        fn main() -> i64 {
            let mask = |x: i64| x | 3 ^ 1;
            let check = |x: i64| x >= 1 && x != 10;
            let sum = |x: i64| x + 1 * 2;
            return 0;
        }
    "#);
    assert_eq!(ast.items.len(), 2);
    let Item::Function(main) = &ast.items[1] else { panic!("expected main"); };
    for statement in &main.body[..3] {
        let Stmt::Let {init: Expr::Lambda {params, body, ..}, ..} = statement else {
            panic!("expected compact closure: {statement:?}");
        };
        assert_eq!(params.len(), 1);
        assert!(params[0].default.is_none());
        assert!(matches!(body.as_slice(), [Stmt::Return(Some(Expr::Binary(..)))]));
    }
}

#[test]
fn qualified_paths_remain_parseable_in_types_calls_and_nb_imports() {
    let ast = parse(r#"
        use NB::*;
        use nexo::app::App;
        fn launch(app: &nexo::app::App) -> i64 {
            let id = std::abi::native::task::native_spawn;
            return id;
        }
    "#);
    assert_eq!(ast.items.len(), 3);
    assert!(matches!(&ast.items[0], Item::Use{path, ..} if path == "NB::*"));
    assert!(matches!(&ast.items[1], Item::Use{path, ..} if path == "nexo::app::App"));
}


// Regression for conflicts in nullable angle-bracket type arguments (reported
// at grammar.lalrpop:98 and :265 in LALRPOP 0.22.2).
#[test]
fn generic_parameters_and_macro_concept_arguments_parse() {
    let ast = parse(r#"
        macro_rules! checked { ($value:expr where same_as<i64>) => $value; }
        fn identity<T>(value: T) -> T { return value; }
        fn pair<T, U>(left: T, right: U) -> U { return right; }
        fn bounded<T: Integral, U>(left: T, right: U) -> U { return right; }
        struct Container<T> { value: T }
    "#);
    assert_eq!(ast.items.len(), 5);
}

#[test]
fn empty_normal_function_argument_and_function_type_lists_still_parse() {
    let ast = parse(r#"
        fn execute(callback: fn() -> i32) -> i32 { return callback(); }
        fn empty() -> i32 { return 0; }
    "#);
    assert_eq!(ast.items.len(), 2);
}

// An annotated field's '(' must bind to @Range(...) rather than be read as
// the beginning of the unit type. Markers and qualified annotations must
// still work, while undecorated unit fields remain valid.
#[test]
fn annotation_arguments_and_field_types_are_unambiguous() {
    let ast = parse(r#"
        use NB::*;
        struct Limits {
            age: @Int(min = 10, max = 20) i64,
            score: @NB::Positive i64,
            name: @NotEmpty str,
            callback: @Required fn() -> i32,
            empty: ()
        }
    "#);
    let Some(Item::Struct(ty)) = ast.items.get(1) else { panic!("missing Limits"); };
    assert_eq!(ty.fields.len(), 5);
    assert_eq!(ty.fields[0].attributes[0].name, "Int");
    assert_eq!(ty.fields[0].attributes[0].args, vec!["min=10".to_string(), "max=20".to_string()]);
    assert_eq!(ty.fields[1].attributes[0].name, "NB::Positive");
    assert_eq!(ty.fields[4].attributes.len(), 0);
}

// The declaration header contains the optional generic suffix, but the
// nonterminal representing the suffix itself must never be nullable.
#[test]
fn generic_declaration_headers_are_unambiguous() {
    let ast = parse(r#"
        class DB {}
        struct Box<T> { value: T }
        struct Pair<T: Integral, U> { left: T, right: U }
        enum Maybe<T> { Some(T), None }
        fn identity<T>(value: T) -> T { return value; }
        fn plain() -> i64 { return 0; }
        impl<T> Box<T> {
            fn get(self: &Self) -> T { return self.value; }
        }
    "#);
    assert_eq!(ast.items.len(), 7);
    let Item::Struct(pair) = &ast.items[2] else { panic!("expected Pair"); };
    assert_eq!(pair.generics, vec!["T".to_string(), "U".to_string()]);
    assert_eq!(pair.concept_bounds.len(), 1);
}

// The property accessor token `get` must not make ordinary method calls
// illegal. This broke std::old::string, Nexo and NB package-loading tests.
#[test]
fn property_get_keyword_is_allowed_as_member_name() {
    let ast = parse(r#"
        struct Vector { value: i64 }
        impl Vector {
            fn get(self: &Self, index: i64) -> i64 {
                return self.value + index;
            }
        }
        fn main() -> i64 {
            let v = new Vector { value: 41 };
            return v.get(1);
        }
    "#);
    assert_eq!(ast.items.len(), 3);
}

// `scope` starts a lexical statement but must also work as a named
// attribute parameter, without broadening the expression identifier set.
#[test]
fn nb_component_scope_named_annotation_argument() {
    let ast = parse(r#"
        @Service(scope = "singleton") class Service {}
        @Service(scope = "prototype") class Transient {}
        @RestController(path = "/api") class Controller {}
    "#);
    assert_eq!(ast.items.len(), 3);
    let Item::Struct(service) = &ast.items[0] else { panic!("expected class"); };
    assert_eq!(service.attributes[0].args, vec!["scope=\"singleton\"".to_string()]);
}

// Global `get` calls and concept member names must coexist with property syntax.
#[test]
fn get_contextual_keyword_as_function_and_concept_member() {
    let ast = parse(r#"
        concept Readable<T> = has_method<T,get>;
        fn get(value: i64) -> i64 { return value; }
        fn main() -> i64 { return get(42); }
    "#);
    assert_eq!(ast.items.len(),3);
}

#[test]
fn std_raw_pointer_store_uses_evaluated_local_destination() {
    parse(r#"
        fn sample() -> i32 {
            unsafe {
                let data: *mut u8 = std::old::raw::alloc_elements::<u8>(4);
                let slot: *mut u8 = std::old::raw::element_at::<u8>(data,1);
                *slot = 3;
            }
            return 0;
        }
    "#);
}

// `with` has two grammatical roles: a capability clause and a method name.
// Keep the latter contextual instead of accepting statement keywords as Ident.
#[test]
fn capability_with_keyword_remains_available_as_a_generic_method_name() {
    let ast = parse(r#"
        struct Box<T> { item: T }
        impl<T> Box<T> {
            fn with<R>(self: &Box<T>, callback: fn(&T)->R) -> R {
                return callback(&self.item);
            }
        }
        fn ordinary() -> i64 { return 1; }
    "#);
    assert_eq!(ast.items.len(), 3);
    let Some(Item::Impl(block)) = ast.items.get(1) else { panic!("missing Box impl"); };
    assert_eq!(block.methods[0].name, "with");
}

// Complex lvalue targets should evaluate to a local pointer before storing;
// this is the currently supported and type-checkable Nova assignment form.
#[test]
fn pointer_fields_can_be_assigned_through_typed_local_places() {
    parse(r#"
        struct Holder { pointer: *mut i64 }
        impl Holder {
            fn set(self: &Holder, value: i64) -> () {
                unsafe {
                    let destination: *mut i64 = self.pointer;
                    *destination = value;
                }
                return;
            }
        }
    "#);
}

// Scientific notation must lex as a single float token. Previously e308 was
// tokenized as an identifier after a decimal literal, breaking std::old::json.
#[test]
fn scientific_float_literals_are_single_tokens() {
    use crate::ast::{Expr, Stmt};
    let ast = parse(r#"
        fn numbers() -> i64 {
            let maximum: f64 = 1.7976931348623157e308;
            let tiny: f64 = 2.5e-10;
            let large: f64 = 6.0E+8;
            let whole_mantissa: f64 = 9e3;
            let plain: f64 = 1.25;
            return 0;
        }
    "#);
    let Some(Item::Function(function)) = ast.items.first() else { panic!("missing numbers"); };
    let expected = [
        "1.7976931348623157e308", "2.5e-10", "6.0E+8", "9e3", "1.25"
    ];
    for (statement, expected_text) in function.body.iter().zip(expected) {
        let Stmt::Let {init: Expr::Float(actual), ..} = statement else {
            panic!("expected a float literal, got {statement:?}");
        };
        assert_eq!(actual, expected_text);
    }
}

// Generic static constructors are written using Nova's existing `new` syntax.
// The old demo used `Box<T>::new` which is not accepted by the LR(1) parser:
// a bare `<` after an expression also begins a comparison. The explicit
// `new Box<T>(...)` syntax is unambiguous and lowers to Box::new::<T>(...).
#[test]
fn nb_generic_constructor_sugar_is_a_typed_generic_call() {
    use crate::ast::{Expr, Stmt, TypeRef};
    let ast = parse(r#"
        fn demo() -> i64 {
            let one = new beans::Singleton<Count>(Count::new());
            let two = new beans::Prototype<Count>(Count::new);
            return 0;
        }
    "#);
    let Some(Item::Function(function)) = ast.items.first() else { panic!("missing demo"); };
    let [Stmt::Let {init:first,..},Stmt::Let {init:second,..},..] = function.body.as_slice() else {
        panic!("missing generic constructors");
    };
    assert!(matches!(first,Expr::GenericCall(name,types,args)
        if name=="beans::Singleton::new" && types==&vec![TypeRef::Named("Count".into())] && args.len()==1));
    assert!(matches!(second,Expr::GenericCall(name,types,args)
        if name=="beans::Prototype::new" && types==&vec![TypeRef::Named("Count".into())] && args.len()==1));
}


// The Nova AST currently accepts assignments to a variable or its immediate
// field. Raw-backed owner state must be updated through a typed local pointer;
// this also preserves HIR's unsafe checks for writes through *mut.
#[test]
fn raw_backed_owner_field_updates_use_local_pointer_lvalues() {
    let ast = parse(r#"
        struct Header {length: i64}
        struct Owner {state: *mut Header}
        impl Owner {
            fn clear(self: &Owner) -> () {
                unsafe {
                    let header: *mut Header = self.state;
                    header.length = 0;
                }
                return;
            }
        }
        fn main() -> i32 {return 0;}
    "#);
    let typed = crate::surface::lower(&ast)
        .expect("typed local pointer field assignment must pass surface lowering");
    crate::hir::lower(&typed)
        .expect("typed pointer write within unsafe must pass HIR lowering");
}

// NexoFrame defines get! and other public macros with contextual keyword
// names. Those spellings are allowed as members, not as local identifiers.
#[test]
fn contextual_member_names_are_valid_macro_rule_names() {
    let parsed = parse(r#"
        pub macro_rules! get { ($value_path:expr) => $value_path; }
        pub macro_rules! next { ($value:expr) => $value; }
        fn use_macros() -> i32 {return pkg::get!(9);}
    "#);
    assert_eq!(parsed.items.len(), 3);
}


#[test]
fn try_expression_and_try_blocks_parse() {
    use crate::ast::{Expr, Item, Stmt};
    let ast = parse(r#"
        fn fetch() -> Result<i32, i32> { return Ok(42); }
        fn load() -> Result<i32, i32> {
            let value = try fetch();
            return Ok(value);
        }
        fn load_block() -> Result<i32, i32> {
            let value = try { fetch() };
            return Ok(value);
        }
        fn recover() -> i32 {
            let value = try { fetch() } catch error { 0 };
            return value;
        }
    "#);
    let functions: Vec<_> = ast.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some(function),
        _ => None,
    }).collect();
    assert_eq!(functions.len(), 4);
    assert!(matches!(
        &functions[1].body[0],
        Stmt::Let { init: Expr::Call(name, _), .. } if name == "__nova_internal_try"
    ));
    assert!(matches!(
        &functions[2].body[0],
        Stmt::Let { init: Expr::Call(name, _), .. } if name == "__nova_internal_try"
    ));
    assert!(matches!(
        &functions[3].body[0],
        Stmt::Let { init: Expr::Block(_), .. }
    ));
    crate::ergonomic_lowering::lower(&ast)
        .expect("try propagation and catch should lower through Result control flow");
}

#[test]
fn try_catch_supports_error_variant_patterns() {
    let ast = parse(r#"
        enum UserError { NotFound, Blocked(i32), Unavailable }
        fn fetch() throws UserError -> i32 { throw UserError::Blocked(403); }
        fn recover() -> i32 {
            let value = try { fetch() } catch {
                UserError::NotFound => { 0 },
                UserError::Blocked(code) => { code },
                _ => { 2 }
            };
            return value;
        }
    "#);
    let lowered = crate::ergonomic_lowering::lower(&ast)
        .expect("pattern-based catch should lower into nested Result/error matches");
    let rendered = format!("{lowered:?}");
    assert!(rendered.contains("__nova_try_error"), "{rendered}");
    assert!(rendered.contains("Blocked"), "{rendered}");
}

#[test]
fn try_statement_block_parses() {
    use crate::ast::{Expr, Item, Stmt};
    let ast = parse(r#"
        fn run() -> Result<i32, i32> {
            try { Ok(42) };
            return Ok(0);
        }
    "#);
    let Some(Item::Function(function)) = ast.items.first() else { panic!("missing function"); };
    assert!(matches!(
        &function.body[0],
        Stmt::Expr(Expr::Call(name, _)) if name == "__nova_internal_try"
    ));
}


#[test]
fn throws_contract_and_throw_statement_lower_to_result() {
    use crate::ast::{Expr, Item, Stmt, TypeRef};
    let ast = parse(r#"
        fn fail() throws i32 -> i32 {
            throw 7;
        }
        fn caller() throws i32 -> i32 {
            let value = try fail();
            return Ok(value);
        }
    "#);
    let functions: Vec<_> = ast.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some(function),
        _ => None,
    }).collect();
    assert_eq!(functions.len(), 2);
    assert_eq!(
        functions[0].result,
        TypeRef::Applied("std::error::result::Result".into(), vec![
            TypeRef::Named("i32".into()), TypeRef::Named("i32".into())
        ])
    );
    assert!(matches!(
        &functions[0].body[0],
        Stmt::Return(Some(Expr::Call(name, args))) if name == "Err" && args.len() == 1
    ));
    crate::ergonomic_lowering::lower(&ast)
        .expect("typed throw and try should lower through Result control flow");
}
