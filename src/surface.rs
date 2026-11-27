//! Surface-language lowering: namespace resolution, monomorphization, impl dispatch,
//! tagged enums, match exhaustiveness, and built-in derives. The typed HIR and MIR
//! only receive the small, fully specialized core AST.
use crate::ast::{self, BinaryOp, Expr, Item, Pattern, Stmt, TypeRef};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct StructDef {
    decl: ast::Struct,
    namespace: String,
}
#[derive(Clone)]
struct EnumDef {
    decl: ast::Enum,
    namespace: String,
}
#[derive(Clone)]
struct FuncDef {
    decl: ast::Function,
    namespace: String,
    impl_generics: Vec<String>,
}
#[derive(Clone)]
struct EnumMeta {
    name: String,
    variants: Vec<(String, Vec<(String, TypeRef)>)>,
}
#[derive(Clone)]
struct Scope {
    vars: HashMap<String, TypeRef>,
    namespace: String,
    subst: HashMap<String, TypeRef>,
    returns: TypeRef,
    constructor_base: Option<TypeRef>,
}

pub fn lower(input: &ast::Program) -> Result<ast::Program, String> {
    lower_with_helpers(input, crate::compile_helpers::HelperMode::Macro)
}
pub fn lower_with_helpers(
    input: &ast::Program,
    helper_mode: crate::compile_helpers::HelperMode,
) -> Result<ast::Program, String> {
    lower_impl(input, helper_mode, None)
}
/// Collect errors from independent concrete instantiations before stopping.
/// We cannot safely continue to HIR after failed type materialization.
pub fn lower_with_helpers_all_errors(
    input: &ast::Program,
    helper_mode: crate::compile_helpers::HelperMode,
) -> Result<ast::Program, Vec<String>> {
    let mut errors = Vec::new();
    lower_impl(input, helper_mode, Some(&mut errors)).map_err(|error| {
        if errors.is_empty() {
            vec![error]
        } else {
            errors
        }
    })
}
fn lower_impl(
    input: &ast::Program,
    helper_mode: crate::compile_helpers::HelperMode,
    mut collected: Option<&mut Vec<String>>,
) -> Result<ast::Program, String> {
    let mut engine = Engine::default();
    let helpers = crate::compile_helpers::lower(input, helper_mode);
    let expanded = crate::proc_macros::expand(&helpers)?;
    let expanded = crate::macros::expand(&expanded)?;
    let expanded = crate::ergonomic_lowering::lower(&expanded)?;
    let expanded = crate::concurrency::lower_and_check(&expanded)?;
    let expanded = crate::call_arguments::lower(&expanded)?;
    let expanded = crate::out_parameters::lower(&expanded)?;
    // Derive aliases live in .nova metadata and expand to supported derives
    // before type materialization. The transformation is deterministic and
    // cannot bypass the existing derive/ownership validation.
    let expanded = crate::meta::expand(&expanded)?;
    crate::meta::validate(&expanded)?;
    crate::effects::validate(&expanded)?;
    let expanded = crate::consteval::fold(&expanded)?;
    // Materialize dyn-trait forwarding functions while trait declarations are
    // still present; type_features lowers/removes those declarations later.
    let mut expanded = expanded;
    materialize_dyn_methods(&mut expanded.items, "");
    engine.concepts = crate::concepts::Registry::collect(&expanded)?;
    crate::meta::Registry::collect(&expanded)?
        .check_derive_contracts(&expanded, &engine.concepts)?;
    let expanded = crate::type_features::lower(&expanded)?;
    let expanded = crate::iterators::desugar(&expanded);
    let expanded = crate::lambdas::lift(expanded)?;
    let expanded = crate::coroutines::desugar(&expanded)?;
    engine.collect(&expanded.items, "")?;
    engine.prepare_inheritance()?;
    let concrete = engine
        .funcs
        .keys()
        .filter(|name| {
            !engine.funcs[*name].decl.variadic
                && engine.funcs[*name].decl.generics.is_empty()
                && engine.funcs[*name].impl_generics.is_empty()
        })
        .cloned()
        .collect::<Vec<_>>();
    // Concrete types may be referenced by fields before a function uses them.
    let concrete_types = engine
        .structs
        .keys()
        .chain(engine.enums.keys())
        .filter(|name| {
            let g = engine
                .structs
                .get(*name)
                .map(|x| &x.decl.generics)
                .or_else(|| engine.enums.get(*name).map(|x| &x.decl.generics))
                .unwrap();
            g.is_empty()
        })
        .cloned()
        .collect::<Vec<_>>();
    // Stable order makes diagnostics reproducible across compiler invocations.
    let mut concrete_types = concrete_types;
    let mut concrete = concrete;
    if collected.is_some() {
        concrete_types.sort();
        concrete.sort();
    }
    for name in concrete_types {
        if let Err(error) = engine.instantiate_type(&name, &[]) {
            if let Some(errors) = collected.as_deref_mut() {
                errors.push(format!("type '{name}': {error}"));
                if errors.len() >= 64 {
                    break;
                }
            } else {
                return Err(error);
            }
        }
    }
    for name in concrete {
        if collected.as_ref().is_some_and(|errors| errors.len() >= 64) {
            break;
        }
        if let Err(error) = engine.instantiate_function(&name, &[], None) {
            if let Some(errors) = collected.as_deref_mut() {
                errors.push(error);
            } else {
                return Err(error);
            }
        }
    }
    for (ns, path) in &engine.imports {
        let q = join(ns, path);
        let found = engine
            .funcs
            .keys()
            .chain(engine.structs.keys())
            .chain(engine.enums.keys())
            .chain(engine.externs.keys())
            .any(|name| name.starts_with(&format!("{q}::")) || name == &q)
            || engine
                .funcs
                .keys()
                .chain(engine.structs.keys())
                .chain(engine.enums.keys())
                .any(|name| name.starts_with(&format!("{path}::")));
        if !found {
            let error = format!("import '{path}' cannot be resolved");
            if let Some(errors) = collected.as_deref_mut() {
                if errors.len() < 64 {
                    errors.push(error);
                }
            } else {
                return Err(error);
            }
        }
    }
    if let Some(errors) = collected {
        if !errors.is_empty() {
            // Never lower an incomplete or invalid program into HIR/LLVM.
            return Err(format!("{} independent surface error(s)", errors.len()));
        }
    }
    // Surface lowering owns pointer arithmetic lowering, so coroutine
    // desugaring cannot discover this intrinsic early enough to synthesize
    // its declaration. Keep the compiler-only ABI contract explicit in the
    // typed input even when the current program does not use it; codegen
    // recognizes the symbol and emits inline GEP instead of an external call.
    let compiler_intrinsics = [
        (
            "__nova_internal_byte_offset",
            vec![("address", "u64"), ("bytes", "u64")],
            "u64",
        ),
        (
            "__nova_internal_copy_bytes",
            vec![("destination", "u64"), ("source", "u64"), ("count", "u64")],
            "()",
        ),
        (
            "__nova_internal_move_bytes",
            vec![("destination", "u64"), ("source", "u64"), ("count", "u64")],
            "()",
        ),
        (
            "__nova_internal_set_bytes",
            vec![("destination", "u64"), ("value", "u8"), ("count", "u64")],
            "()",
        ),
        (
            "__nova_internal_heap_allocate",
            vec![("alignment", "u64"), ("size", "u64")],
            "u64",
        ),
        ("__nova_internal_heap_free", vec![("address", "u64")], "()"),
        ("__nova_internal_load_byte", vec![("address", "u64")], "u8"),
        ("__nova_internal_text_length", vec![("text", "str")], "u64"),
        ("__nova_internal_text_address", vec![("text", "str")], "u64"),
        (
            "__nova_internal_borrow_text",
            vec![("address", "u64")],
            "str",
        ),
        ("__nova_internal_sqrt_f64", vec![("value", "f64")], "f64"),
        ("__nova_internal_abs_f64", vec![("value", "f64")], "f64"),
        ("__nova_internal_floor_f64", vec![("value", "f64")], "f64"),
        ("__nova_internal_ceil_f64", vec![("value", "f64")], "f64"),
    ];
    for (name, params, result) in compiler_intrinsics {
        if engine
            .items
            .iter()
            .any(|item| matches!(item, Item::Extern(function) if function.name == name))
        {
            continue;
        }
        engine.items.push(Item::Extern(ast::ExternFunction {
            name: name.into(),
            c_variadic: false,
            // These declarations are compiler-private symbols. Public
            // safety boundaries are expressed by Nova wrappers.
            attributes: vec![],
            params: params
                .into_iter()
                .map(|(name, ty)| ast::Param {
                    name: name.into(),
                    ty: TypeRef::Named(ty.into()),
                    default: None,
                    attributes: vec![],
                })
                .collect(),
            result: TypeRef::Named(result.into()),
        }));
    }
    Ok(ast::Program {
        items: engine.items,
    })
}

#[derive(Default)]
struct Engine {
    funcs: HashMap<String, FuncDef>,
    structs: HashMap<String, StructDef>,
    enums: HashMap<String, EnumDef>,
    externs: HashMap<String, ast::ExternFunction>,
    items: Vec<Item>,
    type_cache: HashMap<String, TypeRef>,
    func_cache: HashSet<String>,
    concrete_fields: HashMap<String, Vec<ast::Field>>,
    enum_meta: HashMap<String, EnumMeta>,
    serial: HashSet<String>,
    deserial: HashSet<String>,
    defaultable: HashSet<String>,
    imports: Vec<(String, String)>,
    counter: usize,
    instance_args: HashMap<String, Vec<TypeRef>>,
    concepts: crate::concepts::Registry,
    // Generic owner destructors can instantiate methods that use `size_of<T>`.
    // Delay them until the outermost type has registered all concrete fields.
    type_instantiation_depth: usize,
    pending_destructors: Vec<(String, Vec<TypeRef>)>,
    flushing_deferred_destructors: bool,
}
fn join(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.into()
    } else {
        format!("{ns}::{name}")
    }
}
fn replace_self_with_dyn(ty: &TypeRef, trait_name: &str) -> TypeRef {
    match ty {
        TypeRef::Named(name) if name == "Self" => TypeRef::Named(format!("dyn {trait_name}")),
        TypeRef::Applied(name, args) => TypeRef::Applied(
            name.clone(),
            args.iter()
                .map(|arg| replace_self_with_dyn(arg, trait_name))
                .collect(),
        ),
        TypeRef::Array(inner, len) => {
            TypeRef::Array(Box::new(replace_self_with_dyn(inner, trait_name)), *len)
        }
        TypeRef::Pointer { mutable, inner } => TypeRef::Pointer {
            mutable: *mutable,
            inner: Box::new(replace_self_with_dyn(inner, trait_name)),
        },
        TypeRef::Reference { mutable, inner } => TypeRef::Reference {
            mutable: *mutable,
            inner: Box::new(replace_self_with_dyn(inner, trait_name)),
        },
        TypeRef::Function { params, result } => TypeRef::Function {
            params: params
                .iter()
                .map(|param| replace_self_with_dyn(param, trait_name))
                .collect(),
            result: Box::new(replace_self_with_dyn(result, trait_name)),
        },
        _ => ty.clone(),
    }
}
fn contains_associated_self_type(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Named(name) => name.starts_with("Self::"),
        TypeRef::Applied(_, args) => args.iter().any(contains_associated_self_type),
        TypeRef::Array(inner, _)
        | TypeRef::Pointer { inner, .. }
        | TypeRef::Reference { inner, .. } => contains_associated_self_type(inner),
        TypeRef::Function { params, result } => {
            params.iter().any(contains_associated_self_type)
                || contains_associated_self_type(result)
        }
    }
}
fn dyn_methods_for_trait(trait_decl: &ast::Trait, namespace: &str) -> Vec<Item> {
    let trait_name = join(namespace, &trait_decl.name);
    let trait_key = ident(&trait_name);
    trait_decl
        .methods
        .iter()
        .enumerate()
        .filter_map(|(slot, method)| {
            // Associated types require a binding such as dyn Iterator<Item=T>.
            // Until that syntax exists, these methods are not object-safe and do
            // not receive a callable vtable slot.
            if contains_associated_self_type(&method.result)
                || method
                    .params
                    .iter()
                    .any(|param| contains_associated_self_type(&param.ty))
            {
                return None;
            }
            let name = format!("__nova_dyn__{trait_key}__{slot}__{}", method.name);
            let params = method
                .params
                .iter()
                .map(|param| ast::Param {
                    name: param.name.clone(),
                    ty: replace_self_with_dyn(&param.ty, &trait_name),
                    default: None,
                    attributes: vec![],
                })
                .collect();
            let result = replace_self_with_dyn(&method.result, &trait_name);
            Some(Item::Function(ast::Function {
                mode: ast::FunctionMode::Normal,
                name,
                generics: vec![],
                concept_bounds: vec![],
                params,
                variadic: false,
                result: result.clone(),
                body: if result == TypeRef::Named("()".into()) {
                    vec![Stmt::Return(None)]
                } else {
                    vec![Stmt::Return(Some(Expr::Zero(result)))]
                },
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            }))
        })
        .collect()
}
fn materialize_dyn_methods(items: &mut Vec<Item>, namespace: &str) {
    let mut generated = Vec::new();
    for item in items.iter_mut() {
        match item {
            Item::Namespace { name, items } => {
                materialize_dyn_methods(items, &join(namespace, name))
            }
            Item::Public(inner) => match inner.as_mut() {
                Item::Trait(trait_decl) => {
                    generated.extend(dyn_methods_for_trait(trait_decl, namespace));
                }
                Item::Namespace { name, items } => {
                    materialize_dyn_methods(items, &join(namespace, name));
                }
                _ => {}
            },
            Item::Trait(trait_decl) => {
                generated.extend(dyn_methods_for_trait(trait_decl, namespace))
            }
            _ => {}
        }
    }
    items.extend(generated);
}
/// Only real lvalues may be implicitly borrowed for a method receiver.
/// A method on a computed temporary still needs explicit ownership semantics.
fn addressable_receiver(expr: &Expr) -> bool {
    match expr {
        Expr::Var(_) => true,
        // Access through a borrowed receiver is still an lvalue. Field
        // lowering rewrites `self.field` with `self: &T` into
        // `Field(Deref(Var("self")), "field")` before method auto-borrowing.
        // Keep the requirement recursive so computed temporary owners are
        // not implicitly borrowed.
        Expr::Deref(owner) => addressable_receiver(owner),
        Expr::Field(owner, _) | Expr::Index(owner, _) => addressable_receiver(owner),
        _ => false,
    }
}
fn ident(name: &str) -> String {
    name.replace("::", "__")
}
fn dyn_trait_name(name: &str) -> Option<&str> {
    name.strip_prefix("dyn ")
}
fn impl_trait_name(name: &str) -> Option<&str> {
    name.strip_prefix("impl ")
}
fn type_suffix(t: &TypeRef) -> String {
    match t {
        // `()` is a valid Nova type but not a valid unquoted LLVM identifier.
        // Keep the readable spelling in diagnostics; use an ABI-safe type key
        // for concrete generic names such as Result<(), JsonError>.
        TypeRef::Named(name) if name == "()" => "unit".into(),
        TypeRef::Named(name) => dyn_trait_name(name)
            .map(|trait_name| format!("dyn_{}", ident(trait_name)))
            .unwrap_or_else(|| ident(name)),
        TypeRef::Applied(name, args) => format!(
            "{}_{}",
            ident(name),
            args.iter().map(type_suffix).collect::<Vec<_>>().join("_")
        ),
        TypeRef::Array(item, n) => format!("array_{}_{}", type_suffix(item), n),
        TypeRef::Function { params, result } => format!(
            "fn_{}_to_{}",
            params.iter().map(type_suffix).collect::<Vec<_>>().join("_"),
            type_suffix(result)
        ),
        TypeRef::Pointer { mutable, inner } => format!(
            "p{}_{}",
            if *mutable { "m" } else { "c" },
            type_suffix(inner)
        ),
        TypeRef::Reference { mutable, inner } => format!(
            "r{}_{}",
            if *mutable { "m" } else { "c" },
            type_suffix(inner)
        ),
    }
}
fn substitute(ty: &TypeRef, map: &HashMap<String, TypeRef>) -> TypeRef {
    match ty {
        TypeRef::Named(name) => map.get(name).cloned().unwrap_or_else(|| ty.clone()),
        TypeRef::Applied(name, args) => TypeRef::Applied(
            name.clone(),
            args.iter().map(|v| substitute(v, map)).collect(),
        ),
        TypeRef::Array(t, n) => TypeRef::Array(Box::new(substitute(t, map)), *n),
        TypeRef::Function { params, result } => TypeRef::Function {
            params: params.iter().map(|p| substitute(p, map)).collect(),
            result: Box::new(substitute(result, map)),
        },
        TypeRef::Pointer { mutable, inner } => TypeRef::Pointer {
            mutable: *mutable,
            inner: Box::new(substitute(inner, map)),
        },
        TypeRef::Reference { mutable, inner } => TypeRef::Reference {
            mutable: *mutable,
            inner: Box::new(substitute(inner, map)),
        },
    }
}
// Concrete struct identities are shared with HIR's type table. Use the same
// demangler in both phases so diagnostics never expose ABI-mangled names.
fn display_mangled_named_type(name: &str) -> String {
    crate::types::display_type_name(name)
}

// Human-readable Nova types in type-inference errors. Preserve the AST's
// structured types internally; never require the user to interpret Debug output.
fn display_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named(name) => dyn_trait_name(name)
            .map(|trait_name| format!("dyn {}", display_mangled_named_type(trait_name)))
            .unwrap_or_else(|| display_mangled_named_type(name)),
        TypeRef::Applied(name, args) => format!(
            "{}<{}>",
            display_mangled_named_type(name),
            args.iter().map(display_type).collect::<Vec<_>>().join(", ")
        ),
        TypeRef::Array(inner, count) => format!("[{}; {}]", display_type(inner), count),
        TypeRef::Pointer { mutable, inner } => format!(
            "*{} {}",
            if *mutable { "mut" } else { "const" },
            display_type(inner)
        ),
        TypeRef::Reference { mutable, inner } => format!(
            "&{}{}",
            if *mutable { "mut " } else { "" },
            display_type(inner)
        ),
        TypeRef::Function { params, result } => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(display_type)
                .collect::<Vec<_>>()
                .join(", "),
            display_type(result)
        ),
    }
}

fn type_mismatch(expected: &TypeRef, actual: &TypeRef) -> String {
    format!(
        "expected {}, found {}",
        display_type(expected),
        display_type(actual)
    )
}

/// An exclusive borrow may be viewed as shared for the duration of its use.
/// This never upgrades a shared borrow or moves the borrowed owner.
fn implicit_shared_reborrow(actual: &TypeRef, expected: &TypeRef) -> bool {
    matches!(
        (actual, expected),
        (
            TypeRef::Reference { mutable: true, inner: from },
            TypeRef::Reference { mutable: false, inner: to },
        ) if from == to
    )
}

// Structural type unification for generic calls. The sole reference
// mutability adjustment is the safe &mut T -> &T permission downgrade.
// Array lengths, raw pointer mutability, and concrete types stay invariant.
fn type_variables(
    actual: &TypeRef,
    pattern: &TypeRef,
    generics: &[String],
    map: &mut HashMap<String, TypeRef>,
) -> Result<(), String> {
    match pattern {
        TypeRef::Named(n) if generics.contains(n) => {
            if let Some(previous) = map.get(n) {
                if previous != actual {
                    return Err(format!(
                        "generic '{n}' inferred as both {} and {}",
                        display_type(previous),
                        display_type(actual)
                    ));
                }
            } else {
                map.insert(n.clone(), actual.clone());
            }
            Ok(())
        }
        TypeRef::Named(n) => {
            if actual == pattern || matches!(actual,TypeRef::Named(a) if a==n) {
                Ok(())
            } else {
                Err(type_mismatch(pattern, actual))
            }
        }
        TypeRef::Applied(n, ps) => match actual {
            TypeRef::Applied(a, as_) if n == a && ps.len() == as_.len() => {
                for (p, a) in ps.iter().zip(as_) {
                    type_variables(a, p, generics, map)?;
                }
                Ok(())
            }
            // The surface pass monomorphizes applied types to concrete names.
            TypeRef::Named(a) if a == &ident(n) || a.starts_with(&format!("{}__", ident(n))) => {
                Ok(())
            }
            _ => Err(type_mismatch(pattern, actual)),
        },
        TypeRef::Array(p, n) => match actual {
            TypeRef::Array(a, m) if n == m => type_variables(a, p, generics, map),
            TypeRef::Array(_, m) => Err(format!("array length mismatch: expected {n}, found {m}")),
            _ => Err(type_mismatch(pattern, actual)),
        },
        TypeRef::Function { params, result } => match actual {
            TypeRef::Function {
                params: a,
                result: r,
            } if params.len() == a.len() => {
                for (x, y) in a.iter().zip(params) {
                    type_variables(x, y, generics, map)?;
                }
                type_variables(r, result, generics, map)
            }
            _ => Err(format!(
                "expected function type {}, found {}",
                display_type(pattern),
                display_type(actual)
            )),
        },
        TypeRef::Pointer { mutable, inner: p } => match actual {
            TypeRef::Pointer {
                mutable: got,
                inner: a,
            } if mutable == got => type_variables(a, p, generics, map),
            TypeRef::Pointer { .. } => Err(format!(
                "pointer mutability mismatch: {}",
                type_mismatch(pattern, actual)
            )),
            _ => Err(type_mismatch(pattern, actual)),
        },
        TypeRef::Reference { mutable, inner: p } => match actual {
            TypeRef::Reference {
                mutable: got,
                inner: a,
            } if mutable == got || (!*mutable && *got) => type_variables(a, p, generics, map),
            _ => Err(format!(
                "reference type mismatch: {}",
                type_mismatch(pattern, actual)
            )),
        },
    }
}
// Widening-only implicit promotion. Narrowing stays explicit (`as`).
fn numeric_info(t: &TypeRef) -> Option<(bool, u8, bool)> {
    let TypeRef::Named(n) = t else { return None };
    match n.as_str() {
        "i8" => Some((true, 8, false)),
        "i16" => Some((true, 16, false)),
        "i32" => Some((true, 32, false)),
        "i64" | "isize" => Some((true, 64, false)),
        "u8" => Some((false, 8, false)),
        "u16" => Some((false, 16, false)),
        "u32" => Some((false, 32, false)),
        "u64" | "usize" => Some((false, 64, false)),
        "f32" => Some((true, 32, true)),
        "f64" => Some((true, 64, true)),
        _ => None,
    }
}
fn castable(from: &TypeRef, to: &TypeRef) -> bool {
    from == to
        || (numeric_info(from).is_some() && numeric_info(to).is_some())
        || (from == &TypeRef::Named("char".into()) && is_integer(to))
        || (to == &TypeRef::Named("char".into()) && is_integer(from))
        || (matches!(from, TypeRef::Function { .. }) && to == &TypeRef::Named("u64".into()))
        || (from == &TypeRef::Named("u64".into()) && matches!(to, TypeRef::Function { .. }))
        // Explicit pointer/address round-trips are supported by typed HIR and
        // LLVM on the 64-bit ABI. HIR still requires an unsafe context for
        // either direction; surface lowering only validates cast shape.
        || (matches!(from, TypeRef::Pointer { .. }) && to == &TypeRef::Named("u64".into()))
        || (from == &TypeRef::Named("u64".into()) && matches!(to, TypeRef::Pointer { .. }))
        || (matches!(from, TypeRef::Pointer { .. }) && matches!(to, TypeRef::Pointer { .. }))
        // The cast is always checked for referent type and unsafe context by HIR.
        || (matches!(from, TypeRef::Reference { .. }) && matches!(to, TypeRef::Pointer { .. }))
}
// Lossless implicit scalar conversion. Explicit `as` remains available for
// narrowing and potentially lossy conversions. No integer -> unsigned cast is
// implicit because negative inputs would wrap.
fn implicit_numeric_widen(from: &TypeRef, to: &TypeRef) -> bool {
    if from == to {
        return true;
    }
    let Some((src_signed, src_bits, src_float)) = numeric_info(from) else {
        return false;
    };
    let Some((dst_signed, dst_bits, dst_float)) = numeric_info(to) else {
        return false;
    };
    match (src_float, dst_float) {
        (true, true) => src_bits < dst_bits,
        (true, false) => false,
        (false, true) => {
            let exact_bits = if dst_bits == 32 { 24 } else { 53 };
            (src_bits as u16 + u16::from(src_signed == false)) <= exact_bits
        }
        (false, false) => {
            if src_signed && !dst_signed {
                false
            } else if !src_signed && dst_signed {
                src_bits < dst_bits
            } else {
                // Equal-width integer aliases (notably `usize`/`u64` and
                // `isize`/`i64` on the current 64-bit target) have identical
                // LLVM representations. Treat them as implicit no-op
                // conversions; narrower values may still widen losslessly.
                src_bits <= dst_bits
            }
        }
    }
}
fn promote_numeric(a: &TypeRef, b: &TypeRef) -> Option<TypeRef> {
    if a == b {
        return Some(a.clone());
    }
    let (sa, wa, fa) = numeric_info(a)?;
    let (sb, wb, fb) = numeric_info(b)?;
    let n = if fa || fb {
        let max_int = [(!fa, wa), (!fb, wb)]
            .into_iter()
            .filter(|(v, _)| *v)
            .map(|(_, bits)| bits)
            .max()
            .unwrap_or(0);
        if (fa && wa == 64) || (fb && wb == 64) || max_int > 24 {
            if max_int > 53 {
                return None;
            }
            "f64"
        } else {
            "f32"
        }
    } else {
        let signed_bits = (if sa { wa } else { 0 }).max(if sb { wb } else { 0 });
        let unsigned_bits = (if !sa { wa } else { 0 }).max(if !sb { wb } else { 0 });
        if signed_bits == 0 {
            match wa.max(wb) {
                8 => "u8",
                16 => "u16",
                32 => "u32",
                _ => "u64",
            }
        } else if unsigned_bits == 0 {
            match wa.max(wb) {
                8 => "i8",
                16 => "i16",
                32 => "i32",
                _ => "i64",
            }
        } else {
            let needed = signed_bits.max(unsigned_bits + 1);
            if needed <= 8 {
                "i8"
            } else if needed <= 16 {
                "i16"
            } else if needed <= 32 {
                "i32"
            } else if needed <= 64 {
                "i64"
            } else {
                return None;
            }
        }
    };
    Some(TypeRef::Named(n.into()))
}
// Generic-aware AST substitutions for inherited methods. They execute before HIR,
// preserving one concrete struct representation throughout LLVM lowering.
fn inherit_expr(e: &mut Expr, map: &HashMap<String, TypeRef>) {
    match e {
        Expr::If {
            condition,
            then_body,
            else_body,
        } => {
            inherit_expr(condition, map);
            inherit_stmts(then_body, map);
            inherit_stmts(else_body, map);
        }
        Expr::Block(body) => inherit_stmts(body, map),
        Expr::Unary(_, x) | Expr::Deref(x) | Expr::Yield(x) | Expr::Await(x) | Expr::Spread(x) => {
            inherit_expr(x, map)
        }
        Expr::Binary(a, _, b) | Expr::Index(a, b) => {
            inherit_expr(a, map);
            inherit_expr(b, map);
        }
        Expr::Call(_, args) | Expr::MacroCall(_, args) => {
            for a in args {
                inherit_expr(a, map);
            }
        }
        Expr::CallIndirect(callee, args) => {
            inherit_expr(callee, map);
            for a in args {
                inherit_expr(a, map);
            }
        }
        Expr::Lambda {
            params,
            result,
            body,
        } => {
            for p in params {
                p.ty = substitute(&p.ty, map);
            }
            *result = substitute(result, map);
            inherit_stmts(body, map);
        }
        Expr::MethodCall(base, _, args, _) => {
            inherit_expr(base, map);
            for a in args {
                inherit_expr(a, map);
            }
        }
        Expr::Field(base, _) | Expr::AddressOf { value: base, .. } => inherit_expr(base, map),
        Expr::GenericCall(_, types, args) => {
            for t in types {
                *t = substitute(t, map);
            }
            for a in args {
                inherit_expr(a, map);
            }
        }
        Expr::StructInit(_, fields) => {
            for (_, a) in fields {
                inherit_expr(a, map);
            }
        }
        Expr::GenericStructInit(_, types, fields) => {
            for t in types {
                *t = substitute(t, map);
            }
            for (_, a) in fields {
                inherit_expr(a, map);
            }
        }
        Expr::Array(args) => {
            for a in args {
                inherit_expr(a, map);
            }
        }
        Expr::Zero(t) => {
            *t = substitute(t, map);
        }
        Expr::Cast(value, t) => {
            inherit_expr(value, map);
            *t = substitute(t, map);
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Unit
        | Expr::Var(_)
        | Expr::FunctionRef(_) => {}
    }
}
fn inherit_stmts(stmts: &mut [Stmt], map: &HashMap<String, TypeRef>) {
    for s in stmts {
        match s {
            Stmt::Let {
                ty, init, defer, ..
            } => {
                if let Some(t) = ty {
                    *t = substitute(t, map);
                }
                inherit_expr(init, map);
                if let Some(e) = defer {
                    inherit_expr(e, map);
                }
            }
            Stmt::Defer(e)
            | Stmt::Expr(e)
            | Stmt::Tail(e)
            | Stmt::Set { value: e, .. }
            | Stmt::FieldSet { value: e, .. }
            | Stmt::CompoundSet { value: e, .. } => inherit_expr(e, map),
            Stmt::Return(Some(e)) => inherit_expr(e, map),
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                inherit_expr(condition, map);
                inherit_stmts(then_body, map);
                if let Some(body) = else_body {
                    inherit_stmts(body, map);
                }
            }
            Stmt::While { condition, body } => {
                inherit_expr(condition, map);
                inherit_stmts(body, map);
            }
            Stmt::DerefSet { value, .. } => inherit_expr(value, map),
            Stmt::ForEach { iterable, body, .. } => {
                inherit_expr(iterable, map);
                inherit_stmts(body, map);
            }
            Stmt::For {
                start, end, body, ..
            } => {
                inherit_expr(start, map);
                inherit_expr(end, map);
                inherit_stmts(body, map);
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => inherit_stmts(body, map),
            Stmt::Match { value, arms } => {
                inherit_expr(value, map);
                for arm in arms {
                    inherit_stmts(&mut arm.body, map);
                }
            }
            Stmt::Break | Stmt::Continue | Stmt::Return(None) => {}
        }
    }
}

impl Engine {
    fn register_generated_impl(
        &mut self,
        target: TypeRef,
        trait_name: &str,
        methods: Vec<ast::Function>,
    ) -> Result<(), String> {
        let block = Item::Impl(ast::Impl {
            generics: vec![],
            trait_name: Some(trait_name.into()),
            target,
            associated_types: vec![],
            methods,
        });
        self.collect(std::slice::from_ref(&block), "")?;
        self.items.push(block);
        Ok(())
    }

    fn accepts_dyn_trait(&self, actual: &TypeRef, expected: &TypeRef, namespace: &str) -> bool {
        let Some(trait_name) = (match expected {
            TypeRef::Named(name) => dyn_trait_name(name),
            TypeRef::Reference { inner, .. } => match inner.as_ref() {
                TypeRef::Named(name) => dyn_trait_name(name),
                _ => None,
            },
            _ => None,
        }) else {
            return false;
        };
        let concrete = match (expected, actual) {
            (TypeRef::Named(_), ty) => ty,
            (TypeRef::Reference { .. }, TypeRef::Reference { inner, .. }) => inner.as_ref(),
            _ => return false,
        };
        let concrete = match concrete {
            TypeRef::Reference { inner, .. } | TypeRef::Pointer { inner, .. } => inner.as_ref(),
            other => other,
        };
        if dyn_trait_name(display_type(concrete).as_str()).is_some() {
            return false;
        }
        self.concepts
            .implements_trait(concrete, trait_name, namespace)
            || self.items.iter().any(|item| {
                let Item::Impl(implementation) = item else {
                    return false;
                };
                implementation.trait_name.as_deref() == Some(trait_name)
                    && implementation.target == *concrete
            })
    }
    fn overload_key(&self, name: &str) -> String {
        if !self.funcs.contains_key(name) {
            return name.into();
        }
        let mut index = 1;
        loop {
            let candidate = format!("{name}__overload_{index}");
            if !self.funcs.contains_key(&candidate) {
                return candidate;
            }
            index += 1;
        }
    }
    fn function_keys(&self, name: &str) -> Vec<String> {
        let prefix = format!("{name}__overload_");
        let mut keys = self
            .funcs
            .keys()
            .filter(|key| key.as_str() == name || key.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }
    fn overload_base<'a>(&self, key: &'a str) -> &'a str {
        key.split_once("__overload_")
            .map(|(base, _)| base)
            .unwrap_or(key)
    }
    fn argument_hint(argument: &ast::Expr) -> Option<TypeRef> {
        match argument {
            ast::Expr::Int(raw) => {
                let value = raw.parse::<i64>().ok()?;
                Some(TypeRef::Named(
                    if value > i32::MAX as i64 {
                        "i64"
                    } else {
                        "i32"
                    }
                    .into(),
                ))
            }
            ast::Expr::Float(_) => Some(TypeRef::Named("f64".into())),
            ast::Expr::Bool(_) => Some(TypeRef::Named("bool".into())),
            ast::Expr::Str(_) => Some(TypeRef::Named("str".into())),
            ast::Expr::Cast(_, target) => Some(target.clone()),
            ast::Expr::StructInit(name, _) => Some(TypeRef::Named(name.clone())),
            ast::Expr::GenericStructInit(name, types, _) => {
                Some(TypeRef::Applied(name.clone(), types.clone()))
            }
            _ => None,
        }
    }

    fn overload_score(parameter: &TypeRef, argument: Option<&TypeRef>) -> usize {
        let Some(argument) = argument else { return 1 };
        if parameter == argument {
            return 4;
        }
        let numeric = |ty: &TypeRef| {
            matches!(ty,
            TypeRef::Named(name) if matches!(name.as_str(),
                "i8"|"i16"|"i32"|"i64"|"i128"|"u8"|"u16"|"u32"|"u64"|"u128"|"f32"|"f64"))
        };
        if numeric(parameter) && numeric(argument) {
            2
        } else {
            0
        }
    }

    fn select_overload(&self, name: &str, arguments: &[ast::Expr]) -> Option<String> {
        let mut matches = self
            .function_keys(name)
            .into_iter()
            .filter(|key| {
                let function = &self.funcs[key].decl;
                let fixed = function.params.len() - usize::from(function.variadic);
                (function.variadic && arguments.len() >= fixed)
                    || (!function.variadic && arguments.len() == fixed)
            })
            .map(|key| {
                let function = &self.funcs[&key].decl;
                let fixed = function.params.len() - usize::from(function.variadic);
                let score = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        let parameter =
                            &function.params[if index < fixed { index } else { fixed }].ty;
                        Self::overload_score(parameter, Self::argument_hint(argument).as_ref())
                    })
                    .sum::<usize>();
                (score, key)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| right.cmp(left));
        matches.into_iter().next().map(|(_, key)| key)
    }
    // Validate and materialize inheritance in topological order. Inherited
    // methods are re-specialized with the derived `self` type rather than
    // copying a base value (which could otherwise double-drop owned fields).
    fn prepare_inheritance(&mut self) -> Result<(), String> {
        let mut done = HashSet::new();
        let mut stack = Vec::new();
        let names = self.structs.keys().cloned().collect::<Vec<_>>();
        for name in names {
            self.prepare_class(&name, &mut done, &mut stack)?;
        }
        // Explicit `override` without a parent method is an error, even on roots.
        Ok(())
    }
    fn prepare_class(
        &mut self,
        name: &str,
        done: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if stack.iter().any(|n| n == name) {
            stack.push(name.into());
            return Err(format!("cyclic class inheritance: {}", stack.join(" -> ")));
        }
        stack.push(name.into());
        let child = self
            .structs
            .get(name)
            .ok_or_else(|| format!("unknown class '{name}'"))?
            .clone();
        let parent = if let Some(base) = &child.decl.parent {
            let (raw, args) = match base {
                TypeRef::Named(n) => (n.as_str(), vec![]),
                TypeRef::Applied(n, args) => (n.as_str(), args.clone()),
                _ => {
                    return Err(format!(
                        "base type of '{name}' must be a named class or struct"
                    ))
                }
            };
            let p = self.qualify(raw, &child.namespace);
            if !self.structs.contains_key(&p) {
                return Err(format!("'{name}' inherits unknown class/struct '{raw}'"));
            }
            self.prepare_class(&p, done, stack)?;
            let parent_decl = &self.structs[&p].decl;
            if parent_decl.generics.len() != args.len() {
                return Err(format!(
                    "base '{}' expects {} template parameters, got {}",
                    p,
                    parent_decl.generics.len(),
                    args.len()
                ));
            }
            for ty in &args {
                let mut vars = HashSet::new();
                fn used(t: &TypeRef, vars: &mut HashSet<String>) {
                    match t {
                        TypeRef::Named(n) => {
                            vars.insert(n.clone());
                        }
                        TypeRef::Applied(_, args) => {
                            for t in args {
                                used(t, vars);
                            }
                        }
                        TypeRef::Array(t, _)
                        | TypeRef::Pointer { inner: t, .. }
                        | TypeRef::Reference { inner: t, .. } => used(t, vars),
                        TypeRef::Function { params, result } => {
                            for p in params {
                                used(p, vars);
                            }
                            used(result, vars);
                        }
                    }
                }
                used(ty, &mut vars);
                for var in vars {
                    // Valid primitive/concrete types are fine; generic names must
                    // be declared by the derived class.
                    if !child.decl.generics.contains(&var)
                        && ![
                            "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64",
                            "str", "bool", "usize", "isize", "()",
                        ]
                        .contains(&var.as_str())
                        && !self
                            .structs
                            .contains_key(&self.qualify(&var, &child.namespace))
                    {
                        return Err(format!(
                            "unbound template parameter '{var}' in base of '{name}'"
                        ));
                    }
                }
            }
            Some((p, args))
        } else {
            None
        };
        if let Some((base, args)) = parent {
            let derived_args = child
                .decl
                .generics
                .iter()
                .map(|g| TypeRef::Named(g.clone()))
                .collect::<Vec<_>>();
            let derived_ty = if derived_args.is_empty() {
                TypeRef::Named(name.into())
            } else {
                TypeRef::Applied(name.into(), derived_args)
            };
            let inherited = self
                .funcs
                .iter()
                .filter(|(key, _)| {
                    key.starts_with(&format!("{base}::"))
                        && !key.rsplit_once("::").unwrap().1.starts_with("__super__")
                })
                .map(|(key, val)| (key.clone(), val.clone()))
                .collect::<Vec<_>>();
            let inherited_names = inherited
                .iter()
                .map(|(key, _)| key.rsplit_once("::").unwrap().1.to_string())
                .collect::<HashSet<_>>();
            for (key, method) in &self.funcs {
                if key.starts_with(&format!("{name}::"))
                    && method.decl.is_override
                    && !inherited_names.contains(key.rsplit_once("::").unwrap().1)
                {
                    return Err(format!(
                        "method '{key}' marked override has no inherited method"
                    ));
                }
            }
            for (qualified, ancestor) in inherited {
                let method = qualified.rsplit_once("::").unwrap().1;
                let key = format!("{name}::{method}");
                let existing = self.funcs.get(&key).cloned();
                // A dedicated parent implementation allows `super.method()`
                // to call base code without slicing or creating a second owner.
                let mut parent_method = ancestor.decl.clone();
                let bindings = ancestor
                    .impl_generics
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                parent_method.name = format!("__super__{method}");
                // Rebind only the receiver's owner type. The previous code
                // replaced `&mut Base` with plain `Derived`, silently losing
                // mutability while materializing inherited methods. Preserve
                // the receiver ABI/borrow shape so framework behaviours can
                // mutate state from a derived class just like a Unity
                // Component or MonoBehaviour.
                if let Some(receiver) = parent_method.params.get_mut(0) {
                    receiver.ty = match &receiver.ty {
                        TypeRef::Reference { mutable, .. } => TypeRef::Reference {
                            mutable: *mutable,
                            inner: Box::new(derived_ty.clone()),
                        },
                        TypeRef::Pointer { mutable, .. } => TypeRef::Pointer {
                            mutable: *mutable,
                            inner: Box::new(derived_ty.clone()),
                        },
                        _ => derived_ty.clone(),
                    };
                }
                for p in parent_method.params.iter_mut().skip(1) {
                    p.ty = substitute(&p.ty, &bindings);
                }
                parent_method.result = substitute(&parent_method.result, &bindings);
                inherit_stmts(&mut parent_method.body, &bindings);
                parent_method.is_virtual = false;
                parent_method.is_override = false;
                self.funcs.insert(
                    format!("{name}::__super__{method}"),
                    FuncDef {
                        decl: parent_method,
                        namespace: ancestor.namespace.clone(),
                        impl_generics: child.decl.generics.clone(),
                    },
                );
                if let Some(own) = existing {
                    if method == "drop" {
                        return Err(format!("'{name}::drop' cannot replace inherited '{qualified}': automatic base destructor chaining is not implemented (avoids double free/leaks)"));
                    }
                    if own.decl.is_override {
                        if !ancestor.decl.is_virtual {
                            return Err(format!("'{name}::{method}' cannot override non-virtual base method '{qualified}'"));
                        }
                        if own.decl.variadic != ancestor.decl.variadic
                            || own.decl.generics.len() != ancestor.decl.generics.len()
                            || own.decl.params.len() != ancestor.decl.params.len()
                        {
                            return Err(format!(
                                "override '{name}::{method}' has incompatible parameters"
                            ));
                        }
                        let impl_map = ancestor
                            .impl_generics
                            .iter()
                            .cloned()
                            .zip(args.iter().cloned())
                            .collect::<HashMap<_, _>>();
                        let expected_result = substitute(&ancestor.decl.result, &impl_map);
                        let expected_params = ancestor
                            .decl
                            .params
                            .iter()
                            .skip(1)
                            .map(|p| substitute(&p.ty, &impl_map))
                            .collect::<Vec<_>>();
                        if own.decl.result != expected_result
                            || own
                                .decl
                                .params
                                .iter()
                                .skip(1)
                                .map(|p| p.ty.clone())
                                .collect::<Vec<_>>()
                                != expected_params
                        {
                            return Err(format!("override '{name}::{method}' must preserve inherited return and argument types"));
                        }
                        // As in C++, a valid override remains virtual for any
                        // later subclass, even if it only spells `override`.
                        if let Some(implemented) = self.funcs.get_mut(&key) {
                            implemented.decl.is_virtual = true;
                        }
                    } else if ancestor.decl.is_virtual {
                        return Err(format!("'{name}::{method}' replaces a virtual method; add the 'override' modifier"));
                    }
                    continue;
                }
                // Inheritance of an instance method specializes parent template
                // variables to the derived class's generic arguments.
                let mut method_decl = ancestor.decl.clone();
                if let Some(receiver) = method_decl.params.get_mut(0) {
                    receiver.ty = match &receiver.ty {
                        TypeRef::Reference { mutable, .. } => TypeRef::Reference {
                            mutable: *mutable,
                            inner: Box::new(derived_ty.clone()),
                        },
                        TypeRef::Pointer { mutable, .. } => TypeRef::Pointer {
                            mutable: *mutable,
                            inner: Box::new(derived_ty.clone()),
                        },
                        _ => derived_ty.clone(),
                    };
                }
                for p in method_decl.params.iter_mut().skip(1) {
                    p.ty = substitute(&p.ty, &bindings);
                }
                method_decl.result = substitute(&method_decl.result, &bindings);
                inherit_stmts(&mut method_decl.body, &bindings);
                method_decl.is_override = false;
                self.funcs.insert(
                    key,
                    FuncDef {
                        decl: method_decl,
                        namespace: ancestor.namespace,
                        impl_generics: child.decl.generics.clone(),
                    },
                );
            }
        }
        for (key, method) in &self.funcs {
            if key.starts_with(&format!("{name}::"))
                && method.decl.is_override
                && child.decl.parent.is_none()
            {
                return Err(format!(
                    "method '{key}' is marked override but '{name}' has no base type"
                ));
            }
            if key.starts_with(&format!("{name}::"))
                && method.decl.is_virtual
                && method.decl.name == "drop"
            {
                return Err(format!("virtual destructor '{key}' is unsupported until dynamic class ownership is implemented"));
            }
        }
        stack.pop();
        done.insert(name.into());
        Ok(())
    }
    fn collect(&mut self, items: &[Item], ns: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Namespace { name, items } => self.collect(items, &join(ns, name))?,
                Item::Struct(s) => {
                    let name = join(ns, &s.name);
                    if self.structs.contains_key(&name) || self.enums.contains_key(&name) {
                        return Err(format!("duplicate type '{name}'"));
                    }
                    check_params(&s.generics)?;
                    self.structs.insert(
                        name,
                        StructDef {
                            decl: s.clone(),
                            namespace: ns.into(),
                        },
                    );
                }
                Item::Enum(e) => {
                    let name = join(ns, &e.name);
                    if self.structs.contains_key(&name) || self.enums.contains_key(&name) {
                        return Err(format!("duplicate type '{name}'"));
                    }
                    check_params(&e.generics)?;
                    let mut variants = HashSet::new();
                    for v in &e.variants {
                        if !variants.insert(&v.name) {
                            return Err(format!("duplicate variant '{}::{}'", name, v.name));
                        }
                        if v.payload.len() > 8 {
                            return Err(
                                "enum variants currently support at most eight payload values"
                                    .into(),
                            );
                        }
                    }
                    if e.variants.is_empty() {
                        return Err(format!("enum '{name}' needs at least one variant"));
                    }
                    self.enums.insert(
                        name,
                        EnumDef {
                            decl: e.clone(),
                            namespace: ns.into(),
                        },
                    );
                }
                Item::Function(f) => {
                    let name = join(ns, &f.name);
                    check_params(&f.generics)?;
                    ast::function_is_unsafe(&f.attributes, &format!("function '{name}'"))?;
                    if f.params
                        .iter()
                        .any(|p| p.name.starts_with("__nova_invalid_pack__"))
                    {
                        return Err(format!(
                            "function '{name}': variadic pack must be the last parameter"
                        ));
                    }
                    if f.params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<HashSet<_>>()
                        .len()
                        != f.params.len()
                    {
                        return Err(format!("duplicate parameter name in '{name}'"));
                    }
                    let key = self.overload_key(&name);
                    self.funcs.insert(
                        key,
                        FuncDef {
                            decl: f.clone(),
                            namespace: ns.into(),
                            impl_generics: vec![],
                        },
                    );
                }
                Item::Impl(block) => {
                    check_params(&block.generics)?;
                    let target_name = match &block.target {
                        TypeRef::Named(n) | TypeRef::Applied(n, _) => n,
                        _ => return Err("impl target must be a named type".into()),
                    };
                    // A blanket impl whose target is itself a type parameter is
                    // resolved lazily from a call site (for example
                    // `impl<T: Arithmetic> T`).  It must carry a constraint;
                    // otherwise it would be an unconstrained extension of
                    // every type and would make method lookup ambiguous.
                    if block.trait_name.is_none()
                        && block.generics.iter().any(|generic| generic == target_name)
                        && block.methods.iter().any(|method| {
                            !method.concept_bounds.iter().any(|bound| {
                                bound
                                    .args
                                    .iter()
                                    .any(|argument| contains_type_var(argument, target_name))
                            })
                        })
                    {
                        return Err(format!(
                            "blanket impl target '{target_name}' requires a concept bound"
                        ));
                    }
                    let qualified = self.qualify(target_name, ns);
                    for method in &block.methods {
                        ast::function_is_unsafe(
                            &method.attributes,
                            &format!("method '{}'", method.name),
                        )?;
                        if method
                            .params
                            .iter()
                            .any(|p| p.name.starts_with("__nova_invalid_pack__"))
                        {
                            return Err(format!(
                                "method '{}': variadic pack must be the last parameter",
                                method.name
                            ));
                        }
                        check_params(&method.generics)?;
                        if method.name.starts_with("__super__") {
                            return Err(format!("method '{}': names beginning with __super__ are reserved for inherited dispatch",method.name));
                        }
                        if (method.is_virtual || method.is_override)
                            && method.params.first().map(|p| &p.ty) != Some(&block.target)
                        {
                            return Err(format!("method '{}': virtual/override self type must match the impl target",method.name));
                        }
                        if method.generics.iter().any(|g| block.generics.contains(g)) {
                            return Err(format!(
                                "method '{}' repeats an impl template parameter",
                                method.name
                            ));
                        }
                        let name = format!("{qualified}::{}", method.name);
                        let key = self.overload_key(&name);
                        self.funcs.insert(
                            key,
                            FuncDef {
                                decl: method.clone(),
                                namespace: ns.into(),
                                impl_generics: block.generics.clone(),
                            },
                        );
                        let static_marker = method.attributes.iter().any(|a| a.name == "static");
                        if static_marker {
                            if method.params.iter().any(|p| p.name == "self") {
                                return Err(format!("error[NOVA-CTOR001]: static method '{name}' must not declare a self parameter"));
                            }
                            if method.is_virtual || method.is_override {
                                return Err(format!("error[NOVA-CTOR002]: static method '{name}' cannot be virtual or override"));
                            }
                            if method.name == "new"
                                && method.result != block.target
                                && method.result != TypeRef::Named("Self".into())
                            {
                                return Err(format!("error[NOVA-CTOR003]: constructor '{name}' must return the enclosing type {}", display_type(&block.target)));
                            }
                        } else if method.params.first().map(|p| p.name.as_str()) != Some("self") {
                            return Err(format!("error[NOVA-CTOR004]: method '{name}' must declare self as the first parameter or be marked `static fn`"));
                        }
                    }
                }
                Item::Extern(f) => {
                    ast::function_is_unsafe(&f.attributes, &format!("extern '{}'", f.name))?;
                    if f.params
                        .iter()
                        .any(|p| p.name == "__nova_invalid_c_varargs__")
                    {
                        return Err(format!(
                            "extern '{}': C ellipsis must appear once and last",
                            f.name
                        ));
                    }
                    let name = join(ns, &f.name);
                    if self.externs.insert(name.clone(), f.clone()).is_some() {
                        return Err(format!("duplicate extern '{name}'"));
                    }
                    let mut out = f.clone();
                    out.name = ident(&name);
                    self.items.push(Item::Extern(out));
                }
                Item::Import(path) => self.imports.push((ns.into(), path.clone())),
                Item::Macro(m) => {
                    return Err(format!(
                        "internal error: macro '{}' was not expanded",
                        m.name
                    ))
                }
                Item::TypeAlias(a) => {
                    return Err(format!(
                        "internal error: typedef '{}' was not expanded",
                        a.name
                    ))
                }
                Item::Trait(t) => {
                    return Err(format!(
                        "internal error: trait '{}' was not validated",
                        t.name
                    ))
                }
                Item::Concept(c) => {
                    return Err(format!(
                        "internal error: concept '{}' was not evaluated",
                        c.name
                    ))
                }
                Item::ConceptAssert(_) => {
                    return Err("internal error: static_assert was not evaluated".into())
                }
                Item::CompileTimeConst(decl) => {
                    // consteval::fold must inline and remove module constants
                    // before the surface pass materializes declarations.
                    return Err(format!(
                        "internal error: compile-time constant '{}' was not evaluated",
                        decl.name
                    ));
                }
                Item::Public(inner) => self.collect(std::slice::from_ref(inner.as_ref()), ns)?,
                Item::Mod { name, .. } => {
                    return Err(format!("unresolved module '{name}' (use package loader)"))
                }
                Item::Use { path, .. } => {
                    return Err(format!("unresolved use '{path}' (use package loader)"))
                }
            }
        }
        Ok(())
    }
    /// Resolve a source-level type spelling to the unique declaration identity.
    ///
    /// Imports are preferred, followed by namespace-relative lookup. The final
    /// unique-short-name fallback is needed for type positions that survive
    /// typedef/module expansion without their `use` entry (notably imported
    /// standard-library class payloads such as `String`). It is deliberately
    /// disabled when more than one declaration has that short name.
    fn resolve_declared_type_name(&self, name: &str, ns: &str) -> String {
        let is_declared = |candidate: &str| {
            self.structs.contains_key(candidate) || self.enums.contains_key(candidate)
        };
        if is_declared(name) {
            return name.to_owned();
        }
        if let Some(imported) = self.imports.iter().rev().find_map(|(scope, path)| {
            let visible = scope == ns
                || (!scope.is_empty() && ns.starts_with(&format!("{scope}::")));
            (visible && path.rsplit("::").next() == Some(name) && is_declared(path))
                .then(|| path.clone())
        }) {
            return imported;
        }
        let qualified = self.qualify(name, ns);
        if is_declared(&qualified) {
            return qualified;
        }
        let mut matches = self
            .structs
            .keys()
            .chain(self.enums.keys())
            .filter(|candidate| candidate.rsplit("::").next() == Some(name));
        if let Some(only) = matches.next() {
            if matches.next().is_none() {
                return only.clone();
            }
        }
        qualified
    }
    fn qualify(&self, name: &str, ns: &str) -> String {
        let normalized = name.replace("__", "::");
        if normalized != name
            && (self.structs.contains_key(&normalized)
                || self.enums.contains_key(&normalized)
                || self.funcs.contains_key(&normalized)
                || self.externs.contains_key(&normalized))
        {
            return normalized;
        }
        if ns.is_empty() {
            return name.into();
        }
        // A fully-qualified symbol (e.g. std::old::string::from_text) takes
        // precedence over a relative spelling. A static method such as
        // RouteTemplate::valid, however, can itself contain `::` while
        // still being relative to the current module (NB::web).
        if name.contains("::")
            && (self.structs.contains_key(name)
                || self.enums.contains_key(name)
                || self.funcs.contains_key(name)
                || self.externs.contains_key(name))
        {
            return name.into();
        }
        let mut scope = ns;
        while !scope.is_empty() {
            let candidate = join(scope, name);
            if self.structs.contains_key(&candidate)
                || self.enums.contains_key(&candidate)
                || self.funcs.contains_key(&candidate)
                || self.externs.contains_key(&candidate)
            {
                return candidate;
            }
            scope = scope
                .rsplit_once("::")
                .map(|(parent, _)| parent)
                .unwrap_or("");
        }
        // During the stdlib namespace migration, accept the historical public
        // spelling without creating a second declaration or type identity.
        // ABI, metadata and already-canonical old paths remain untouched.
        if let Some(rest) = name.strip_prefix("std::") {
            if !rest.starts_with("old::")
                && !rest.starts_with("abi::")
                && !rest.starts_with("meta::")
            {
                let legacy = format!("std::old::{rest}");
                if self.funcs.contains_key(&legacy)
                    || self.externs.contains_key(&legacy)
                    || self.structs.contains_key(&legacy)
                    || self.enums.contains_key(&legacy)
                {
                    return legacy;
                }
            }
        }
        name.into()
    }
    // Resolve `T::method(...)` after function monomorphization has bound T to
    // a concrete type. Ordinary path qualification cannot find a declaration
    // named literally `T::method`, and a mangled concrete type such as
    // `std__old__string__String` is not itself a source-language namespace.
    //
    // The owner is matched against the instantiated type's actual template
    // arguments, rather than guessing by splitting its mangled symbol. This
    // also supports static methods declared in `impl Owner<U>` blocks.
    fn generic_static_call(
        &mut self,
        name: &str,
        scope: &Scope,
        explicit_args: usize,
    ) -> Result<Option<(String, Vec<TypeRef>)>, String> {
        let Some((parameter, method)) = name.split_once("::") else {
            return Ok(None);
        };
        let Some(bound) = scope.subst.get(parameter) else {
            return Ok(None);
        };
        let resolved = self.resolved_type(bound, &scope.namespace, &scope.subst)?;
        let TypeRef::Named(concrete) = resolved else {
            return Err(format!(
                "static call '{name}' requires a named concrete type, got {}",
                display_type(&resolved)
            ));
        };
        let owner_args = self
            .instance_args
            .get(&concrete)
            .cloned()
            .unwrap_or_default();
        let mut candidates = self
            .funcs
            .iter()
            .filter_map(|(full, definition)| {
                let (owner, _) = self.overload_base(full).rsplit_once("::")?;
                if definition.decl.name != method
                    || !definition
                        .decl
                        .attributes
                        .iter()
                        .any(|a| a.name == "static")
                    || definition.impl_generics.len() != owner_args.len()
                {
                    return None;
                }
                let fixed = definition.decl.params.len() - usize::from(definition.decl.variadic);
                if (!definition.decl.variadic && explicit_args != fixed)
                    || (definition.decl.variadic && explicit_args < fixed)
                {
                    return None;
                }
                let instance = if owner_args.is_empty() {
                    ident(owner)
                } else {
                    format!(
                        "{}__{}",
                        ident(owner),
                        owner_args
                            .iter()
                            .map(type_suffix)
                            .collect::<Vec<_>>()
                            .join("__")
                    )
                };
                (instance == concrete).then(|| full.clone())
            })
            .collect::<Vec<_>>();
        candidates.sort();
        match candidates.as_slice() {
            [selected] => Ok(Some((selected.clone(), owner_args))),
            [] => Err(format!(
                "type '{}' has no static method '{}' (from generic call '{}')",
                display_type(&TypeRef::Named(concrete)),
                method,
                name
            )),
            _ => Err(format!(
                "ambiguous static method '{}': {}",
                name,
                candidates.join(", ")
            )),
        }
    }
    /// Resolve a static constructor supplied by a constrained blanket impl,
    /// such as `impl<T: Arithmetic> T { static fn new(...) -> T { ... } }`.
    /// This is deliberately limited to `new`: ordinary static dispatch must
    /// remain tied to a concrete owner and must not become open extension
    /// dispatch accidentally.
    fn blanket_constructor_call(
        &self,
        name: &str,
        explicit_args: usize,
    ) -> Result<Option<(String, Vec<TypeRef>)>, String> {
        let Some((concrete, method)) = name.rsplit_once("::") else {
            return Ok(None);
        };
        if method != "new" {
            return Ok(None);
        }
        let mut candidates = self
            .funcs
            .iter()
            .filter_map(|(full, definition)| {
                let (owner, _) = self.overload_base(full).rsplit_once("::")?;
                if definition.decl.name != "new"
                    || !definition
                        .decl
                        .attributes
                        .iter()
                        .any(|attribute| attribute.name == "static")
                    || definition
                        .impl_generics
                        .iter()
                        .all(|generic| generic != owner)
                {
                    return None;
                }
                let fixed = definition.decl.params.len() - usize::from(definition.decl.variadic);
                if (!definition.decl.variadic && explicit_args != fixed)
                    || (definition.decl.variadic && explicit_args < fixed)
                {
                    return None;
                }
                Some((full.clone(), owner.to_string()))
            })
            .filter(|(_, owner)| owner != concrete)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        match candidates.as_slice() {
            [] => Ok(None),
            [(selected, _)] => Ok(Some((
                selected.clone(),
                vec![TypeRef::Named(concrete.to_string())],
            ))),
            _ => Err(format!(
                "ambiguous blanket constructor '{}': {}",
                name,
                candidates
                    .iter()
                    .map(|(candidate, _)| candidate.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    fn concrete_constructor_call(
        &self,
        concrete: &str,
        explicit_args: usize,
    ) -> Result<Option<String>, String> {
        let owner_args = self
            .instance_args
            .get(concrete)
            .cloned()
            .unwrap_or_default();
        let mut candidates = self
            .funcs
            .iter()
            .filter_map(|(full, definition)| {
                let (owner, _) = self.overload_base(full).rsplit_once("::")?;
                if definition.decl.name != "new"
                    || !definition
                        .decl
                        .attributes
                        .iter()
                        .any(|attribute| attribute.name == "static")
                    || definition.impl_generics.len() != owner_args.len()
                {
                    return None;
                }
                let fixed = definition.decl.params.len() - usize::from(definition.decl.variadic);
                if (!definition.decl.variadic && explicit_args != fixed)
                    || (definition.decl.variadic && explicit_args < fixed)
                {
                    return None;
                }
                let instance = if owner_args.is_empty() {
                    ident(owner)
                } else {
                    format!(
                        "{}__{}",
                        ident(owner),
                        owner_args
                            .iter()
                            .map(type_suffix)
                            .collect::<Vec<_>>()
                            .join("__")
                    )
                };
                (instance == concrete).then(|| full.clone())
            })
            .collect::<Vec<_>>();
        candidates.sort();
        match candidates.as_slice() {
            [] => Ok(None),
            [selected] => Ok(Some(selected.clone())),
            _ => Err(format!(
                "ambiguous constructor for '{}': {}",
                concrete,
                candidates.join(", ")
            )),
        }
    }
    fn resolved_type(
        &mut self,
        ty: &TypeRef,
        ns: &str,
        subst: &HashMap<String, TypeRef>,
    ) -> Result<TypeRef, String> {
        match substitute(ty, subst) {
            TypeRef::Named(n) => {
                if let Some(trait_name) = dyn_trait_name(&n) {
                    // `use path::Trait` must canonicalize the trait inside a
                    // dyn object too. Otherwise method lookup searches for a
                    // short-name vtable while materialization generated one
                    // for the fully-qualified trait identity.
                    let imported = self.imports.iter().rev().find_map(|(scope, path)| {
                        let visible = scope == ns
                            || (!scope.is_empty() && ns.starts_with(&format!("{scope}::")));
                        (visible && path.rsplit("::").next() == Some(trait_name))
                            .then(|| path.clone())
                    });
                    let canonical = imported.unwrap_or_else(|| {
                        let qualified = self.qualify(trait_name, ns);
                        if qualified == trait_name && !trait_name.contains("::") && !ns.is_empty() {
                            join(ns, trait_name)
                        } else {
                            qualified
                        }
                    });
                    return Ok(TypeRef::Named(format!("dyn {canonical}")));
                }
                // A type imported with `use path::Type` is visible by its
                // short name in that module and its child modules. Resolve the
                // import before namespace-relative lookup; otherwise enum
                // payloads and fields keep `String` as an unrelated unresolved
                // name while expressions correctly resolve methods to the
                // canonical `std::text::string::String` declaration.
                let q = self.resolve_declared_type_name(&n, ns);
                if self.structs.contains_key(&q) || self.enums.contains_key(&q) {
                    self.instantiate_type(&q, &[])
                } else if self.structs.contains_key(&n) || self.enums.contains_key(&n) {
                    self.instantiate_type(&n, &[])
                } else if impl_trait_name(&n).is_some() {
                    return Err("'impl Trait' is only valid as a function return type".into());
                } else {
                    Ok(TypeRef::Named(n))
                }
            }
            TypeRef::Applied(n, args) => {
                let args = args
                    .iter()
                    .map(|x| self.resolved_type(x, ns, subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let q = self.resolve_declared_type_name(&n, ns);
                self.instantiate_type(&q, &args)
            }
            TypeRef::Array(inner, n) => Ok(TypeRef::Array(
                Box::new(self.resolved_type(&inner, ns, subst)?),
                n,
            )),
            TypeRef::Function { params, result } => Ok(TypeRef::Function {
                params: params
                    .iter()
                    .map(|p| self.resolved_type(p, ns, subst))
                    .collect::<Result<Vec<_>, _>>()?,
                result: Box::new(self.resolved_type(&result, ns, subst)?),
            }),
            TypeRef::Pointer { mutable, inner } => Ok(TypeRef::Pointer {
                mutable,
                inner: Box::new(self.resolved_type(&inner, ns, subst)?),
            }),
            TypeRef::Reference { mutable, inner } => Ok(TypeRef::Reference {
                mutable,
                inner: Box::new(self.resolved_type(&inner, ns, subst)?),
            }),
        }
    }
    fn instantiate_type(&mut self, name: &str, args: &[TypeRef]) -> Result<TypeRef, String> {
        self.type_instantiation_depth += 1;
        let result = self.instantiate_type_inner(name, args);
        self.type_instantiation_depth -= 1;

        // A nested generic field (for example Vec<JsonValue>) must not
        // specialize Vec::drop while JsonValue's own payload fields are still
        // being materialized. Flush only after the outermost type is complete.
        if self.type_instantiation_depth == 0
            && !self.flushing_deferred_destructors
            && result.is_ok()
        {
            self.flush_deferred_destructors()?;
        }
        result
    }

    fn flush_deferred_destructors(&mut self) -> Result<(), String> {
        self.flushing_deferred_destructors = true;
        while let Some((destructor, args)) = self.pending_destructors.pop() {
            if let Err(error) = self.instantiate_function(&destructor, &args, None) {
                self.flushing_deferred_destructors = false;
                return Err(error);
            }
        }
        self.flushing_deferred_destructors = false;
        Ok(())
    }

    fn instantiate_type_inner(&mut self, name: &str, args: &[TypeRef]) -> Result<TypeRef, String> {
        // The stage-0 coroutine runtime transports a single signed 64-bit ABI
        // word. Generic owners are a *compile-time* type contract, not a way
        // to reinterpret an arbitrary struct, pointer or f64 as that word.
        // Keep the restriction in monomorphization so all construction paths,
        // including explicitly instantiated globals/fields, are validated.
        if matches!(
            name,
            "std::old::task::Task"
                | "std::old::task::Future"
                | "std::old::generator::Generator"
                | "std::old::coro::Coroutine"
        ) {
            if args.len() != 1 || !matches!(&args[0], TypeRef::Named(t) if t == "i64" || t == "u64")
            {
                return Err(format!(
                    "typed asynchronous owner '{name}' requires an i64 or u64 result in the stage-0 coroutine ABI; arbitrary T requires typed coroutine frames (got {args:?})"
                ));
            }
        }
        if self.type_cache.len() > 2048 {
            return Err("type monomorphization limit (2048) exceeded".into());
        }
        let concrete = if args.is_empty() {
            ident(name)
        } else {
            format!(
                "{}__{}",
                ident(name),
                args.iter().map(type_suffix).collect::<Vec<_>>().join("__")
            )
        };
        if self.type_cache.contains_key(&concrete) {
            return Ok(TypeRef::Named(concrete));
        }
        let (fields, params, bounds, attributes, ns, is_enum) =
            if let Some(s) = self.structs.get(name) {
                (
                    s.decl.fields.clone(),
                    s.decl.generics.clone(),
                    s.decl.concept_bounds.clone(),
                    s.decl.attributes.clone(),
                    s.namespace.clone(),
                    false,
                )
            } else if let Some(e) = self.enums.get(name) {
                (
                    vec![],
                    e.decl.generics.clone(),
                    e.decl.concept_bounds.clone(),
                    e.decl.attributes.clone(),
                    e.namespace.clone(),
                    true,
                )
            } else {
                let mut nearby = self
                    .structs
                    .keys()
                    .filter(|candidate| candidate.ends_with(name) || candidate.contains(name))
                    .cloned()
                    .collect::<Vec<_>>();
                nearby.sort();
                nearby.truncate(5);
                let hint = if nearby.is_empty() {
                    String::new()
                } else {
                    format!("; known candidates: {}", nearby.join(", "))
                };
                return Err(format!("unknown generic type '{name}'{hint}"));
            };
        if params.len() != args.len() {
            return Err(format!(
                "type '{name}' expects {} template parameters, got {}",
                params.len(),
                args.len()
            ));
        }
        let mut subst = params
            .into_iter()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        // `Self` in fields and enum payloads refers to this concrete
        // instantiation, not to a separately named unresolved type. Register
        // it alongside generic parameters before resolving recursive payloads.
        subst.insert("Self".into(), TypeRef::Named(concrete.clone()));
        self.concepts
            .check(&bounds, &ns, &subst, &format!("type {name}"))?;
        validate_attrs(&attributes, is_enum)?;
        if !is_enum
            && attributes.iter().any(|attribute| attribute.name == "repr" && attribute.args.len() == 1 && attribute.args[0] == "C")
            && self.structs.get(name).is_some_and(|definition| definition.decl.parent.is_some())
        {
            return Err(format!("#[repr(C)] cannot be combined with Nova struct inheritance on '{name}'"));
        }
        self.type_cache
            .insert(concrete.clone(), TypeRef::Named(concrete.clone()));
        self.instance_args.insert(concrete.clone(), args.to_vec());
        let mut concrete_fields = Vec::<ast::Field>::new();
        if is_enum {
            let variants = self.enums[name].decl.variants.clone();
            concrete_fields.push(ast::Field {
                name: "__nova_tag".into(),
                ty: TypeRef::Named("i32".into()),
                default: None,
                attributes: vec![],
            });
            let mut metas = vec![];
            for (variant_tag, v) in variants.into_iter().enumerate() {
                let mut payload = vec![];
                for (i, typ) in v.payload.iter().enumerate() {
                    let ty = self.resolved_type(typ, &ns, &subst)?;
                    // Preserve the discriminant in compiler-private field names.
                    // Needed by HIR's active-variant Drop and ownership lowering;
                    // empty variants need no payload fields or destructors.
                    let field = format!("__nova_variant_{variant_tag}_{}_{}", v.name, i);
                    concrete_fields.push(ast::Field {
                        name: field.clone(),
                        ty: ty.clone(),
                        default: None,
                        attributes: vec![],
                    });
                    payload.push((field, ty));
                }
                metas.push((v.name, payload));
            }
            self.enum_meta.insert(
                concrete.clone(),
                EnumMeta {
                    name: name.into(),
                    variants: metas,
                },
            );
        } else {
            // C++-style base-prefix layout: an inherited field precedes every
            // derived field and remains directly addressable on a derived value.
            if let Some(parent) = self.structs.get(name).and_then(|s| s.decl.parent.clone()) {
                let specialized = substitute(&parent, &subst);
                let base_ty = self.resolved_type(&specialized, &ns, &HashMap::new())?;
                let TypeRef::Named(base_name) = base_ty else {
                    return Err("internal base specialization error".into());
                };
                let inherited = self
                    .concrete_fields
                    .get(&base_name)
                    .ok_or_else(|| format!("missing inherited fields of '{base_name}'"))?
                    .clone();
                concrete_fields.extend(inherited);
            }
            for field in fields {
                if concrete_fields.iter().any(|f| f.name == field.name) {
                    return Err(format!(
                        "duplicate inherited field '{}.{}'",
                        concrete, field.name
                    ));
                }
                concrete_fields.push(ast::Field {
                    name: field.name,
                    ty: self.resolved_type(&field.ty, &ns, &subst)?,
                    default: field.default.map(|mut expr| {
                        inherit_expr(&mut expr, &subst);
                        expr
                    }),
                    attributes: field.attributes,
                });
            }
        }
        self.concrete_fields
            .insert(concrete.clone(), concrete_fields.clone());
        let mut emits: Vec<String> = vec![];
        for a in &attributes {
            if a.name == "derive" {
                for d in &a.args {
                    match d.as_str() {
                        "Serialize" => {
                            self.serial.insert(concrete.clone());
                            emits.push("Serialize".into());
                        }
                        "Deserialize" => {
                            self.deserial.insert(concrete.clone());
                            emits.push("Deserialize".into());
                        }
                        "Json" => {
                            self.serial.insert(concrete.clone());
                            self.deserial.insert(concrete.clone());
                            emits.push("Serialize".into());
                            emits.push("Deserialize".into());
                        }
                        "Debug" => {
                            self.serial.insert(concrete.clone());
                            emits.push("Serialize".into());
                        }
                        "Clone" => {}
                        "Default" => {
                            self.defaultable.insert(concrete.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        emits.sort();
        emits.dedup();
        self.items.push(Item::Struct(ast::Struct {
            name: concrete.clone(),
            generics: vec![],
            concept_bounds: vec![],
            fields: concrete_fields.clone(),
            attributes: {
                let mut lowered_attributes = attributes.iter()
                    .filter(|attribute| attribute.name == "repr")
                    .cloned()
                    .collect::<Vec<_>>();
                if !emits.is_empty() {
                    lowered_attributes.push(ast::Attribute {
                        name: "derive".into(),
                        args: emits,
                    });
                }
                lowered_attributes
            },
            is_class: self
                .structs
                .get(name)
                .map(|s| s.decl.is_class)
                .unwrap_or(false),
            parent: None,
        }));
        let name_ty = TypeRef::Named(concrete.clone());
        if self.defaultable.contains(&concrete) {
            let mut fields = vec![];
            for f in &concrete_fields {
                fields.push((f.name.clone(), self.zero_expr(&f.ty)?));
            }
            self.items.push(Item::Function(ast::Function {
                mode: ast::FunctionMode::Normal,
                name: format!("{concrete}_default"),
                generics: vec![],
                concept_bounds: vec![],
                params: vec![],
                variadic: false,
                result: name_ty.clone(),
                body: vec![Stmt::Return(Some(Expr::StructInit(
                    concrete.clone(),
                    fields,
                )))],
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            }));
        }
        if attributes
            .iter()
            .any(|a| a.name == "derive" && a.args.contains(&"Clone".into()))
        {
            if concrete_fields
                .iter()
                .any(|f| matches!(f.ty, TypeRef::Pointer { .. }))
            {
                return Err(format!(
                    "#[derive(Clone)] on '{concrete}' cannot implicitly clone raw pointers"
                ));
            }
            self.items.push(Item::Function(ast::Function {
                mode: ast::FunctionMode::Normal,
                name: format!("{concrete}_clone"),
                generics: vec![],
                concept_bounds: vec![],
                params: vec![ast::Param {
                    name: "value".into(),
                    ty: name_ty.clone(),
                    default: None,
                    attributes: vec![],
                }],
                variadic: false,
                result: name_ty.clone(),
                body: vec![Stmt::Return(Some(Expr::Var("value".into())))],
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            }));
            let cloned_fields = concrete_fields
                .iter()
                .map(|field| {
                    (
                        field.name.clone(),
                        Expr::Field(
                            Box::new(Expr::Deref(Box::new(Expr::Var("self".into())))),
                            field.name.clone(),
                        ),
                    )
                })
                .collect();
            self.register_generated_impl(
                name_ty.clone(),
                "Clone",
                vec![ast::Function {
                    mode: ast::FunctionMode::Normal,
                    name: "clone".into(),
                    generics: vec![],
                    concept_bounds: vec![],
                    params: vec![ast::Param {
                        name: "self".into(),
                        ty: TypeRef::Reference {
                            mutable: false,
                            inner: Box::new(name_ty.clone()),
                        },
                        default: None,
                        attributes: vec![],
                    }],
                    variadic: false,
                    result: name_ty.clone(),
                    body: vec![Stmt::Return(Some(Expr::StructInit(
                        concrete.clone(),
                        cloned_fields,
                    )))],
                    is_virtual: false,
                    is_override: false,
                    attributes: vec![],
                }],
            )?;
        }
        if attributes
            .iter()
            .any(|a| a.name == "derive" && a.args.contains(&"Debug".into()))
        {
            self.items.push(Item::Function(ast::Function {
                mode: ast::FunctionMode::Normal,
                name: format!("{concrete}_debug"),
                generics: vec![],
                concept_bounds: vec![],
                params: vec![ast::Param {
                    name: "value".into(),
                    ty: name_ty.clone(),
                    default: None,
                    attributes: vec![],
                }],
                variadic: false,
                result: TypeRef::Named("i32".into()),
                body: vec![Stmt::Return(Some(Expr::Call(
                    format!("{concrete}_write_json"),
                    vec![Expr::Var("value".into())],
                )))],
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            }));
            self.register_generated_impl(
                name_ty.clone(),
                "Debug",
                vec![ast::Function {
                    mode: ast::FunctionMode::Normal,
                    name: "debug_string".into(),
                    generics: vec![],
                    concept_bounds: vec![],
                    params: vec![ast::Param {
                        name: "self".into(),
                        ty: TypeRef::Reference {
                            mutable: false,
                            inner: Box::new(name_ty.clone()),
                        },
                        default: None,
                        attributes: vec![],
                    }],
                    variadic: false,
                    result: TypeRef::Named("str".into()),
                    body: vec![Stmt::Return(Some(Expr::Call(
                        format!("{concrete}_to_json"),
                        vec![Expr::Deref(Box::new(Expr::Var("self".into())))],
                    )))],
                    is_virtual: false,
                    is_override: false,
                    attributes: vec![],
                }],
            )?;
        }
        // A monomorphized owner must bring its destructor into the concrete
        // function table even if the user never calls `.drop()` explicitly.
        // Typed HIR only inserts RAII cleanup for *defined* drop signatures.
        // Without this step, `Vec<i64>` and `Map<i64,i64>` silently leak.
        if !args.is_empty() && !is_enum {
            let destructor = format!("{name}::drop");
            if let Some(impl_definition) = self.funcs.get(&destructor) {
                if impl_definition.impl_generics.len() == args.len()
                    && impl_definition.decl.generics.is_empty()
                {
                    let pending = (destructor, args.to_vec());
                    if !self.pending_destructors.contains(&pending) {
                        self.pending_destructors.push(pending);
                    }
                }
            }
        }
        Ok(TypeRef::Named(concrete))
    }
    fn zero_expr(&mut self, ty: &TypeRef) -> Result<Expr, String> {
        match ty {
            TypeRef::Named(n) if n == "bool" => Ok(Expr::Bool(false)),
            TypeRef::Named(n) if n == "str" => Ok(Expr::Str("\"\"".into())),
            TypeRef::Named(n) if n == "char" => Ok(Expr::Cast(
                Box::new(Expr::Int("0".into())),
                TypeRef::Named("char".into()),
            )),
            TypeRef::Named(n) if n == "f32" || n == "f64" => Ok(Expr::Float("0.0".into())),
            TypeRef::Named(n)
                if [
                    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "isize", "usize",
                ]
                .contains(&n.as_str()) =>
            {
                Ok(Expr::Int("0".into()))
            }
            TypeRef::Named(n) if self.defaultable.contains(n) => {
                Ok(Expr::Call(format!("{n}_default"), vec![]))
            }
            TypeRef::Array(inner, len) => {
                if *len > 1024 {
                    return Err(
                        "derived Default currently supports arrays with at most 1024 elements"
                            .into(),
                    );
                }
                let mut a = vec![];
                for _ in 0..*len {
                    a.push(self.zero_expr(inner)?);
                }
                Ok(Expr::Array(a))
            }
            _ => Err(format!(
                "type '{}' has no safe default value",
                display_type(ty)
            )),
        }
    }
    fn instantiate_function(
        &mut self,
        name: &str,
        args: &[TypeRef],
        pack_size: Option<usize>,
    ) -> Result<(String, TypeRef), String> {
        if self.func_cache.len() > 2048 {
            return Err("function monomorphization limit (2048) exceeded".into());
        }
        let template = self
            .funcs
            .get(name)
            .ok_or_else(|| format!("unknown function '{name}'"))?
            .clone();
        if template.decl.variadic != pack_size.is_some() {
            return Err(format!(
                "internal variadic specialization mismatch in '{name}'"
            ));
        }
        let mut key = if args.is_empty() {
            ident(name)
        } else {
            format!(
                "{}__{}",
                ident(name),
                args.iter().map(type_suffix).collect::<Vec<_>>().join("__")
            )
        };
        // Generic instance destructors must use the concrete struct's name
        // followed by `__drop`. HIR's automatic cleanup looks up precisely
        // `format!("{}__drop", struct_info.name)`. Previously the method
        // was emitted as `Map__drop__i64__i64` while the owner was named
        // `Map__i64__i64`, so automatic destruction silently did not run.
        if let Some(owner) = name.strip_suffix("::drop") {
            if !template.impl_generics.is_empty() && template.decl.generics.is_empty() {
                key = format!(
                    "{}__{}__drop",
                    ident(owner),
                    args.iter().map(type_suffix).collect::<Vec<_>>().join("__")
                );
            }
        }
        if let Some(n) = pack_size {
            key.push_str(&format!("__pack_{n}"));
        }
        let generic = template
            .impl_generics
            .iter()
            .chain(&template.decl.generics)
            .cloned()
            .collect::<Vec<_>>();
        if generic.len() != args.len() {
            return Err(format!(
                "function '{name}' expects {} template parameters, got {}",
                generic.len(),
                args.len()
            ));
        }
        let subst = generic
            .into_iter()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        // Fail at specialization, before HIR, MIR and code generation.
        self.concepts
            .check(&template.decl.concept_bounds, &template.namespace, &subst, name)
            .map_err(|error| {
                if let Some(helper) = name.split("__nova_macro_concept_check__").nth(1) {
                    // Error belongs to the caller's macro argument, not a user
                    // function: keep the underlying constraint detail intact.
                    format!(
                        "error[NOVA-MAC020]: macro argument does not satisfy its concept ({}):\n  = {error}",
                        helper.replace("__", " ")
                    )
                } else {
                    error
                }
            })?;
        let opaque_trait = match &template.decl.result {
            TypeRef::Named(name) => impl_trait_name(name).map(str::to_owned),
            _ => None,
        };
        let mut result = if opaque_trait.is_some() {
            // Infer the hidden concrete type after parameters are registered.
            // This placeholder never reaches HIR or LLVM.
            TypeRef::Named("()".into())
        } else {
            self.resolved_type(&template.decl.result, &template.namespace, &subst)?
        };
        if !self.func_cache.insert(key.clone()) {
            return Ok((key, result));
        }
        let mut params = vec![];
        let mut scope = Scope {
            vars: HashMap::new(),
            namespace: template.namespace.clone(),
            subst: subst.clone(),
            returns: result.clone(),
            constructor_base: None,
        };
        if template.decl.name == "new" {
            if let Some((owner, _)) = name.rsplit_once("::") {
                if let Some(structure) = self.structs.get(owner) {
                    if let Some(parent) = &structure.decl.parent {
                        let parent = substitute(parent, &scope.subst);
                        scope.constructor_base =
                            Some(self.resolved_type(&parent, &template.namespace, &scope.subst)?);
                    }
                }
            }
        }
        for (i, p) in template.decl.params.iter().enumerate() {
            let mut ty = self.resolved_type(&p.ty, &template.namespace, &subst)?;
            if template.decl.variadic && i + 1 == template.decl.params.len() {
                ty = TypeRef::Array(Box::new(ty), pack_size.expect("checked pack size"));
            }
            if scope.vars.insert(p.name.clone(), ty.clone()).is_some() {
                return Err(format!("duplicate parameter '{}' in '{name}'", p.name));
            }
            params.push(ast::Param {
                name: p.name.clone(),
                ty,
                default: None,
                attributes: vec![],
            });
        }
        let mut function_body = template.decl.body.clone();
        if let Some(trait_name) = opaque_trait {
            let candidate = function_body
                .iter()
                .rev()
                .find_map(|statement| match statement {
                    Stmt::Return(Some(value)) => Some(value),
                    Stmt::Tail(value) => Some(value),
                    _ => None,
                })
                .ok_or_else(|| {
                    format!("function '{name}' returning impl {trait_name} must return a value")
                })?;
            let (_, inferred) = self.expression(candidate, &scope, None)?;
            let dyn_trait = TypeRef::Named(format!("dyn {trait_name}"));
            if !self.accepts_dyn_trait(&inferred, &dyn_trait, &template.namespace) {
                return Err(format!(
                    "type {} does not implement trait '{}' required by impl Trait",
                    display_type(&inferred),
                    trait_name
                ));
            }
            result = inferred;
            scope.returns = result.clone();
        }

        // Preserve the real monomorphization/call-lowering chain for diagnostics.
        // These frames are compile-time context, not a runtime stack trace.
        if let Some(Stmt::Tail(value)) = function_body.last().cloned() {
            function_body.pop();
            if result == TypeRef::Named("()".into()) {
                function_body.push(Stmt::Expr(value));
            } else {
                function_body.push(Stmt::Return(Some(value)));
            }
        }
        let body = self
            .statements(&function_body, &mut scope)
            .map_err(|error| format!("{error}\n  = nova-trace: specializing function '{name}'"))?;
        self.items.push(Item::Function(ast::Function {
            mode: ast::FunctionMode::Normal,
            name: key.clone(),
            generics: vec![],
            concept_bounds: vec![],
            params,
            variadic: false,
            result: result.clone(),
            body,
            is_virtual: false,
            is_override: false,
            attributes: template.decl.attributes.clone(),
        }));
        Ok((key, result))
    }
    fn statements(&mut self, stmts: &[Stmt], scope: &mut Scope) -> Result<Vec<Stmt>, String> {
        let mut output = vec![];
        for s in stmts {
            match s {
                Stmt::Let {
                    name,
                    mutable,
                    ty,
                    init,
                    defer,
                } => {
                    let expected = ty
                        .as_ref()
                        .map(|x| self.resolved_type(x, &scope.namespace, &scope.subst))
                        .transpose()?;
                    let (init, got) = self.expression(init, scope, expected.as_ref())?;
                    if let Some(ref t) = expected {
                        if t != &got {
                            return Err(format!(
                                "local '{name}' expected {}, found {}",
                                display_type(t),
                                display_type(&got)
                            ));
                        }
                    }
                    if scope.vars.contains_key(name) {
                        return Err(format!("duplicate local '{name}'"));
                    }
                    let final_ty = expected.unwrap_or(got);
                    scope.vars.insert(name.clone(), final_ty.clone());
                    output.push(Stmt::Let {
                        name: name.clone(),
                        mutable: *mutable,
                        ty: Some(final_ty),
                        init,
                        defer: defer
                            .as_ref()
                            .map(|e| self.expression(e, scope, None).map(|(e, _)| e))
                            .transpose()?,
                    });
                }
                Stmt::Set { name, value } => {
                    if name == "__nova_let_else_must_diverge__" {
                        return Err("let-else requires its else branch to exit with return, break or continue".into());
                    }
                    let actual_name = name
                        .strip_prefix(crate::ast::REPLACE_PREFIX)
                        .unwrap_or(name);
                    let expected = scope
                        .vars
                        .get(actual_name)
                        .ok_or_else(|| format!("unknown local '{actual_name}'"))?
                        .clone();
                    let (value, _) = self.expression(value, scope, Some(&expected))?;
                    output.push(Stmt::Set {
                        name: name.clone(),
                        value,
                    });
                }
                Stmt::DerefSet { name, value } => {
                    let actual = scope
                        .vars
                        .get(name)
                        .ok_or_else(|| format!("unknown pointer or borrow '{name}'"))?;
                    let target = match actual {
                        TypeRef::Pointer {
                            mutable: true,
                            inner,
                        }
                        | TypeRef::Reference {
                            mutable: true,
                            inner,
                        } => inner.as_ref().clone(),
                        _ => {
                            return Err(format!(
                                "assignment through '*{name}' requires &mut T or *mut T"
                            ))
                        }
                    };
                    let (value, _) = self.expression(value, scope, Some(&target))?;
                    output.push(Stmt::DerefSet {
                        name: name.clone(),
                        value,
                    });
                }
                Stmt::FieldSet {
                    root,
                    field,
                    op,
                    value,
                } => {
                    if root == "__nova_invalid_assignment_target__" {
                        return Err(format!("invalid assignment target: {field}; expected `variable` or `variable.field`"));
                    }
                    let root_ty = scope
                        .vars
                        .get(root)
                        .ok_or_else(|| format!("unknown local '{root}'"))?
                        .clone();
                    let struct_ty = match &root_ty {
                        TypeRef::Pointer {
                            mutable: true,
                            inner,
                        }
                        | TypeRef::Reference {
                            mutable: true,
                            inner,
                        } => inner.as_ref(),
                        TypeRef::Pointer { mutable: false, .. }
                        | TypeRef::Reference { mutable: false, .. } => {
                            return Err(format!("cannot modify field of const pointer '{root}'"))
                        }
                        _ => &root_ty,
                    };
                    let TypeRef::Named(name) = struct_ty else {
                        return Err(format!("'{root}' is not a struct"));
                    };
                    let fields = self
                        .concrete_fields
                        .get(name)
                        .ok_or_else(|| format!("'{name}' has no fields"))?;
                    let found = fields
                        .iter()
                        .find(|f| f.name == *field)
                        .map(|f| f.ty.clone());
                    if found.is_none() {
                        // Computed-property setters are normal Nova instance
                        // methods. Their &mut Self requirement is enforced by
                        // the existing borrow checker; no hidden storage.
                        let rhs = if let Some(binary) = op {
                            Expr::Binary(
                                Box::new(Expr::Field(
                                    Box::new(Expr::Var(root.clone())),
                                    field.clone(),
                                )),
                                *binary,
                                Box::new(value.clone()),
                            )
                        } else {
                            value.clone()
                        };
                        let setter = Expr::MethodCall(
                            Box::new(Expr::Var(root.clone())),
                            format!("set_{field}"),
                            vec![rhs],
                            vec![],
                        );
                        let (call, result) =
                            self.expression(&setter, scope, None).map_err(|error| {
                                format!("'{name}' has no mutable property '{field}': {error}")
                            })?;
                        if result != TypeRef::Named("()".into()) {
                            return Err(format!(
                                "property setter '{name}::set_{field}' must return ()"
                            ));
                        }
                        output.push(Stmt::Expr(call));
                        continue;
                    }
                    let expected = found.unwrap();
                    let rhs = if let Some(binary) = op {
                        Expr::Binary(
                            Box::new(Expr::Field(
                                Box::new(Expr::Var(root.clone())),
                                field.clone(),
                            )),
                            *binary,
                            Box::new(value.clone()),
                        )
                    } else {
                        value.clone()
                    };
                    let (value, _) = self.expression(&rhs, scope, Some(&expected))?;
                    output.push(Stmt::FieldSet {
                        root: root.clone(),
                        field: field.clone(),
                        op: None,
                        value,
                    });
                }
                Stmt::CompoundSet { name, op, value } => {
                    let expected = scope
                        .vars
                        .get(name)
                        .ok_or_else(|| format!("unknown local '{name}'"))?
                        .clone();
                    // Desugar through the same operator solver as ordinary
                    // `a | b`: including #[operator]-backed user types.
                    let combined = Expr::Binary(
                        Box::new(Expr::Var(name.clone())),
                        *op,
                        Box::new(value.clone()),
                    );
                    let (value, result_type) =
                        self.expression(&combined, scope, Some(&expected))?;
                    if result_type != expected {
                        return Err(format!(
                            "compound operator on '{name}' returns {}, expected {}",
                            display_type(&result_type),
                            display_type(&expected)
                        ));
                    }
                    output.push(Stmt::Set {
                        name: name.clone(),
                        value,
                    });
                }
                Stmt::Return(value) => {
                    let value = value
                        .as_ref()
                        .map(|e| {
                            self.expression(e, scope, Some(&scope.returns))
                                .map(|(e, _)| e)
                        })
                        .transpose()?;
                    output.push(Stmt::Return(value));
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let (cond, _) =
                        self.expression(condition, scope, Some(&TypeRef::Named("bool".into())))?;
                    let then_body = self.statements(then_body, &mut scope.clone())?;
                    let else_body = else_body
                        .as_ref()
                        .map(|b| self.statements(b, &mut scope.clone()))
                        .transpose()?;
                    output.push(Stmt::If {
                        condition: cond,
                        then_body,
                        else_body,
                    });
                }
                // Every surface ForEach is lowered by iterators::desugar before
                // monomorphization. A residual node is a compiler pipeline bug.
                Stmt::ForEach { .. } => {
                    return Err(
                        "compiler bug: for-in iterator was not desugared before surface typing"
                            .into(),
                    )
                }
                Stmt::While { condition, body } => {
                    let (cond, _) =
                        self.expression(condition, scope, Some(&TypeRef::Named("bool".into())))?;
                    output.push(Stmt::While {
                        condition: cond,
                        body: self.statements(body, &mut scope.clone())?,
                    });
                }
                Stmt::Loop(body) => {
                    output.push(Stmt::Loop(self.statements(body, &mut scope.clone())?))
                }
                Stmt::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    let (start, ty) = self.expression(start, scope, None)?;
                    if !is_integer(&ty) {
                        return Err("for range requires integer operands".into());
                    }
                    let (end, upper_ty) = self.expression(end, scope, Some(&ty))?;
                    if ty != upper_ty {
                        return Err(format!("for range bounds must have equal types; variable '{name}' starts as {}, but ends as {}", display_type(&ty), display_type(&upper_ty)));
                    }
                    let mut child = scope.clone();
                    if child.vars.contains_key(name) {
                        return Err(format!(
                            "for variable '{name}' shadows an existing local; choose another name"
                        ));
                    }
                    child.vars.insert(name.clone(), ty);
                    output.push(Stmt::For {
                        name: name.clone(),
                        start,
                        end,
                        body: self.statements(body, &mut child)?,
                    });
                }
                Stmt::Break => output.push(Stmt::Break),
                Stmt::Continue => output.push(Stmt::Continue),
                Stmt::Unsafe(body) => {
                    output.push(Stmt::Unsafe(self.statements(body, &mut scope.clone())?))
                }
                Stmt::Defer(e) => output.push(Stmt::Defer(self.expression(e, scope, None)?.0)),
                Stmt::Expr(e) => output.push(Stmt::Expr(self.expression(e, scope, None)?.0)),
                Stmt::Tail(e) => output.push(Stmt::Expr(self.expression(e, scope, None)?.0)),
                Stmt::Match { value, arms } => {
                    let (value, ty) = self.expression(value, scope, None)?;
                    // Matching an enum through `&T` / `&mut T` is a borrow:
                    // keep the reference in the synthetic temporary and bind
                    // payloads as references so no owning field is moved out.
                    // Preserve the mutability of the original reference.
                    // `None` means an owning match; `Some(false)` is `&T`;
                    // `Some(true)` is `&mut T`.
                    let (type_name, borrowed_mut) = match &ty {
                        TypeRef::Named(name) => (name.clone(), None),
                        TypeRef::Reference { mutable, inner } => match inner.as_ref() {
                            TypeRef::Named(name) => (name.clone(), Some(*mutable)),
                            _ => return Err("match reference must point to an enum".into()),
                        },
                        _ => return Err("match currently requires an enum value or reference to an enum".into()),
                    };
                    let meta = self
                        .enum_meta
                        .get(&type_name)
                        .ok_or_else(|| {
                            format!("match requires enum; '{type_name}' is not an enum")
                        })?
                        .clone();
                    if arms.is_empty() {
                        return Err("match must have at least one arm".into());
                    }
                    let tmp = format!("__nova_match_{}", self.counter);
                    self.counter += 1;
                    output.push(Stmt::Let {
                        name: tmp.clone(),
                        mutable: false,
                        ty: Some(ty.clone()),
                        init: value,
                        defer: None,
                    });
                    let mut covered = HashSet::new();
                    let mut alternatives: Vec<(Option<Expr>, Vec<Stmt>)> = vec![];
                    for (index, arm) in arms.iter().enumerate() {
                        if (matches!(arm.pattern, Pattern::Wildcard)
                            || matches!(&arm.pattern,Pattern::Variant{path,bindings} if path=="_" && bindings.is_empty()))
                            && index + 1 != arms.len()
                        {
                            return Err("wildcard match arm must be last".into());
                        }
                        match &arm.pattern {
                            Pattern::Wildcard => alternatives
                                .push((None, self.statements(&arm.body, &mut scope.clone())?)),
                            Pattern::Variant { path, bindings }
                                if path == "_" && bindings.is_empty() =>
                            {
                                alternatives
                                    .push((None, self.statements(&arm.body, &mut scope.clone())?))
                            }
                            Pattern::Variant { path, bindings } => {
                                let name = path.rsplit("::").next().unwrap();
                                let declared = path.rsplit_once("::").map(|x| x.0).unwrap_or("");
                                if !declared.is_empty()
                                    && self.qualify(declared, &scope.namespace) != meta.name
                                {
                                    return Err(format!(
                                        "match pattern '{path}' is not a variant of '{}'",
                                        meta.name
                                    ));
                                }
                                let tag = meta
                                    .variants
                                    .iter()
                                    .position(|x| x.0 == name)
                                    .ok_or_else(|| format!("unknown enum variant '{path}'"))?;
                                if !covered.insert(name.to_string()) {
                                    return Err(format!("duplicate match arm '{name}'"));
                                }
                                let fields = &meta.variants[tag].1;
                                if bindings.len() != fields.len() {
                                    return Err(format!(
                                        "variant '{name}' expects {} bindings, got {}",
                                        fields.len(),
                                        bindings.len()
                                    ));
                                }
                                let mut child = scope.clone();
                                let mut prefix = vec![];
                                for (binding, (field, field_ty)) in bindings.iter().zip(fields) {
                                    // `Ok(())` / `Some(())` has no materialized payload.
                                    // The binding is substituted with Expr::Unit below;
                                    // emitting a unit-typed HIR local is invalid.
                                    if *field_ty == TypeRef::Named("()".into()) {
                                        child.vars.insert(binding.clone(), field_ty.clone());
                                        continue;
                                    }
                                    let field_expr = Expr::Field(
                                        Box::new(Expr::Var(tmp.clone())),
                                        field.clone(),
                                    );
                                    let (binding_ty, init) = if let Some(mutable) = borrowed_mut {
                                        let reference_ty = TypeRef::Reference {
                                            mutable,
                                            inner: Box::new(field_ty.clone()),
                                        };
                                        (
                                            reference_ty,
                                            Expr::AddressOf {
                                                mutable,
                                                value: Box::new(field_expr),
                                            },
                                        )
                                    } else {
                                        (field_ty.clone(), field_expr)
                                    };
                                    child.vars.insert(binding.clone(), binding_ty.clone());
                                    prefix.push(Stmt::Let {
                                        name: binding.clone(),
                                        mutable: false,
                                        ty: Some(binding_ty),
                                        init,
                                        defer: None,
                                    });
                                }
                                prefix.extend(self.statements(&arm.body, &mut child)?);
                                let condition = Expr::Binary(
                                    Box::new(Expr::Field(
                                        Box::new(Expr::Var(tmp.clone())),
                                        "__nova_tag".into(),
                                    )),
                                    BinaryOp::Eq,
                                    Box::new(Expr::Int(tag.to_string())),
                                );
                                alternatives.push((Some(condition), prefix));
                            }
                        }
                    }
                    let complete = alternatives.iter().any(|x| x.0.is_none())
                        || covered.len() == meta.variants.len();
                    if !complete {
                        let missing = meta
                            .variants
                            .iter()
                            .filter(|x| !covered.contains(&x.0))
                            .map(|x| x.0.clone())
                            .collect::<Vec<_>>();
                        return Err(format!(
                            "non-exhaustive match, missing {}",
                            missing.join(", ")
                        ));
                    }
                    // Exhaustiveness makes the last arm an unconditional else branch.
                    let (_, last) = alternatives.pop().unwrap();
                    let mut branch = last;
                    for (condition, body) in alternatives.into_iter().rev() {
                        branch = vec![Stmt::If {
                            condition: condition.unwrap_or(Expr::Bool(true)),
                            then_body: body,
                            else_body: Some(branch),
                        }];
                    }
                    output.extend(branch);
                }
            }
        }
        Ok(output)
    }
    /// Expand only named, fixed-size arrays; never duplicate a side-effecting expression.
    /// An individual expanded element is evaluated exactly once by normal call lowering.
    fn expand_call_args(&self, args: &[Expr], scope: &Scope) -> Result<Vec<Expr>, String> {
        let mut out = Vec::new();
        for arg in args {
            match arg {
                Expr::Spread(value) => {
                    let Expr::Var(name) = value.as_ref() else {
                        return Err("spread requires a named fixed-size array (evaluate expressions into a let first)".into());
                    };
                    let ty = scope
                        .vars
                        .get(name)
                        .ok_or_else(|| format!("unknown spread variable '{name}'"))?;
                    let TypeRef::Array(_, len) = ty else {
                        return Err(format!(
                            "spread operand '{name}' must be an array or variadic pack"
                        ));
                    };
                    if *len > 1024 {
                        return Err("spread limit is 1024 elements".into());
                    }
                    for i in 0..*len {
                        out.push(Expr::Index(
                            Box::new(Expr::Var(name.clone())),
                            Box::new(Expr::Int(i.to_string())),
                        ));
                    }
                }
                other => out.push(other.clone()),
            }
        }
        Ok(out)
    }
    // LP64 Linux ABI layout used by LLVM on the stage-0 target. All native
    // allocations made from Nova must use this intrinsic, not guessed sizes.
    fn layout_of(&self, ty: &TypeRef, visited: &mut Vec<String>) -> Result<(u64, u64), String> {
        fn align_up(size: u64, alignment: u64) -> Result<u64, String> {
            let padding = (alignment - size % alignment) % alignment;
            size.checked_add(padding)
                .ok_or_else(|| "native layout size overflow".into())
        }
        match ty {
            TypeRef::Pointer { .. } | TypeRef::Reference { .. } | TypeRef::Function { .. } => {
                Ok((8, 8))
            }
            TypeRef::Named(name) if dyn_trait_name(name).is_some() => Ok((16, 8)),
            TypeRef::Named(name) => match name.as_str() {
                "bool" | "i8" | "u8" => Ok((1, 1)),
                "i16" | "u16" => Ok((2, 2)),
                "i32" | "u32" | "f32" | "char" => Ok((4, 4)),
                "i64" | "u64" | "isize" | "usize" | "f64" | "str" => Ok((8, 8)),
                "()" => Err("unit values cannot be allocated as records".into()),
                _ => {
                    if visited.contains(name) {
                        return Err(format!("recursive record layout for '{name}'"));
                    }
                    let fields = self
                        .concrete_fields
                        .get(name)
                        .ok_or_else(|| format!("cannot determine native layout of '{name}'"))?;
                    visited.push(name.clone());
                    let mut size: u64 = 0;
                    let mut alignment: u64 = 1;
                    for field in fields {
                        let (field_size, field_alignment) = self.layout_of(&field.ty, visited)?;
                        size = align_up(size, field_alignment)?;
                        size = size
                            .checked_add(field_size)
                            .ok_or("native layout size overflow")?;
                        alignment = alignment.max(field_alignment);
                    }
                    visited.pop();
                    Ok((align_up(size, alignment)?, alignment))
                }
            },
            TypeRef::Array(element, count) => {
                let (size, alignment) = self.layout_of(element, visited)?;
                let total = size
                    .checked_mul(*count as u64)
                    .ok_or("native array size overflow")?;
                Ok((total, alignment))
            }
            TypeRef::Applied(name, _) => Err(format!("unresolved generic layout '{name}'")),
        }
    }
    fn expression(
        &mut self,
        e: &Expr,
        scope: &Scope,
        expected: Option<&TypeRef>,
    ) -> Result<(Expr, TypeRef), String> {
        let (value, actual) = self.expression_inner(e, scope, expected)?;
        if let Some(want) = expected {
            if self.accepts_dyn_trait(&actual, want, &scope.namespace) {
                // Trait-object coercion is represented at the surface boundary;
                // the backend will attach the data/vtable pair during dynamic
                // dispatch lowering.
                return Ok((value, want.clone()));
            }
            // C string literals are NUL-terminated static byte arrays. In a
            // `*const u8` context they decay directly to a const byte pointer;
            // this is deliberately restricted to literals (not arbitrary
            // managed `str` values), whose storage has static lifetime.
            let c_string_literal = matches!(&value, Expr::Str(_))
                && actual == TypeRef::Named("str".into())
                && matches!(want, TypeRef::Pointer { mutable: false, inner }
                    if **inner == TypeRef::Named("u8".into()));
            if c_string_literal {
                // Keep the literal node intact. HIR sees the expected pointer
                // type and emits the literal as a pointer to its NUL-terminated
                // global constant; no runtime conversion is required.
                return Ok((value, want.clone()));
            }
            if &actual != want
                && (implicit_numeric_widen(&actual, want)
                    || implicit_shared_reborrow(&actual, want))
            {
                return Ok((Expr::Cast(Box::new(value), want.clone()), want.clone()));
            }
        }
        Ok((value, actual))
    }
    /// Type a block used as a value. Scope locals never escape; only the tail
    /// expression does. An explicit semicolon means unit, as in Rust.
    fn value_block(
        &mut self,
        body: &[Stmt],
        scope: &Scope,
        expected: Option<&TypeRef>,
        require_value: bool,
    ) -> Result<(Vec<Stmt>, TypeRef), String> {
        let mut inner = scope.clone();
        let (prefix, tail) = match body.split_last() {
            Some((Stmt::Tail(value), prefix)) => (prefix, Some(value)),
            _ => (body, None),
        };
        let mut output = self.statements(prefix, &mut inner)?;
        match tail {
            Some(value) => {
                let (value, ty) = self.expression(value, &inner, expected)?;
                output.push(Stmt::Tail(value));
                Ok((output, ty))
            }
            None if !require_value => Ok((output, TypeRef::Named("()".into()))),
            None => Err("value-producing if branch requires a final expression without ';'".into()),
        }
    }
    fn expression_inner(
        &mut self,
        e: &Expr,
        scope: &Scope,
        expected: Option<&TypeRef>,
    ) -> Result<(Expr, TypeRef), String> {
        let i32t = || TypeRef::Named("i32".into());
        match e {
            Expr::Yield(..) | Expr::Await(..) => {
                Err("compiler bug: suspension was not desugared".into())
            }
            Expr::If {
                condition,
                then_body,
                else_body,
            } => {
                let (condition, cond_ty) =
                    self.expression(condition, scope, Some(&TypeRef::Named("bool".into())))?;
                if cond_ty != TypeRef::Named("bool".into()) {
                    return Err("if expression condition must be bool".into());
                }
                let (then_body, branch_ty) = self.value_block(then_body, scope, expected, true)?;
                let (else_body, other_ty) =
                    self.value_block(else_body, scope, Some(&branch_ty), true)?;
                if branch_ty != other_ty {
                    return Err(format!(
                        "if branches must yield identical types: {} vs {}",
                        display_type(&branch_ty),
                        display_type(&other_ty)
                    ));
                }
                Ok((
                    Expr::If {
                        condition: Box::new(condition),
                        then_body,
                        else_body,
                    },
                    branch_ty,
                ))
            }
            Expr::Block(body) => {
                let (body, ty) = self.value_block(body, scope, expected, false)?;
                Ok((Expr::Block(body), ty))
            }
            Expr::MacroCall(name, _) => Err(format!("macro '{name}!' was not expanded")),
            Expr::Spread(..) => {
                Err("pack expansion '...value' is only allowed in call argument lists".into())
            }
            Expr::Int(_) => Ok((
                e.clone(),
                expected
                    .cloned()
                    .filter(|x| is_integer(x))
                    .unwrap_or_else(i32t),
            )),
            Expr::Float(_) => Ok((
                e.clone(),
                expected
                    .cloned()
                    .filter(|x| matches!(x,TypeRef::Named(n) if n=="f32"||n=="f64"))
                    .unwrap_or(TypeRef::Named("f64".into())),
            )),
            Expr::Bool(_) => Ok((e.clone(), TypeRef::Named("bool".into()))),
            Expr::Str(_) => Ok((e.clone(), TypeRef::Named("str".into()))),
            Expr::Unit => Ok((e.clone(), TypeRef::Named("()".into()))),
            Expr::Lambda { .. } => Err("compiler bug: lambda lifting did not run".into()),
            Expr::FunctionRef(name) => {
                let q = self.qualify(name, &scope.namespace);
                if self.externs.contains_key(&q) {
                    return Err("extern C functions cannot be referenced as safe callbacks; create a typed Nova wrapper".into());
                }
                let func = self
                    .funcs
                    .get(&q)
                    .ok_or_else(|| format!("unknown function reference '{name}'"))?
                    .clone();
                if func.decl.variadic
                    || !func.decl.generics.is_empty()
                    || !func.impl_generics.is_empty()
                {
                    return Err(
                        "function reference requires non-generic, non-variadic function".into(),
                    );
                }
                let params = func
                    .decl
                    .params
                    .iter()
                    .map(|p| self.resolved_type(&p.ty, &func.namespace, &scope.subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let result = if let TypeRef::Named(declared) = &func.decl.result {
                    if impl_trait_name(declared).is_some() {
                        self.instantiate_function(&q, &[], None)?.1
                    } else {
                        self.resolved_type(&func.decl.result, &func.namespace, &scope.subst)?
                    }
                } else {
                    self.resolved_type(&func.decl.result, &func.namespace, &scope.subst)?
                };
                self.instantiate_function(&q, &[], None)?;
                Ok((
                    Expr::FunctionRef(ident(&q)),
                    TypeRef::Function {
                        params,
                        result: Box::new(result),
                    },
                ))
            }
            Expr::CallIndirect(callee, args) => {
                let (callee, ty) = self.expression(callee, scope, None)?;
                let TypeRef::Function { params, result } = ty else {
                    return Err("calling non-function".into());
                };
                if params.len() != args.len() {
                    return Err(format!(
                        "indirect call expects {} arguments, got {}",
                        params.len(),
                        args.len()
                    ));
                }
                let actual = args
                    .iter()
                    .zip(&params)
                    .map(|(a, t)| self.expression(a, scope, Some(t)).map(|x| x.0))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((Expr::CallIndirect(Box::new(callee), actual), *result))
            }
            Expr::Var(n) => {
                if let Some(ty) = scope.vars.get(n) {
                    if *ty == TypeRef::Named("()".into()) {
                        return Ok((Expr::Unit, ty.clone()));
                    }
                    return Ok((e.clone(), ty.clone()));
                }
                if self.funcs.contains_key(&self.qualify(n, &scope.namespace))
                    || self
                        .externs
                        .contains_key(&self.qualify(n, &scope.namespace))
                {
                    return self.expression_inner(&Expr::FunctionRef(n.clone()), scope, expected);
                }
                if self.lookup_variant(n, &scope.namespace, expected).is_some() {
                    return self.variant(n, &[], scope, expected);
                }
                Err(format!("unknown local '{n}'"))
            }
            Expr::Zero(t) => {
                let ty = self.resolved_type(t, &scope.namespace, &scope.subst)?;
                Ok((Expr::Zero(ty.clone()), ty))
            }
            Expr::Cast(value, target) => {
                // The AST preserves the single-quoted token for a precise
                // diagnostic. Lower it to a Unicode scalar in the char type.
                if *target == TypeRef::Named("char".into()) {
                    if let Expr::Str(raw) = value.as_ref() {
                        if raw.starts_with('\'') {
                            let scalar = crate::ast::decode_char_literal(raw)? as u32;
                            // Like numeric literals, char literals can be used
                            // directly in an integral context (e.g. byte_at(i)
                            // == '/'), without converting char variables.
                            if let Some(TypeRef::Named(name)) = expected {
                                let max = match name.as_str() {
                                    "i8" => Some(i8::MAX as u32),
                                    "u8" => Some(u8::MAX as u32),
                                    "i16" => Some(i16::MAX as u32),
                                    "u16" => Some(u16::MAX as u32),
                                    "i32" | "u32" | "i64" | "u64" | "isize" | "usize" => {
                                        Some(u32::MAX)
                                    }
                                    _ => None,
                                };
                                if let Some(max) = max {
                                    if scalar > max {
                                        return Err(format!("error[NOVA-CHAR003]: character U+{scalar:04X} does not fit {name}"));
                                    }
                                    return Ok((
                                        Expr::Int(scalar.to_string()),
                                        TypeRef::Named(name.clone()),
                                    ));
                                }
                            }
                            return Ok((
                                Expr::Cast(
                                    Box::new(Expr::Int(scalar.to_string())),
                                    TypeRef::Named("char".into()),
                                ),
                                TypeRef::Named("char".into()),
                            ));
                        }
                    }
                }
                let target = self.resolved_type(target, &scope.namespace, &scope.subst)?;
                let (value, from) = self.expression(value, scope, None)?;
                if !castable(&from, &target) {
                    return Err(format!(
                        "cannot cast {} to {}; numeric or explicit function-handle casts only",
                        display_type(&from),
                        display_type(&target)
                    ));
                }
                Ok((Expr::Cast(Box::new(value), target.clone()), target))
            }
            Expr::Unary(op, inner) => {
                let (inner, ty) = self.expression(inner, scope, expected)?;
                let result = if matches!(op, ast::UnaryOp::Not) {
                    TypeRef::Named("bool".into())
                } else {
                    ty
                };
                Ok((Expr::Unary(*op, Box::new(inner)), result))
            }
            Expr::Binary(a, op, b) => {
                // A comparison *produces* bool; its operands generally do not.
                // Passing the surrounding `if`/`while` expectation (`bool`) to
                // `sum() != 0` incorrectly requires `sum()` to return bool.
                // Infer the left operand independently, then use its type as
                // the contextual hint for the right operand (including literals).
                let comparison = matches!(
                    op,
                    BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::Lt
                        | BinaryOp::Le
                        | BinaryOp::Gt
                        | BinaryOp::Ge
                );
                let hint = if comparison { None } else { expected };
                let left_literal = if comparison
                    && matches!(a.as_ref(), Expr::Cast(inner, ty)
                    if *ty == TypeRef::Named("char".into())
                    && matches!(inner.as_ref(), Expr::Str(raw) if raw.starts_with('\'')))
                {
                    Some(a.as_ref().clone())
                } else {
                    None
                };
                let (mut a, mut ta) = self.expression(a, scope, hint)?;
                // Evaluate a non-literal RHS on its own: literal RHS keeps LHS context
                // so `i64_value + 2` does not force an unnecessary conversion.
                let right_hint = if matches!(b.as_ref(), Expr::Int(_) | Expr::Float(_))
                    || matches!(b.as_ref(), Expr::Cast(inner, ty)
                        if *ty == TypeRef::Named("char".into())
                        && matches!(inner.as_ref(), Expr::Str(raw) if raw.starts_with('\'')))
                {
                    Some(&ta)
                } else {
                    None
                };
                let (mut b, tb) = self.expression(b, scope, right_hint)?;
                // Literal-only contextual coercion works in either order:
                // byte_at(i) == '/' and '/' == byte_at(i) have identical types.
                if let Some(original) = left_literal {
                    if is_integer(&tb) {
                        (a, ta) = self.expression(&original, scope, Some(&tb))?;
                    }
                }
                // Struct operators dispatch through regular, typechecked trait
                // methods. There is no special-purpose LLVM/runtime operator.
                // Primitive operators always retain their native behavior.
                if let TypeRef::Named(receiver_type) = &ta {
                    if self.concrete_fields.contains_key(receiver_type) {
                        let key = ast::operator_key(*op);
                        let contract = key
                            .map(|key| self.concepts.operator_contract(key, &ta, &scope.namespace))
                            .transpose()?
                            .flatten();
                        let method = contract.map(|(_, m)| m).or(key);
                        if let Some(method) = method {
                            if let Some((trait_name, _)) = contract {
                                if !self.concepts.implements_trait(
                                    &ta,
                                    trait_name,
                                    &scope.namespace,
                                ) {
                                    return Err(format!("error[NOVA-OP002]: operator '{:?}' on '{receiver_type}' requires an explicit impl of '{trait_name}'",op));
                                }
                            }
                            let candidate = self.funcs.keys().any(|candidate| {
                                candidate.ends_with(&format!("::{method}"))
                                    && (receiver_type.as_str()
                                        == ident(candidate.rsplit_once("::").unwrap().0).as_str()
                                        || receiver_type.starts_with(&format!(
                                            "{}__",
                                            ident(candidate.rsplit_once("::").unwrap().0)
                                        )))
                            });
                            if !candidate {
                                return Err(format!("operator {op:?} on '{receiver_type}' requires a matching '{method}' method (typically via std::old::ops trait)"));
                            }
                            return self.expression(
                                &Expr::MethodCall(Box::new(a), method.into(), vec![b], vec![]),
                                scope,
                                expected,
                            );
                        }
                    }
                }
                let is_numeric_op = matches!(
                    op,
                    BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::Mul
                        | BinaryOp::Div
                        | BinaryOp::Rem
                        | BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::Lt
                        | BinaryOp::Le
                        | BinaryOp::Gt
                        | BinaryOp::Ge
                        | BinaryOp::BitAnd
                        | BinaryOp::BitOr
                        | BinaryOp::BitXor
                );
                let mut common = ta.clone();
                if ta != tb
                    && is_numeric_op
                    && numeric_info(&ta).is_some()
                    && numeric_info(&tb).is_some()
                {
                    let to = promote_numeric(&ta, &tb).ok_or_else(|| {
                        format!(
                            "no lossless implicit numeric conversion between {} and {}; use 'as'",
                            display_type(&ta),
                            display_type(&tb)
                        )
                    })?;
                    if ta != to {
                        a = Expr::Cast(Box::new(a), to.clone());
                    }
                    if tb != to {
                        b = Expr::Cast(Box::new(b), to.clone());
                    }
                    common = to;
                }
                let result = if comparison || matches!(op, BinaryOp::And | BinaryOp::Or) {
                    TypeRef::Named("bool".into())
                } else {
                    common
                };
                Ok((Expr::Binary(Box::new(a), *op, Box::new(b)), result))
            }
            Expr::Call(name, original_args) => {
                if name == "__nova_internal_unsafe_expr" {
                    if original_args.len() != 1 {
                        return Err("unsafe expression requires one block".into());
                    }
                    let (body, ty) = self.expression(&original_args[0], scope, expected)?;
                    return Ok((Expr::Call(name.clone(), vec![body]), ty));
                }
                if name == "super" {
                    let Some(parent) = &scope.constructor_base else {
                        return Err("super() is only available inside a derived constructor".into());
                    };
                    let TypeRef::Named(parent_name) = parent else {
                        return Err("super() requires a concrete base class".into());
                    };
                    let constructor = format!("{parent_name}::new");
                    if !self.funcs.contains_key(&constructor) {
                        return Err(format!(
                            "base class '{parent_name}' has no constructor for super()"
                        ));
                    }
                    return self.expression(
                        &Expr::Call(constructor, original_args.clone()),
                        scope,
                        expected,
                    );
                }
                let internal_math = match name.as_str() {
                    "sqrt" => Some("__nova_internal_sqrt_f64"),
                    "abs" => Some("__nova_internal_abs_f64"),
                    "floor" => Some("__nova_internal_floor_f64"),
                    "ceil" => Some("__nova_internal_ceil_f64"),
                    _ => None,
                };
                if let Some(internal) = internal_math {
                    if original_args.len() != 1 {
                        return Err(format!("{name} expects one f64 argument"));
                    }
                    let (value, ty) = self.expression(
                        &original_args[0],
                        scope,
                        Some(&TypeRef::Named("f64".into())),
                    )?;
                    if ty != TypeRef::Named("f64".into()) {
                        return Err(format!("{name} expects an f64 argument"));
                    }
                    return Ok((
                        Expr::Call(internal.into(), vec![value]),
                        TypeRef::Named("f64".into()),
                    ));
                }
                if name == "byte_offset" {
                    if original_args.len() != 2 {
                        return Err("byte_offset expects a pointer and a byte distance".into());
                    }
                    let pointer_ty = TypeRef::Pointer {
                        mutable: true,
                        inner: Box::new(TypeRef::Named("u8".into())),
                    };
                    let (pointer, actual_pointer) =
                        self.expression(&original_args[0], scope, Some(&pointer_ty))?;
                    if !matches!(actual_pointer, TypeRef::Pointer { .. }) {
                        return Err("byte_offset expects a pointer as its first argument".into());
                    }
                    let (distance, _) = self.expression(
                        &original_args[1],
                        scope,
                        Some(&TypeRef::Named("u64".into())),
                    )?;
                    let internal = Expr::Call(
                        "__nova_internal_byte_offset".into(),
                        vec![
                            Expr::Cast(Box::new(pointer), TypeRef::Named("u64".into())),
                            distance,
                        ],
                    );
                    return Ok((
                        Expr::Cast(Box::new(internal), pointer_ty),
                        TypeRef::Pointer {
                            mutable: true,
                            inner: Box::new(TypeRef::Named("u8".into())),
                        },
                    ));
                }
                if name == "__nova_target_new" {
                    let Some(expected) = expected else {
                        return Err("cannot infer the type for `new(...)`; assign it to a typed variable or field, for example `let value: Counter = new(7);`".into());
                    };
                    let target = self.resolved_type(expected, &scope.namespace, &scope.subst)?;
                    let TypeRef::Named(concrete) = &target else {
                        return Err("`new(...)` needs a concrete named target type; add an explicit variable or field type".into());
                    };
                    let constructor = format!("{concrete}::new");
                    let selected = if self.funcs.contains_key(&constructor) {
                        constructor
                    } else if let Some(selected) =
                        self.concrete_constructor_call(concrete, original_args.len())?
                    {
                        selected
                    } else {
                        self.blanket_constructor_call(&constructor, original_args.len())?
                            .map(|(name, _)| name)
                            .ok_or_else(|| {
                                format!("type '{}' has no constructor 'new'", display_type(&target))
                            })?
                    };
                    return self.expression(
                        &Expr::Call(selected, original_args.clone()),
                        scope,
                        Some(&target),
                    );
                }
                // Compiler-private operations are injected after surface
                // collection so they cannot be shadowed by a Nova module.
                // Lower their arguments here and let HIR validate the final
                // intrinsic ABI against the injected declarations.
                if name.starts_with("__nova_internal_") {
                    let mut args = Vec::with_capacity(original_args.len());
                    for argument in original_args {
                        args.push(self.expression(argument, scope, None)?.0);
                    }
                    let result = match name.as_str() {
                        "__nova_internal_copy_bytes"
                        | "__nova_internal_move_bytes"
                        | "__nova_internal_set_bytes"
                        | "__nova_internal_heap_free" => TypeRef::Named("()".into()),
                        "__nova_internal_load_byte" => TypeRef::Named("u8".into()),
                        "__nova_internal_text_length"
                        | "__nova_internal_text_address"
                        | "__nova_internal_heap_allocate"
                        | "__nova_internal_byte_offset" => TypeRef::Named("u64".into()),
                        "__nova_internal_borrow_text" => TypeRef::Named("str".into()),
                        "__nova_internal_sqrt_f64"
                        | "__nova_internal_abs_f64"
                        | "__nova_internal_floor_f64"
                        | "__nova_internal_ceil_f64" => TypeRef::Named("f64".into()),
                        _ => return Err(format!("unknown compiler intrinsic '{name}'")),
                    };
                    return Ok((Expr::Call(name.clone(), args), result));
                }
                // One public print<T> surface. The source declaration lives in
                // std::old::io; all type-specific decisions happen here, at compile
                // time. The result is an ordinary Nova function call, never an
                // indirect C formatter or a tagged runtime value.
                let resolved_print = self.qualify(name, &scope.namespace);
                if resolved_print == "std::old::io::print"
                    || resolved_print == "std::old::io::eprint"
                {
                    if original_args.len() != 1 {
                        return Err(
                            "error[NOVA-FMT008]: print<T> requires exactly one argument".into()
                        );
                    }
                    let (_, ty) = self.expression(&original_args[0], scope, None)?;
                    let value = original_args[0].clone();
                    let descriptor = Expr::Call(
                        if resolved_print.ends_with("::eprint") {
                            "std::io::stderr_descriptor".into()
                        } else {
                            "std::io::stdout_descriptor".into()
                        },
                        vec![],
                    );
                    let (callee, arguments): (&str, Vec<Expr>) = match &ty {
                        TypeRef::Named(kind) if kind == "str" => {
                            ("std::io::write_fd", vec![descriptor, value])
                        }
                        TypeRef::Named(kind) if kind == "bool" => {
                            ("std::old::io::write_bool", vec![descriptor, value])
                        }
                        TypeRef::Named(kind)
                            if matches!(kind.as_str(), "i8" | "i16" | "i32" | "i64" | "isize") =>
                        {
                            (
                                "std::old::io::write_integer",
                                vec![
                                    descriptor,
                                    Expr::Cast(Box::new(value), TypeRef::Named("i64".into())),
                                ],
                            )
                        }
                        TypeRef::Named(kind)
                            if matches!(kind.as_str(), "u8" | "u16" | "u32" | "u64" | "usize") =>
                        {
                            (
                                "std::old::io::write_unsigned",
                                vec![
                                    descriptor,
                                    Expr::Cast(Box::new(value), TypeRef::Named("u64".into())),
                                ],
                            )
                        }
                        TypeRef::Named(kind) if matches!(kind.as_str(), "f32" | "f64") => (
                            "std::old::io::write_float",
                            vec![
                                descriptor,
                                Expr::Cast(Box::new(value), TypeRef::Named("f64".into())),
                                Expr::Int("-1".into()),
                            ],
                        ),
                        TypeRef::Named(kind)
                            if kind.ends_with("std__old__string__String")
                                && !matches!(&value, Expr::Var(_)) =>
                        {
                            ("std::old::io::write_owned", vec![descriptor, value])
                        }
                        TypeRef::Named(_) => {
                            if matches!(&value, Expr::Var(_)) {
                                let (_,view_ty) = self.expression(&Expr::MethodCall(
                                    Box::new(value.clone()),"to_string".into(),vec![],vec![]),
                                    scope,Some(&TypeRef::Named("str".into()))
                                ).map_err(|err|format!("error[NOVA-FMT006]: {} must implement to_string() -> str; {err}",display_type(&ty)))?;
                                if view_ty != TypeRef::Named("str".into()) {
                                    return Err(
                                        "error[NOVA-FMT006]: to_string must return str".into()
                                    );
                                }
                                (
                                    "std::io::write_fd",
                                    vec![
                                        descriptor,
                                        Expr::MethodCall(
                                            Box::new(value),
                                            "to_string".into(),
                                            vec![],
                                            vec![],
                                        ),
                                    ],
                                )
                            } else {
                                ("std::old::io::write_stringable", vec![descriptor, value])
                            }
                        }
                        _ => {
                            return Err(format!(
                                "error[NOVA-FMT006]: {} cannot be printed with print<T>",
                                display_type(&ty)
                            ))
                        }
                    };
                    return self.expression(&Expr::Call(callee.into(), arguments), scope, expected);
                }
                // Compiler-only formatting: each step returns an owned Nova
                // String. No C builder, no generational handle and no runtime
                // dispatch. Each input expression is evaluated once.
                if name == crate::format_macros::BUILD {
                    let mut string = Expr::Call("std::text::string::new_string".into(), vec![]);
                    for part in original_args {
                        let Expr::Call(marker, data) = part else {
                            return Err("error[NOVA-FMT005]: malformed format AST part".into());
                        };
                        if marker == crate::format_macros::TEXT_PART {
                            if data.len() != 1 || !matches!(&data[0], Expr::Str(_)) {
                                return Err("error[NOVA-FMT005]: malformed format literal".into());
                            }
                            string = Expr::Call(
                                "std::text::string::format_add_text".into(),
                                vec![string, data[0].clone()],
                            );
                            continue;
                        }
                        if marker != crate::format_macros::VALUE_PART || data.len() != 3 {
                            return Err("error[NOVA-FMT005]: malformed format argument".into());
                        }
                        let Expr::Int(precision_text) = &data[1] else {
                            return Err(
                                "error[NOVA-FMT005]: format precision must be static".into()
                            );
                        };
                        let precision: i32 = precision_text
                            .parse()
                            .map_err(|_| "error[NOVA-FMT005]: invalid float precision")?;
                        let Expr::Bool(debug) = &data[2] else {
                            return Err("error[NOVA-FMT005]: debug mode must be static".into());
                        };
                        let (_, raw_ty) = self.expression(&data[0], scope, None)?;
                        let (ty, value, borrowed) = match raw_ty {
                            TypeRef::Reference { inner, .. } => (
                                (*inner).clone(),
                                Expr::Deref(Box::new(data[0].clone())),
                                true,
                            ),
                            other => (other, data[0].clone(), false),
                        };
                        let TypeRef::Named(kind) = &ty else {
                            return Err(format!(
                                "error[NOVA-FMT006]: {} is not formattable",
                                display_type(&ty)
                            ));
                        };
                        if precision >= 0 && kind != "f32" && kind != "f64" {
                            return Err(format!(
                                "error[NOVA-FMT004]: precision is only valid for floats, not {}",
                                display_type(&ty)
                            ));
                        }
                        let (function, mut forwarded): (&str, Vec<Expr>) = match kind.as_str() {
                            "str" if *debug => ("std::text::string::format_add_text", vec![value]),
                            "str" => ("std::text::string::format_add_text", vec![value]),
                            "bool" => ("std::text::string::format_add_bool", vec![value]),
                            "i8" | "i16" | "i32" | "i64" | "isize" => (
                                "std::text::string::format_add_i64",
                                vec![Expr::Cast(Box::new(value), TypeRef::Named("i64".into()))],
                            ),
                            "u8" | "u16" | "u32" | "u64" | "usize" => (
                                "std::text::string::format_add_u64",
                                vec![Expr::Cast(Box::new(value), TypeRef::Named("u64".into()))],
                            ),
                            "f32" | "f64" => (
                                "std::text::string::format_add_float",
                                vec![
                                    Expr::Cast(Box::new(value), TypeRef::Named("f64".into())),
                                    Expr::Int(precision.to_string()),
                                ],
                            ),
                            _ => {
                                let is_string = kind == "String"
                                    || kind.ends_with("::String")
                                    || kind.ends_with("std__text__string__String");
                                if is_string && borrowed {
                                    let rendered = Expr::Call(
                                        "std::text::string::as_text".into(),
                                        vec![data[0].clone()],
                                    );
                                    ("std::text::string::format_add_text", vec![rendered])
                                } else if is_string && !matches!(&value, Expr::Var(_)) {
                                    ("std::text::string::format_add_owned", vec![value])
                                } else {
                                    let method = if *debug && !is_string {
                                        "to_debug_string"
                                    } else {
                                        "to_string"
                                    };
                                    let rendered = Expr::MethodCall(
                                        Box::new(value.clone()),
                                        method.into(),
                                        vec![],
                                        vec![],
                                    );
                                    let (_,result) = self.expression(&rendered,scope,None)
                                        .map_err(|error|format!("error[NOVA-FMT006]: implement `{method}() -> str|String` for {}; {error}",display_type(&ty)))?;
                                    if result == TypeRef::Named("str".into()) {
                                        ("std::text::string::format_add_text", vec![rendered])
                                    } else if matches!(&result,TypeRef::Named(name) if name.ends_with("std__text__string__String"))
                                    {
                                        ("std::text::string::format_add_owned", vec![rendered])
                                    } else {
                                        return Err("error[NOVA-FMT006]: formatting method must return str or String".into());
                                    }
                                }
                            }
                        };
                        let mut args = vec![string];
                        args.append(&mut forwarded);
                        string = Expr::Call(function.into(), args);
                    }
                    return self.expression(&string, scope, expected);
                }
                // Use the same owned-String builder as format!, then send that
                // value through the modern std::io surface. Formatting never
                // reaches the compatibility runtime namespace.
                if name == crate::format_macros::WRITE_VALUE {
                    if original_args.len() != 4 {
                        return Err("error[NOVA-FMT005]: malformed format write".into());
                    }
                    let Expr::Bool(stderr) = &original_args[3] else {
                        return Err("error[NOVA-FMT005]: output stream must be static".into());
                    };
                    let rendered = Expr::Call(
                        crate::format_macros::BUILD.into(),
                        vec![Expr::Call(
                            crate::format_macros::VALUE_PART.into(),
                            vec![
                                original_args[0].clone(),
                                original_args[1].clone(),
                                original_args[2].clone(),
                            ],
                        )],
                    );
                    let descriptor = Expr::Call(
                        if *stderr {
                            "std::io::stderr_descriptor"
                        } else {
                            "std::io::stdout_descriptor"
                        }
                        .into(),
                        vec![],
                    );
                    return self.expression(
                        &Expr::Call("std::io::write_string".into(), vec![descriptor, rendered]),
                        scope,
                        expected,
                    );
                }
                let args = self.expand_call_args(original_args, scope)?;
                // Coroutine keywords are lowered to legacy runtime calls before
                // type inference. When the operand is a typed owner, dispatch
                // to the generic method instead of exposing its raw ABI handle.
                // This preserves `await task -> T`, `join task -> T` and
                // `next generator -> T` without changing the stage-0 runtime.
                if args.len() == 1 {
                    let method = match name.as_str() {
                        "std::abi::native::task::native_await_value" => Some((
                            "std__old__task__Task__",
                            "std__old__task__Future__",
                            "await_result",
                        )),
                        "std::abi::native::task::native_block_on" => {
                            Some(("std__old__task__Task__", "std__old__task__Future__", "wait"))
                        }
                        "std::abi::native::task::native_spawn" => Some((
                            "std__old__task__Task__",
                            "std__old__task__Future__",
                            "schedule",
                        )),
                        "std::abi::native::generator::native_next" => Some((
                            "std__old__generator__Generator__",
                            "std__old__generator__Generator__",
                            "next",
                        )),
                        "std::abi::native::generator::native_done" => Some((
                            "std__old__generator__Generator__",
                            "std__old__generator__Generator__",
                            "finished",
                        )),
                        "std::abi::native::coro::native_drop" => {
                            Some(("std__old__task__Task__", "std__old__task__Future__", "drop"))
                        }
                        _ => None,
                    };
                    if let Some((owner_a, owner_b, method_name)) = method {
                        let (_, type_ref) = self.expression(&args[0], scope, None)?;
                        if matches!(&type_ref,TypeRef::Named(n) if n.starts_with(owner_a)||n.starts_with(owner_b))
                        {
                            return self.expression(
                                &Expr::MethodCall(
                                    Box::new(args[0].clone()),
                                    method_name.into(),
                                    vec![],
                                    vec![],
                                ),
                                scope,
                                expected,
                            );
                        }
                    }
                    if matches!(
                        name.as_str(),
                        "std::abi::native::generator::native_done"
                            | "std::abi::native::coro::native_drop"
                    ) {
                        let (_, type_ref) = self.expression(&args[0], scope, None)?;
                        if let TypeRef::Named(owner) = type_ref {
                            let dispatch = match name.as_str() {
                                "std::abi::native::generator::native_done"
                                    if owner.starts_with("std__old__coro__Coroutine__") =>
                                {
                                    Some("completed")
                                }
                                "std::abi::native::coro::native_drop"
                                    if owner.starts_with("std__old__generator__Generator__")
                                        || owner.starts_with("std__old__coro__Coroutine__") =>
                                {
                                    Some("drop")
                                }
                                _ => None,
                            };
                            if let Some(method) = dispatch {
                                return self.expression(
                                    &Expr::MethodCall(
                                        Box::new(args[0].clone()),
                                        method.into(),
                                        vec![],
                                        vec![],
                                    ),
                                    scope,
                                    expected,
                                );
                            }
                        }
                    }
                }
                if args.len() == 2 && name == "std::abi::native::coro::native_resume" {
                    let (_, type_ref) = self.expression(&args[0], scope, None)?;
                    if matches!(&type_ref,TypeRef::Named(owner) if owner.starts_with("std__old__coro__Coroutine__"))
                    {
                        return self.expression(
                            &Expr::MethodCall(
                                Box::new(args[0].clone()),
                                "resume".into(),
                                vec![args[1].clone()],
                                vec![],
                            ),
                            scope,
                            expected,
                        );
                    }
                }
                if matches!(scope.vars.get(name), Some(TypeRef::Function { .. })) {
                    return self.expression(
                        &Expr::CallIndirect(Box::new(Expr::Var(name.clone())), args),
                        scope,
                        expected,
                    );
                }
                // Capturing closures are generated callable structs, not forged
                // function pointers. The 'call' method carries the captured state.
                if let Some(TypeRef::Named(closure)) = scope.vars.get(name) {
                    if closure.starts_with("__nova_closure_") {
                        return self.expression(
                            &Expr::MethodCall(
                                Box::new(Expr::Var(name.clone())),
                                "call".into(),
                                args,
                                vec![],
                            ),
                            scope,
                            expected,
                        );
                    }
                }
                // Static JSON derives: User::fromJson(str), User::toJson(user).
                // Path already accepts `::`; no new grammar productions are needed.
                if let Some((owner, method)) = name.rsplit_once("::") {
                    if method == "fromJson" || method == "toJson" || method == "writeJson" {
                        let resolved = self.resolved_type(
                            &TypeRef::Named(owner.to_string()),
                            &scope.namespace,
                            &scope.subst,
                        )?;
                        let TypeRef::Named(concrete) = resolved else {
                            return Err("JSON methods require a concrete struct type".into());
                        };
                        if !self.concrete_fields.contains_key(&concrete) {
                            return Err(format!("'{owner}' is not a concrete struct"));
                        }
                        if method == "fromJson" && !self.deserial.contains(&concrete) {
                            return Err(format!(
                                "'{concrete}' must derive Deserialize before calling fromJson()"
                            ));
                        }
                        if method != "fromJson" && !self.serial.contains(&concrete) {
                            return Err(format!(
                                "'{concrete}' must derive Serialize before calling {method}()"
                            ));
                        }
                        let target = match method {
                            "fromJson" => format!("{concrete}_from_json"),
                            "toJson" => format!("{concrete}_to_json"),
                            _ => format!("{concrete}_write_json"),
                        };
                        return self.expression(&Expr::Call(target, args), scope, expected);
                    }
                }
                if self
                    .lookup_variant(name, &scope.namespace, expected)
                    .is_some()
                {
                    return self.variant(name, &args, scope, expected);
                }
                // Generic associated calls use the bound *type*, not the
                // literal parameter name (e.g. T::from_json -> User::from_json).
                let (q, owner_args) = match self.generic_static_call(name, scope, args.len())? {
                    Some(bound) => bound,
                    None => {
                        let direct = self.qualify(name, &scope.namespace);
                        if self.funcs.contains_key(&direct) || self.externs.contains_key(&direct) {
                            (direct, Vec::new())
                        } else {
                            match self.blanket_constructor_call(name, args.len())? {
                                Some(blanket) => blanket,
                                None => (direct, Vec::new()),
                            }
                        }
                    }
                };
                if self.externs.contains_key(&q) {
                    let sig = self.externs[&q].clone();
                    if (sig.c_variadic && args.len() < sig.params.len())
                        || (!sig.c_variadic && args.len() != sig.params.len())
                    {
                        return Err(format!(
                            "extern '{name}' expects {}{} args, got {}",
                            sig.params.len(),
                            if sig.c_variadic { " or more" } else { "" },
                            args.len()
                        ));
                    }
                    let mut actual = vec![];
                    for (index, arg) in args.iter().enumerate() {
                        let hint = sig.params.get(index).map(|p| &p.ty);
                        actual.push(self.expression(arg, scope, hint)?.0);
                    }
                    return Ok((Expr::Call(ident(&q), actual), sig.result));
                }
                // Built-in derives emit concrete functions after specialization. They
                // are already in `items`, so resolve calls without re-specializing.
                let mut candidates =
                    vec![ident(&q), ident(&join(&scope.namespace, name)), ident(name)];
                candidates.dedup();
                let generated = candidates.iter().find_map(|candidate| {
                    self.items.iter().find_map(|item| match item {
                        Item::Function(f) if &f.name == candidate => Some(f.clone()),
                        _ => None,
                    })
                });
                if let Some(f) = generated {
                    if args.len() != f.params.len() {
                        return Err(format!(
                            "generated function '{name}' expects {} arguments",
                            f.params.len()
                        ));
                    }
                    let mut actual = vec![];
                    for (arg, p) in args.iter().zip(&f.params) {
                        actual.push(self.expression(arg, scope, Some(&p.ty))?.0);
                    }
                    return Ok((Expr::Call(f.name, actual), f.result));
                }
                if let Some(base) = ident(&q).strip_suffix("_from_json") {
                    if self.deserial.contains(base) {
                        if args.len() != 1 {
                            return Err(format!("deserializer '{name}' expects 1 str argument"));
                        }
                        let ty = TypeRef::Named("str".into());
                        let (arg, _) = self.expression(&args[0], scope, Some(&ty))?;
                        return Ok((
                            Expr::Call(format!("{base}_from_json"), vec![arg]),
                            TypeRef::Named(base.to_string()),
                        ));
                    }
                }
                if let Some(base) = ident(&q).strip_suffix("_to_json") {
                    if self.serial.contains(base) {
                        if args.len() != 1 {
                            return Err(format!("serializer '{name}' expects 1 struct argument"));
                        }
                        let ty = TypeRef::Named(base.to_string());
                        let (arg, _) = self.expression(&args[0], scope, Some(&ty))?;
                        return Ok((
                            Expr::Call(format!("{base}_to_json"), vec![arg]),
                            TypeRef::Named("str".into()),
                        ));
                    }
                }
                if let Some(base) = ident(&q).strip_suffix("_write_json") {
                    if self.serial.contains(base) {
                        if args.len() != 1 {
                            return Err(format!("serializer '{name}' expects 1 argument"));
                        }
                        let ty = TypeRef::Named(base.to_string());
                        let (arg, _) = self.expression(&args[0], scope, Some(&ty))?;
                        return Ok((
                            Expr::Call(format!("{base}_write_json"), vec![arg]),
                            TypeRef::Named("i32".into()),
                        ));
                    }
                }
                let selected = self
                    .select_overload(&q, &args)
                    .ok_or_else(|| format!("unknown function '{name}'"))?;
                let template = self.funcs[&selected].clone();
                let fixed = template.decl.params.len() - usize::from(template.decl.variadic);
                if (!template.decl.variadic && args.len() != fixed) || args.len() < fixed {
                    return Err(format!(
                        "function '{name}' expects {}{} arguments, got {}",
                        fixed,
                        if template.decl.variadic {
                            " or more"
                        } else {
                            ""
                        },
                        args.len()
                    ));
                }
                if args.len() - fixed > 1024 {
                    return Err("variadic pack exceeds the 1024-element limit".into());
                }
                let generics = template
                    .impl_generics
                    .iter()
                    .chain(&template.decl.generics)
                    .cloned()
                    .collect::<Vec<_>>();
                let mut map = HashMap::new();
                // The concrete owner supplies `impl<U>` generics of a static
                // associated function, while its own `fn method<V>` generics
                // continue to be inferred from arguments or the result type.
                for (generic, concrete) in template.impl_generics.iter().zip(owner_args) {
                    map.insert(generic.clone(), concrete);
                }
                if let Some(want) = expected {
                    if generics
                        .iter()
                        .any(|g| contains_type_var(&template.decl.result, g))
                    {
                        type_variables(want, &template.decl.result, &generics, &mut map)?;
                    }
                }
                let mut actual = vec![];
                for (i, arg) in args.iter().enumerate() {
                    let p = &template.decl.params[if i < fixed { i } else { fixed }];
                    let substituted = substitute(&p.ty, &map);
                    let unresolved = generics.iter().any(|g| contains_type_var(&substituted, g));
                    let concrete_hint = if unresolved {
                        None
                    } else {
                        Some(self.resolved_type(&substituted, &template.namespace, &map)?)
                    };
                    let (val, ty) = self.expression(arg, scope, concrete_hint.as_ref())?;
                    // Namespaced types are monomorphized before call lowering:
                    // e.g. `lexer::Scan` (source type) becomes `lexer__Scan`.
                    // Compare a non-generic parameter using its resolved type,
                    // not its source spelling, to preserve cross-module calls.
                    // A generic function can mix template parameters with ordinary
                    // concrete parameters. Infer only from parameters still containing
                    // an unbound template variable; compare all others against their
                    // resolved (monomorphized) type, just as method calls do.
                    // For example, add_nested<T>(writer: ObjectWriter, value: &T)
                    // must compare writer against std__old__json__ObjectWriter rather
                    // than the source spelling std::old::json::ObjectWriter.
                    let pattern = if unresolved {
                        &p.ty
                    } else {
                        concrete_hint.as_ref().unwrap_or(&p.ty)
                    };
                    type_variables(&ty,pattern,&generics,&mut map).map_err(|error|format!(
                        "{error}\n  = argument {} (`{}`) of call `{name}`\n  = declared parameter: {}\n  = supplied argument: {}",
                        i + 1, p.name, display_type(pattern), display_type(&ty)
                    ))?;
                    let val = if matches!(&ty, TypeRef::Reference { mutable: true, .. }) {
                        let inferred = substitute(&p.ty, &map);
                        if !generics.iter().any(|g| contains_type_var(&inferred, g)) {
                            let desired =
                                self.resolved_type(&inferred, &template.namespace, &map)?;
                            if implicit_shared_reborrow(&ty, &desired) {
                                Expr::Cast(Box::new(val), desired)
                            } else {
                                val
                            }
                        } else {
                            val
                        }
                    } else {
                        val
                    };
                    actual.push(val);
                }
                let concrete = generics
                    .iter()
                    .map(|n| {
                        map.get(n).cloned().ok_or_else(|| {
                            format!("cannot infer generic '{n}' in '{name}'; use explicit ::<T>")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if template.decl.variadic {
                    let tail = actual.split_off(fixed);
                    let pack_type = self.resolved_type(
                        &template.decl.params[fixed].ty,
                        &template.namespace,
                        &map,
                    )?;
                    actual.push(if tail.is_empty() {
                        Expr::Zero(TypeRef::Array(Box::new(pack_type), 0))
                    } else {
                        Expr::Array(tail)
                    });
                }
                let (fun, result) = self.instantiate_function(
                    &selected,
                    &concrete,
                    if template.decl.variadic {
                        Some(args.len() - fixed)
                    } else {
                        None
                    },
                )?;
                if let Some(want) = expected {
                    if &result != want && !implicit_numeric_widen(&result, want) {
                        return Err(format!(
                            "call '{name}' produces {}, expected {}",
                            display_type(&result),
                            display_type(want)
                        ));
                    }
                }
                Ok((Expr::Call(fun, actual), result))
            }
            Expr::GenericCall(name, types, original_args) => {
                if name == "size_of" || name == "align_of" {
                    if types.len() != 1 || !original_args.is_empty() {
                        return Err(format!("{name}::<T>() expects one type and no arguments"));
                    }
                    let concrete_type =
                        self.resolved_type(&types[0], &scope.namespace, &scope.subst)?;
                    let (size, alignment) = self.layout_of(&concrete_type, &mut Vec::new())?;
                    let value = if name == "size_of" { size } else { alignment };
                    return Ok((Expr::Int(value.to_string()), TypeRef::Named("u64".into())));
                }
                // Explicit monomorphization of a safe Nova callback, without
                // invoking it: function_ref::<T>(some_generic_function).
                if name == "function_ref" {
                    let [Expr::Var(function)] = original_args.as_slice() else {
                        return Err("function_ref::<T> expects one named Nova function".into());
                    };
                    let qualified = self.qualify(function, &scope.namespace);
                    if self.externs.contains_key(&qualified) {
                        return Err(
                            "extern C functions cannot be referenced as safe callbacks".into()
                        );
                    }
                    let template = self
                        .funcs
                        .get(&qualified)
                        .ok_or_else(|| format!("unknown callback '{function}'"))?
                        .clone();
                    if template.decl.variadic {
                        return Err("variadic functions cannot be callbacks".into());
                    }
                    if ast::function_is_unsafe(&template.decl.attributes, &qualified)? {
                        return Err(
                            "unsafe functions cannot be referenced as safe callbacks".into()
                        );
                    }
                    let arguments = types
                        .iter()
                        .map(|ty| self.resolved_type(ty, &scope.namespace, &scope.subst))
                        .collect::<Result<Vec<_>, _>>()?;
                    let generics = template
                        .impl_generics
                        .iter()
                        .chain(&template.decl.generics)
                        .cloned()
                        .collect::<Vec<_>>();
                    if generics.len() != arguments.len() {
                        return Err("function_ref template argument count mismatch".into());
                    }
                    let substitutions = generics
                        .into_iter()
                        .zip(arguments.iter().cloned())
                        .collect::<HashMap<_, _>>();
                    let parameters = template
                        .decl
                        .params
                        .iter()
                        .map(|param| {
                            self.resolved_type(&param.ty, &template.namespace, &substitutions)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let (symbol, result) =
                        self.instantiate_function(&qualified, &arguments, None)?;
                    return Ok((
                        Expr::FunctionRef(symbol),
                        TypeRef::Function {
                            params: parameters,
                            result: Box::new(result),
                        },
                    ));
                }
                let args = self.expand_call_args(original_args, scope)?;
                let q = match self.generic_static_call(name, scope, args.len())? {
                    Some((selected, _)) => selected,
                    None => {
                        let direct = self.qualify(name, &scope.namespace);
                        if self.funcs.contains_key(&direct) || self.externs.contains_key(&direct) {
                            direct
                        } else {
                            match self.blanket_constructor_call(name, args.len())? {
                                Some((selected, _)) => selected,
                                None => direct,
                            }
                        }
                    }
                };
                // Compile-time reflection is a string literal containing an
                // immutable JSON descriptor. No reflection tables are emitted
                // into the executable and serialization reuses derive(Json).
                if (name == "reflect" || name == "type_name") && types.len() == 1 && args.is_empty()
                {
                    let target = self.resolved_type(&types[0], &scope.namespace, &scope.subst)?;
                    let TypeRef::Named(type_name) = target else {
                        return Err("reflect expects a named concrete type".into());
                    };
                    let descriptor = if name == "type_name" {
                        type_name.clone()
                    } else {
                        let fields = self.concrete_fields.get(&type_name).ok_or_else(|| {
                            format!("reflect<{type_name}> requires a concrete struct or class")
                        })?;
                        let columns = fields
                            .iter()
                            .map(|field| {
                                format!(
                                    "{{\"name\":{:?},\"type\":{:?}}}",
                                    field.name,
                                    format!("{:?}", field.ty)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",");
                        format!("{{\"name\":{:?},\"fields\":[{}]}}", type_name, columns)
                    };
                    return Ok((
                        Expr::Str(format!("{descriptor:?}")),
                        TypeRef::Named("str".into()),
                    ));
                }
                let concrete = types
                    .iter()
                    .map(|t| self.resolved_type(t, &scope.namespace, &scope.subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let selected = self
                    .select_overload(&q, &args)
                    .ok_or_else(|| format!("unknown generic function '{name}'"))?;
                let template = self.funcs[&selected].clone();
                let generic = template
                    .impl_generics
                    .iter()
                    .chain(&template.decl.generics)
                    .cloned()
                    .collect::<Vec<_>>();
                if generic.len() != concrete.len() {
                    return Err(format!(
                        "incorrect number of generic arguments for '{name}'"
                    ));
                }
                let fixed = template.decl.params.len() - usize::from(template.decl.variadic);
                if (!template.decl.variadic && args.len() != fixed) || args.len() < fixed {
                    return Err(format!(
                        "function '{name}' expects {}{} arguments, got {}",
                        fixed,
                        if template.decl.variadic {
                            " or more"
                        } else {
                            ""
                        },
                        args.len()
                    ));
                }
                if args.len() - fixed > 1024 {
                    return Err("variadic pack exceeds the 1024-element limit".into());
                }
                let subst = generic
                    .into_iter()
                    .zip(concrete.iter().cloned())
                    .collect::<HashMap<_, _>>();
                let mut actual = vec![];
                for (i, arg) in args.iter().enumerate() {
                    let p = &template.decl.params[if i < fixed { i } else { fixed }];
                    let hint = self.resolved_type(&p.ty, &template.namespace, &subst)?;
                    actual.push(self.expression(arg, scope, Some(&hint))?.0);
                }
                if template.decl.variadic {
                    let tail = actual.split_off(fixed);
                    let hint = self.resolved_type(
                        &template.decl.params[fixed].ty,
                        &template.namespace,
                        &subst,
                    )?;
                    actual.push(if tail.is_empty() {
                        Expr::Zero(TypeRef::Array(Box::new(hint), 0))
                    } else {
                        Expr::Array(tail)
                    });
                }
                let (f, ret) = self.instantiate_function(
                    &selected,
                    &concrete,
                    if template.decl.variadic {
                        Some(args.len() - fixed)
                    } else {
                        None
                    },
                )?;
                Ok((Expr::Call(f, actual), ret))
            }
            Expr::StructInit(name, fields) => self.struct_init(name, &[], fields, scope, expected),
            Expr::GenericStructInit(name, types, fields) => {
                let types = types
                    .iter()
                    .map(|x| self.resolved_type(x, &scope.namespace, &scope.subst))
                    .collect::<Result<Vec<_>, _>>()?;
                self.struct_init(name, &types, fields, scope, expected)
            }
            Expr::Field(value, field) => {
                let (mut base, mut ty) = self.expression(value, scope, None)?;
                if let TypeRef::Pointer { inner, .. } | TypeRef::Reference { inner, .. } = &ty {
                    let inner = inner.as_ref().clone();
                    base = Expr::Deref(Box::new(base));
                    ty = inner;
                }
                let TypeRef::Named(n) = &ty else {
                    return Err("field access requires a struct".into());
                };
                if self.enum_meta.contains_key(n) {
                    return Err("enum internals are inaccessible; use match instead".into());
                }
                let defs = self
                    .concrete_fields
                    .get(n)
                    .ok_or_else(|| format!("'{n}' has no fields"))?;
                if let Some(info) = defs.iter().find(|f| f.name == *field) {
                    return Ok((Expr::Field(Box::new(base), field.clone()), info.ty.clone()));
                }
                // Properties lower to normal methods and are resolved with
                // the same borrow checker, concepts and template inference.
                // A field always takes precedence over a computed property.
                let getter =
                    Expr::MethodCall(value.clone(), format!("property_{field}"), vec![], vec![]);
                self.expression(&getter, scope, expected).map_err(|_| {
                    format!("'{n}' has neither a field nor a computed property '{field}'")
                })
            }
            Expr::MethodCall(receiver, name, args, explicit_types) => {
                if matches!(receiver.as_ref(),Expr::Var(n) if n=="super") {
                    if !scope.vars.contains_key("self") {
                        return Err("super.method() requires an instance method".into());
                    }
                    let call = Expr::MethodCall(
                        Box::new(Expr::Var("self".into())),
                        format!("__super__{name}"),
                        args.clone(),
                        explicit_types.clone(),
                    );
                    return self.expression(&call, scope, expected);
                }
                let (base, ty) = self.expression(receiver, scope, None)?;
                let dyn_trait = match &ty {
                    TypeRef::Reference { inner, .. } | TypeRef::Pointer { inner, .. } => {
                        match inner.as_ref() {
                            TypeRef::Named(name) => dyn_trait_name(name),
                            _ => None,
                        }
                    }
                    TypeRef::Named(name) => dyn_trait_name(name),
                    _ => None,
                };
                if let Some(trait_name) = dyn_trait {
                    let prefix = format!("__nova_dyn__{}__", ident(trait_name));
                    let selected = self
                        .funcs
                        .keys()
                        .find(|candidate| {
                            (candidate.starts_with(&prefix)
                                || candidate.contains(&format!("::{prefix}")))
                                && candidate.ends_with(&format!("__{name}"))
                        })
                        .cloned();
                    if let Some(selected) = selected {
                        let template = self.funcs[&selected].clone();
                        let args = self.expand_call_args(args, scope)?;
                        if args.len() + 1 != template.decl.params.len() {
                            return Err(format!(
                                "dynamic method '{name}' received the wrong number of arguments"
                            ));
                        }
                        let mut actual = vec![base];
                        for (arg, param) in args.iter().zip(template.decl.params.iter().skip(1)) {
                            let hint =
                                self.resolved_type(&param.ty, &template.namespace, &scope.subst)?;
                            let (value, _) = self.expression(arg, scope, Some(&hint))?;
                            actual.push(value);
                        }
                        let (function, result) = self.instantiate_function(&selected, &[], None)?;
                        return Ok((Expr::Call(function, actual), result));
                    }
                    return Err(format!(
                        "dynamic method '{name}' is not available for dyn {trait_name}"
                    ));
                }
                // Generator/coroutine handles are represented as u64 during
                // bootstrap. `dispose handle` is parsed as `.drop()`, but a
                // scalar handle has no user destructor; route it to the
                // compiler-owned coroutine ABI instead.
                if name == "drop"
                    && matches!(&ty, TypeRef::Named(kind) if kind == "u64")
                    && args.is_empty()
                {
                    return self.expression(
                        &Expr::Call("std::abi::native::coro::native_drop".into(), vec![base]),
                        scope,
                        expected,
                    );
                }
                // Coroutine/generator prefix operators are parsed as method
                // calls so they can also remain ergonomic on typed owners.
                // A raw bootstrap handle is still a u64, therefore dispatch
                // the operation directly to the compiler-owned ABI instead
                // of looking for a user method on u64.
                if matches!(&ty, TypeRef::Named(kind) if kind == "u64") && args.is_empty() {
                    let intrinsic = match name.as_str() {
                        "next" => Some("std::abi::native::generator::native_next"),
                        "finished" => Some("std::abi::native::generator::native_done"),
                        "wait" => Some("std::abi::native::task::native_block_on"),
                        "schedule" => Some("std::abi::native::task::native_spawn"),
                        _ => None,
                    };
                    if let Some(intrinsic) = intrinsic {
                        let call = Expr::Call(intrinsic.into(), vec![base]);
                        // The runtime transports completed task values in one
                        // machine word. Re-apply the async function's declared
                        // scalar result at the user-facing wait site so
                        // `let ok: bool = task.wait()` and integer aliases do
                        // not leak the ABI's i64 representation.
                        if let Some(want) = expected {
                            if matches!(want, TypeRef::Named(name) if name == "bool") {
                                return self.expression(
                                    &Expr::Binary(
                                        Box::new(call),
                                        BinaryOp::Ne,
                                        Box::new(Expr::Int("0".into())),
                                    ),
                                    scope,
                                    expected,
                                );
                            }
                            if !matches!(want, TypeRef::Named(name) if name == "i64") {
                                return self.expression(
                                    &Expr::Cast(Box::new(call), want.clone()),
                                    scope,
                                    expected,
                                );
                            }
                        }
                        return self.expression(&call, scope, expected);
                    }
                }
                if !explicit_types.is_empty()
                    && matches!(name.as_str(), "toJson" | "writeJson" | "len")
                {
                    return Err(format!(
                        "method '{name}' does not accept explicit generic arguments"
                    ));
                }
                if name == "toJson" || name == "writeJson" {
                    if !args.is_empty() {
                        return Err(format!("{name}() takes no arguments"));
                    }
                    let TypeRef::Named(concrete) = &ty else {
                        return Err(format!(
                            "{name}() requires a struct value, not {}",
                            display_type(&ty)
                        ));
                    };
                    if !self.concrete_fields.contains_key(concrete) {
                        return Err(format!("{name}() requires a struct value"));
                    }
                    if !self.serial.contains(concrete) {
                        return Err(format!(
                            "'{concrete}' must derive Serialize before calling {name}()"
                        ));
                    }
                    let symbol = if name == "toJson" {
                        format!("{concrete}_to_json")
                    } else {
                        format!("{concrete}_write_json")
                    };
                    let result = if name == "toJson" {
                        TypeRef::Named("str".into())
                    } else {
                        TypeRef::Named("i32".into())
                    };
                    return Ok((Expr::Call(symbol, vec![base]), result));
                }
                if name == "len" && args.is_empty() {
                    if let TypeRef::Array(_, n) = &ty {
                        // A compile-time constant must not silently erase receiver side effects.
                        if !matches!(receiver.as_ref(), Expr::Var(_)) {
                            return Err(
                                "array len() requires a named array variable in this bootstrap"
                                    .into(),
                            );
                        }
                        if *n > i32::MAX as usize {
                            return Err("array length exceeds i32".into());
                        }
                        return Ok((Expr::Int(n.to_string()), i32t()));
                    }
                }
                let (concrete_name, pointer_receiver) = match &ty {
                    TypeRef::Named(n) => (n.clone(), false),
                    TypeRef::Pointer { inner, .. } | TypeRef::Reference { inner, .. } => {
                        match inner.as_ref() {
                            TypeRef::Named(n) => (n.clone(), true),
                            _ => return Err("methods require a struct receiver".into()),
                        }
                    }
                    _ => return Err("methods require a struct or enum value".into()),
                };
                let mut candidates = self
                    .funcs
                    .keys()
                    .filter(|f| self.funcs[*f].decl.name == name.as_str())
                    .cloned()
                    .collect::<Vec<_>>();
                candidates.sort();
                let receiver_is_addressable = addressable_receiver(&base);
                let mut selected = candidates.into_iter().find(|f| {
                    // Free functions can share a method name. They are not
                    // receiver candidates and must not crash method lookup.
                    let Some((target, _)) = self.overload_base(f).rsplit_once("::") else {
                        return false;
                    };
                    let function = &self.funcs[f].decl;
                    let fixed = function.params.len() - usize::from(function.variadic);
                    let arity_matches = (function.variadic && args.len() + 1 >= fixed)
                        || (!function.variadic && args.len() + 1 == fixed);
                    let right_type = concrete_name == ident(target)
                        || concrete_name.starts_with(&format!("{}__", ident(target)));
                    let wants_pointer = matches!(
                        self.funcs[f].decl.params.first().map(|p| &p.ty),
                        Some(TypeRef::Pointer { .. } | TypeRef::Reference { .. })
                    );
                    let can_auto_borrow = !pointer_receiver
                        && matches!(
                            self.funcs[f].decl.params.first().map(|p| &p.ty),
                            Some(TypeRef::Reference { .. })
                        );
                    let temporary_can_move = !receiver_is_addressable
                        && !matches!(
                            self.funcs[f].decl.params.first().map(|p| &p.ty),
                            Some(TypeRef::Reference { .. } | TypeRef::Pointer { .. })
                        );
                    arity_matches
                        && right_type
                        && self.funcs[f]
                            .decl
                            .params
                            .first()
                            .is_some_and(|p| p.name == "self")
                        && (receiver_is_addressable || temporary_can_move)
                        && (pointer_receiver == wants_pointer || can_auto_borrow)
                });
                // Blanket impls are intentionally not materialized during the
                // initial collection pass. If ordinary dispatch did not find
                // a method, try an impl whose owner is one of its generic
                // parameters and let instantiate_function perform the final
                // concept check for the concrete receiver type.
                let blanket_owner = if selected.is_none() {
                    self.funcs
                        .keys()
                        .filter(|f| self.funcs[*f].decl.name == name.as_str())
                        .filter_map(|f| {
                            let (owner, _) = self.overload_base(f).rsplit_once("::")?;
                            let definition = &self.funcs[f];
                            let fixed = definition.decl.params.len()
                                - usize::from(definition.decl.variadic);
                            let arity_matches = (definition.decl.variadic
                                && args.len() + 1 >= fixed)
                                || (!definition.decl.variadic && args.len() + 1 == fixed);
                            (arity_matches
                                && definition
                                    .impl_generics
                                    .iter()
                                    .any(|generic| generic == owner)
                                && definition
                                    .decl
                                    .params
                                    .first()
                                    .is_some_and(|p| p.name == "self")
                                && (definition.decl.params.first().is_some_and(
                                    |p| matches!(&p.ty, TypeRef::Named(ty) if ty == owner),
                                )))
                            .then(|| f.clone())
                        })
                        .next()
                } else {
                    None
                };
                let is_blanket = selected.is_none() && blanket_owner.is_some();
                selected = selected.or(blanket_owner);
                let selected = selected.ok_or_else(||format!("method '{name}' is not defined for '{concrete_name}' with this receiver type"))?;
                let template = self.funcs[&selected].clone();
                // Local/field/index receivers borrow their original place.
                // Computed temporaries still cannot be implicitly borrowed.
                let base = if !pointer_receiver {
                    if let Some(ast::Param {
                        ty: TypeRef::Reference { mutable, .. },
                        ..
                    }) = template.decl.params.first()
                    {
                        if !addressable_receiver(&base) {
                            return Err(format!(
                                "method '{name}' requires an addressable receiver for implicit borrow (receiver: '{concrete_name}', resolved method: '{selected}')"
                            ));
                        }
                        Expr::AddressOf {
                            mutable: *mutable,
                            value: Box::new(base),
                        }
                    } else {
                        base
                    }
                } else if let (
                    TypeRef::Reference {
                        mutable: true,
                        inner,
                    },
                    Some(ast::Param {
                        ty: TypeRef::Reference { mutable: false, .. },
                        ..
                    }),
                ) = (&ty, template.decl.params.first())
                {
                    let shared = TypeRef::Reference {
                        mutable: false,
                        inner: inner.clone(),
                    };
                    Expr::Cast(Box::new(base), shared)
                } else {
                    base
                };
                let args = self.expand_call_args(args, scope)?;
                let fixed = template.decl.params.len() - usize::from(template.decl.variadic);
                if (template.decl.variadic && args.len() + 1 < fixed)
                    || (!template.decl.variadic && args.len() + 1 != fixed)
                {
                    return Err(format!(
                        "method '{name}' expects {}{} explicit arguments, got {}",
                        fixed.saturating_sub(1),
                        if template.decl.variadic {
                            " or more"
                        } else {
                            ""
                        },
                        args.len()
                    ));
                }
                if args.len() + 1 - fixed > 1024 {
                    return Err("variadic method pack exceeds the 1024-element limit".into());
                }
                let generic = template
                    .impl_generics
                    .iter()
                    .chain(&template.decl.generics)
                    .cloned()
                    .collect::<Vec<_>>();
                let mut map = HashMap::new();
                let base_args = if is_blanket {
                    vec![TypeRef::Named(concrete_name.clone())]
                } else {
                    self.instance_args
                        .get(&concrete_name)
                        .cloned()
                        .unwrap_or_default()
                };
                if template.impl_generics.len() != base_args.len() {
                    return Err(format!(
                        "method '{name}' does not match receiver generic arity"
                    ));
                }
                for (param, arg) in template.impl_generics.iter().zip(base_args.iter().cloned()) {
                    map.insert(param.clone(), arg);
                }
                // The receiver binds the impl<T> parameters. `obj.method::<U>()`
                // supplies only the method's own generics, not those of impl<T>.
                if !explicit_types.is_empty() {
                    if explicit_types.len() != template.decl.generics.len() {
                        return Err(format!(
                            "method '{name}' expects {} explicit generic arguments, got {}",
                            template.decl.generics.len(),
                            explicit_types.len()
                        ));
                    }
                    for (param, ty) in template.decl.generics.iter().zip(explicit_types) {
                        let concrete = self.resolved_type(ty, &scope.namespace, &scope.subst)?;
                        map.insert(param.clone(), concrete);
                    }
                }
                // SDLEF's typed component query is a language-level generic
                // operation. `object.hasComponent::<C>()` is decided from
                // the receiver's instantiated component type, so it cannot
                // accidentally turn into a runtime string lookup or a plain
                // active flag check.
                if name == "hasComponent" || name == "HasComponent" {
                    if template.decl.generics.len() != 1 || explicit_types.len() != 1 {
                        return Err("hasComponent requires exactly one explicit component type, e.g. hasComponent::<Transform>()".into());
                    }
                    let component_type = base_args.first().ok_or_else(|| {
                        "hasComponent requires a typed GameObject<T> receiver".to_string()
                    })?;
                    let requested_type = map.get(&template.decl.generics[0]).ok_or_else(|| {
                        "hasComponent could not resolve its component type".to_string()
                    })?;
                    return Ok((
                        Expr::Bool(component_type == requested_type),
                        TypeRef::Named("bool".into()),
                    ));
                }
                if name == "getComponent" || name == "GetComponent" {
                    if template.decl.generics.len() != 1 || explicit_types.len() != 1 {
                        return Err("getComponent requires exactly one explicit component type, e.g. getComponent::<Transform>()".into());
                    }
                    let component_type = base_args.first().ok_or_else(|| {
                        "getComponent requires a typed GameObject<T> receiver".to_string()
                    })?;
                    let requested_type = map.get(&template.decl.generics[0]).ok_or_else(|| {
                        "getComponent could not resolve its component type".to_string()
                    })?;
                    if component_type != requested_type {
                        return Err(format!(
                            "getComponent::<{}>() is not available on GameObject<{}>",
                            display_type(requested_type),
                            display_type(component_type)
                        ));
                    }
                }
                let mut actual = vec![base];
                for (i, arg) in args.iter().enumerate() {
                    let p = &template.decl.params[if i + 1 < fixed { i + 1 } else { fixed }];
                    let substituted = substitute(&p.ty, &map);
                    let unresolved = generic.iter().any(|g| contains_type_var(&substituted, g));
                    let hint = if unresolved {
                        None
                    } else {
                        Some(self.resolved_type(&substituted, &template.namespace, &map)?)
                    };
                    let (val, got) = self.expression(arg, scope, hint.as_ref())?;
                    // Namespace-qualified source types are lowered to concrete
                    // symbols before calls (express::Request -> express__Request).
                    // Check non-generic method parameters against their resolved
                    // signature, just as ordinary function calls already do.
                    // Generic parameters must retain their template variables
                    // for inference by type_variables().
                    let pattern = if unresolved {
                        &p.ty
                    } else {
                        hint.as_ref().unwrap_or(&p.ty)
                    };
                    type_variables(&got,pattern,&generic,&mut map).map_err(|error|format!(
                        "{error}\n  = argument {} (`{}`) of method `{name}`\n  = declared parameter: {}\n  = supplied argument: {}",
                        i + 1, p.name, display_type(pattern), display_type(&got)
                    ))?;
                    let val = if matches!(&got, TypeRef::Reference { mutable: true, .. }) {
                        let inferred = substitute(&p.ty, &map);
                        if !generic.iter().any(|g| contains_type_var(&inferred, g)) {
                            let desired =
                                self.resolved_type(&inferred, &template.namespace, &map)?;
                            if implicit_shared_reborrow(&got, &desired) {
                                Expr::Cast(Box::new(val), desired)
                            } else {
                                val
                            }
                        } else {
                            val
                        }
                    } else {
                        val
                    };
                    actual.push(val);
                }
                let concrete = generic
                    .iter()
                    .map(|n| {
                        map.get(n)
                            .cloned()
                            .ok_or_else(|| format!("cannot infer method generic '{n}'"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if template.decl.variadic {
                    let tail = actual.split_off(fixed);
                    let pack_ty = self.resolved_type(
                        &template.decl.params[fixed].ty,
                        &template.namespace,
                        &map,
                    )?;
                    actual.push(if tail.is_empty() {
                        Expr::Zero(TypeRef::Array(Box::new(pack_ty), 0))
                    } else {
                        Expr::Array(tail)
                    });
                }
                let (f, ret) = self.instantiate_function(
                    &selected,
                    &concrete,
                    if template.decl.variadic {
                        Some(args.len() + 1 - fixed)
                    } else {
                        None
                    },
                )?;
                Ok((Expr::Call(f, actual), ret))
            }
            Expr::Array(values) => {
                if values.is_empty() {
                    if let Some(TypeRef::Array(inner, 0)) = expected {
                        let ty = TypeRef::Array(inner.clone(), 0);
                        return Ok((Expr::Zero(ty.clone()), ty));
                    }
                    return Err("empty arrays require an explicit [T; 0] type".into());
                }
                let element = if let Some(TypeRef::Array(item, len)) = expected {
                    if *len != values.len() {
                        return Err("array length mismatch".into());
                    }
                    Some(item.as_ref())
                } else {
                    None
                };
                let mut actual = vec![];
                let mut ty = None;
                for value in values {
                    let (e, t) = self.expression(value, scope, element.or(ty.as_ref()))?;
                    if let Some(old) = &ty {
                        if old != &t {
                            return Err("array has mixed element types".into());
                        }
                    }
                    ty = Some(t);
                    actual.push(e);
                }
                Ok((
                    Expr::Array(actual),
                    TypeRef::Array(Box::new(ty.unwrap()), values.len()),
                ))
            }
            Expr::Index(base, index) => {
                let (value, ty) = self.expression(base, scope, None)?;
                let element = match ty {
                    TypeRef::Array(element, _) => *element,
                    TypeRef::Pointer { inner, .. } => *inner,
                    _ => return Err("indexing requires an array or pointer".into()),
                };
                let (index, _) = self.expression(index, scope, Some(&i32t()))?;
                Ok((Expr::Index(Box::new(value), Box::new(index)), element))
            }
            Expr::AddressOf { mutable, value } => {
                let (value, ty) = self.expression(value, scope, None)?;
                let target_type = match expected {
                    Some(TypeRef::Pointer {
                        mutable: target_mutable,
                        inner,
                    }) if *target_mutable == *mutable && **inner == ty => TypeRef::Pointer {
                        mutable: *mutable,
                        inner: Box::new(ty),
                    },
                    _ => TypeRef::Reference {
                        mutable: *mutable,
                        inner: Box::new(ty),
                    },
                };
                Ok((
                    Expr::AddressOf {
                        mutable: *mutable,
                        value: Box::new(value),
                    },
                    target_type,
                ))
            }
            Expr::Deref(value) => {
                let (value, ty) = self.expression(value, scope, None)?;
                let inner = match ty {
                    TypeRef::Pointer { inner, .. } | TypeRef::Reference { inner, .. } => inner,
                    _ => return Err("dereferencing non pointer".into()),
                };
                Ok((Expr::Deref(Box::new(value)), *inner))
            }
        }
    }
    fn struct_init(
        &mut self,
        name: &str,
        args: &[TypeRef],
        fields: &[(String, Expr)],
        scope: &Scope,
        expected: Option<&TypeRef>,
    ) -> Result<(Expr, TypeRef), String> {
        let q = self.qualify(name, &scope.namespace);
        let definition = self
            .structs
            .get(&q)
            .map(|entry| entry.decl.clone())
            .ok_or_else(|| format!("unknown struct '{name}'"))?;
        let mut inferred = Vec::new();
        if args.is_empty() && !definition.generics.is_empty() {
            let mut substitutions = HashMap::new();
            // If the surrounding expression provides a concrete instance,
            // prefer its generic arguments over default literal inference.
            if let Some(want) = expected {
                match want {
                    // `new Pair { ... }` can inherit its generic arguments
                    // from `let pair: Pair<i64, str> = ...`.  Without this,
                    // integer literals are inferred first (`i32`) and then
                    // conflict with the declared `i64` key type.
                    TypeRef::Applied(want_name, actuals)
                        if self.qualify(want_name, &scope.namespace) == q
                            && actuals.len() == definition.generics.len() =>
                    {
                        for (formal, actual) in definition.generics.iter().zip(actuals) {
                            substitutions.insert(formal.clone(), actual.clone());
                        }
                    }
                    TypeRef::Named(want_name) => {
                        if let Some(existing) = self.instance_args.get(want_name) {
                            for (formal, actual) in definition.generics.iter().zip(existing) {
                                substitutions.insert(formal.clone(), actual.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Infer remaining type variables from named fields. Each field
            // expression is checked once for its type; normal lowering later
            // consumes it exactly once and preserves evaluation order.
            for (field, expr) in fields {
                let declared = definition
                    .fields
                    .iter()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| format!("unknown field '{field}' in '{name}'"))?;
                let (_, actual) = self.expression(expr, scope, None)?;
                type_variables(
                    &actual,
                    &declared.ty,
                    &definition.generics,
                    &mut substitutions,
                )
                .map_err(|error| format!("cannot infer field '{field}' of '{name}': {error}"))?;
            }
            for generic in &definition.generics {
                inferred.push(substitutions.get(generic).cloned().ok_or_else(|| {
                    format!("cannot infer '{generic}' for '{name}' from its fields; use <T>")
                })?);
            }
        }
        let resolved = self.instantiate_type(&q, if args.is_empty() { &inferred } else { args })?;
        let TypeRef::Named(ref concrete) = resolved else {
            unreachable!()
        };
        if let Some(want) = expected {
            let want = self.resolved_type(want, &scope.namespace, &scope.subst)?;
            if want != resolved {
                return Err(format!(
                    "constructor for '{name}' produces {}, expected {}",
                    display_type(&resolved),
                    display_type(&want)
                ));
            }
        }
        let definitions = self
            .concrete_fields
            .get(concrete)
            .cloned()
            .ok_or_else(|| format!("internal type layout missing for '{concrete}'"))?;
        // Class storage currently uses a flattened base-prefix layout, but
        // construction must not leak that representation into derived class
        // initializers.  `new Child { ... }` supplies only Child's fields;
        // inherited fields belong to the base-construction step.  Defaults on
        // inherited fields remain honored, while fields without defaults are
        // represented by the zero value until explicit base-constructor
        // lowering is available.
        let own_fields = definition
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<HashSet<_>>();
        let derived_class = definition.is_class && definition.parent.is_some();
        let base_constructor = if derived_class {
            let instance_args = self
                .instance_args
                .get(concrete)
                .cloned()
                .unwrap_or_default();
            let bindings = definition
                .generics
                .iter()
                .cloned()
                .zip(instance_args)
                .collect::<HashMap<_, _>>();
            let parent = substitute(definition.parent.as_ref().unwrap(), &bindings);
            let parent = self.resolved_type(&parent, &scope.namespace, &HashMap::new())?;
            let TypeRef::Named(parent_name) = parent.clone() else {
                return Err(format!("invalid base type for '{name}' initializer"));
            };
            let constructor = format!("{parent_name}::new");
            let canonical_constructor = format!("{}::new", parent_name.replace("__", "::"));
            let constructor_key = if self.funcs.contains_key(&constructor) {
                constructor.clone()
            } else {
                canonical_constructor
            };
            let usable = self.funcs.get(&constructor_key).is_some_and(|function| {
                function.decl.params.is_empty()
                    && function
                        .decl
                        .attributes
                        .iter()
                        .any(|attribute| attribute.name == "static")
            });
            usable.then_some((constructor_key, parent))
        } else {
            None
        };
        let base_temp = "__nova_base_constructor";
        let mut needs_base_constructor = false;
        let mut initializer_scope = scope.clone();
        if let Some((_, parent)) = &base_constructor {
            initializer_scope
                .vars
                .insert(base_temp.into(), parent.clone());
        }
        let mut supplied = HashMap::<String, &Expr>::new();
        for (field, expr) in fields {
            if supplied.insert(field.clone(), expr).is_some() {
                return Err(format!("duplicate field '{field}' in '{name}' initializer"));
            }
            if !definitions.iter().any(|f| f.name == *field) {
                return Err(format!("unknown field '{field}' in '{name}' initializer"));
            }
            if derived_class
                && !own_fields.contains(field.as_str())
                && !scope.vars.contains_key(base_temp)
            {
                return Err(format!(
                    "inherited field '{field}' cannot be initialized in '{name}'; initialize it in the base constructor"
                ));
            }
        }
        let mut actual = Vec::with_capacity(definitions.len());
        for field in definitions {
            let init = if let Some(user) = supplied.get(&field.name) {
                (**user).clone()
            } else if derived_class
                && !own_fields.contains(field.name.as_str())
                && base_constructor.is_some()
            {
                needs_base_constructor = true;
                Expr::Field(Box::new(Expr::Var(base_temp.into())), field.name.clone())
            } else if let Some(default) = field.default {
                default
            } else if derived_class && !own_fields.contains(field.name.as_str()) {
                self.zero_expr(&field.ty)?
            } else {
                return Err(format!(
                    "missing field '{}' in '{name}' initializer",
                    field.name
                ));
            };
            actual.push((
                field.name,
                self.expression(&init, &initializer_scope, Some(&field.ty))?
                    .0,
            ));
        }
        let value = Expr::StructInit(concrete.clone(), actual);
        if needs_base_constructor {
            let (constructor, parent) = base_constructor.expect("base constructor checked above");
            return self.expression(
                &Expr::Block(vec![
                    Stmt::Let {
                        name: base_temp.into(),
                        mutable: false,
                        ty: Some(parent),
                        init: Expr::Call(constructor, vec![]),
                        defer: None,
                    },
                    Stmt::Tail(value),
                ]),
                scope,
                expected,
            );
        }
        Ok((value, resolved))
    }
    fn lookup_variant(
        &self,
        path: &str,
        ns: &str,
        expected: Option<&TypeRef>,
    ) -> Option<(String, String)> {
        // Qualified variants are resolved against their explicit enum owner.
        if let Some((ty, variant)) = path.rsplit_once("::") {
            let q = self.qualify(ty, ns);
            let def = self.enums.get(&q)?;
            return def
                .decl
                .variants
                .iter()
                .any(|x| x.name == variant)
                .then(|| (q, variant.into()));
        }
        // A package can embed STD more than once (e.g. std and nexo::std).
        // Both copies declare Option::None and Option::Some, so an unqualified
        // variant must first be matched against the expected *concrete* enum.
        // The enum metadata records the original qualified declaration.
        if let Some(TypeRef::Named(concrete)) = expected {
            if let Some(meta) = self.enum_meta.get(concrete) {
                if self
                    .enums
                    .get(&meta.name)
                    .is_some_and(|def| def.decl.variants.iter().any(|variant| variant.name == path))
                {
                    return Some((meta.name.clone(), path.into()));
                }
            }
        }
        let mut possible = self
            .enums
            .iter()
            .filter(|(_, decl)| {
                decl.decl
                    .variants
                    .iter()
                    .any(|variant| variant.name == path)
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        possible.sort();
        if possible.len() == 1 {
            Some((possible.remove(0), path.into()))
        } else {
            None
        }
    }
    fn variant(
        &mut self,
        path: &str,
        args: &[Expr],
        scope: &Scope,
        expected: Option<&TypeRef>,
    ) -> Result<(Expr, TypeRef), String> {
        let (enum_name, variant) = self
            .lookup_variant(path, &scope.namespace, expected)
            .ok_or_else(|| format!("unknown enum variant '{path}'"))?;
        let def = self.enums[&enum_name].clone();
        let definition = def
            .decl
            .variants
            .iter()
            .find(|v| v.name == variant)
            .unwrap()
            .clone();
        if args.len() != definition.payload.len() {
            return Err(format!(
                "variant '{path}' expects {} payload values",
                definition.payload.len()
            ));
        }
        let mut map = HashMap::new();
        let mut concrete_self = None;
        if let Some(TypeRef::Named(concrete)) = expected {
            if let Some(meta) = self.enum_meta.get(concrete) {
                if meta.name == enum_name {
                    let concrete_args =
                        self.instance_args.get(concrete).cloned().ok_or_else(|| {
                            format!("missing concrete enum arguments for {concrete}")
                        })?;
                    for (g, raw) in def.decl.generics.iter().zip(concrete_args) {
                        map.insert(g.clone(), raw);
                    }
                    concrete_self = Some(TypeRef::Named(concrete.clone()));
                }
            }
        }
        // Enum payloads may themselves contain `Self`, including inside
        // generic containers (e.g. `array(Vec<Self>)`). Resolve it against
        // the concrete enum before checking constructor arguments. Without
        // this, the declared payload remains `Vec<Self>` while the supplied
        // expression is correctly typed as `Vec<JsonValue>`.
        if concrete_self.is_none() && def.decl.generics.is_empty() {
            concrete_self = Some(self.instantiate_type(&enum_name, &[])?);
        }
        if let Some(self_type) = concrete_self {
            map.insert("Self".into(), self_type);
        }
        let mut actual = vec![];
        let payload_namespace = def.namespace.clone();
        let all_generics_known = def.decl.generics.iter().all(|name| map.contains_key(name));
        for (e, p) in args.iter().zip(&definition.payload) {
            // Enum declarations retain source-level payload types (for example,
            // `String`). Resolve those names in the enum's declaration namespace
            // before checking a concrete payload. Previously this path compared
            // the unqualified declaration spelling with the already-specialized
            // expression type, producing the particularly confusing diagnostic
            // `expected std::text::string::String, found std::text::string::String`.
            // For generic enums whose arguments are not fully inferred yet, keep
            // the structural pattern so type_variables can infer them as before.
            let resolved_payload = if all_generics_known {
                Some(self.resolved_type(p, &payload_namespace, &map)?)
            } else {
                match p {
                    TypeRef::Named(n) => map.get(n).cloned(),
                    _ => None,
                }
            };
            let pattern = resolved_payload
                .clone()
                .unwrap_or_else(|| substitute(p, &map));
            let (expr, ty) = self.expression(e, scope, resolved_payload.as_ref())?;
            type_variables(&ty, &pattern, &def.decl.generics, &mut map)?;
            actual.push(expr);
        }
        let concrete=def.decl.generics.iter().map(|n|map.get(n).cloned().ok_or_else(||format!("cannot infer enum template argument '{n}' for '{path}'; annotate the variable"))).collect::<Result<Vec<_>,_>>()?;
        let instance = self.instantiate_type(&enum_name, &concrete)?;
        let TypeRef::Named(n) = &instance else {
            unreachable!()
        };
        let meta = self.enum_meta[n].clone();
        let tag = meta.variants.iter().position(|x| x.0 == variant).unwrap();
        let mut values = vec![("__nova_tag".into(), Expr::Int(tag.to_string()))];
        let mut j = 0;
        for (which, fields) in &meta.variants {
            for (field, ty) in fields {
                if *which == variant {
                    values.push((field.clone(), actual[j].clone()));
                    j += 1;
                } else {
                    values.push((field.clone(), Expr::Zero(ty.clone())));
                }
            }
        }
        Ok((Expr::StructInit(n.clone(), values), instance))
    }
}
fn check_params(params: &[String]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for p in params {
        if !seen.insert(p) {
            return Err(format!("duplicate generic parameter '{p}'"));
        }
    }
    Ok(())
}
fn validate_attrs(attrs: &[ast::Attribute], is_enum: bool) -> Result<(), String> {
    let mut derives = HashSet::new();
    let mut repr = false;
    for a in attrs {
        match a.name.as_str() {
            "derive" => {
                for d in &a.args {
                    if ![
                        "Serialize",
                        "Deserialize",
                        "Json",
                        "Debug",
                        "Clone",
                        "Default",
                    ]
                    .contains(&d.as_str())
                    {
                        return Err(format!("unknown derive '{d}'"));
                    }
                    if !derives.insert(d) {
                        return Err(format!("duplicate derive '{d}'"));
                    }
                    if is_enum {
                        return Err(format!("derive '{d}' on enums is not yet supported"));
                    }
                }
            }
            "capabilities" if !is_enum && !a.args.is_empty() => {
                // Registry already validated the actual trait implementations.
                // No capabilities flags are carried to LLVM.
            }
            "requires" if !is_enum && !a.args.is_empty() => {
                // Structural concept obligations were verified by Registry.
            }
            "repr" if a.args.len() == 1 && a.args[0] == "C" && !is_enum => {
                if repr {
                    return Err("duplicate #[repr(C)]".into());
                }
                repr = true;
            }
            _ => {
                // Nova Meta has already verified this source-defined attribute
                // and its target. Metadata with no lowering hook is erased here.
            }
        }
    }
    Ok(())
}
fn contains_type_var(ty: &TypeRef, variable: &str) -> bool {
    match ty {
        TypeRef::Named(n) => n == variable,
        TypeRef::Applied(_, args) => args.iter().any(|t| contains_type_var(t, variable)),
        TypeRef::Array(t, _)
        | TypeRef::Pointer { inner: t, .. }
        | TypeRef::Reference { inner: t, .. } => contains_type_var(t, variable),
        TypeRef::Function { params, result } => {
            params.iter().any(|x| contains_type_var(x, variable))
                || contains_type_var(result, variable)
        }
    }
}
fn is_integer(ty: &TypeRef) -> bool {
    matches!(ty,TypeRef::Named(n) if ["i8","i16","i32","i64","u8","u16","u32","u64","isize","usize"].contains(&n.as_str()))
}

#[cfg(test)]
mod readable_type_diagnostic_tests {
    use super::*;

    #[test]
    fn reports_borrowed_mutex_mismatch_in_source_type_syntax() {
        let mutex = TypeRef::Named("std__old__sync__MutexI64".to_owned());
        let borrow = TypeRef::Reference {
            mutable: false,
            inner: Box::new(mutex.clone()),
        };
        let nested = TypeRef::Reference {
            mutable: false,
            inner: Box::new(borrow.clone()),
        };
        let mismatch = type_variables(&nested, &borrow, &[], &mut HashMap::new()).unwrap_err();
        assert!(
            mismatch.contains("expected std::old::sync::MutexI64, found &std::old::sync::MutexI64"),
            "{mismatch}"
        );
        assert_eq!(display_type(&nested), "&&std::old::sync::MutexI64");
        assert!(!mismatch.contains("Named("), "{mismatch}");
    }

    #[test]
    fn displays_monomorphized_result_types_as_source_generic_syntax() {
        let result = TypeRef::Named(
            "std__error__result__Result__lib__value__JsonValue__lib__parser__JsonParserError".into(),
        );
        assert_eq!(
            display_type(&result),
            "std::error::result::Result<lib::value::JsonValue, lib::parser::JsonParserError>"
        );
    }

    #[test]
    fn displays_single_scalar_generic_arguments() {
        let option = TypeRef::Named("std__core__option__Option__i64".into());
        assert_eq!(display_type(&option), "std::core::option::Option<i64>");
    }

    #[test]
    fn uses_nova_syntax_for_advanced_types() {
        let type_ref = TypeRef::Function {
            params: vec![
                TypeRef::Reference {
                    mutable: true,
                    inner: Box::new(TypeRef::Named("i64".into())),
                },
                TypeRef::Array(Box::new(TypeRef::Named("u8".into())), 4),
            ],
            result: Box::new(TypeRef::Pointer {
                mutable: false,
                inner: Box::new(TypeRef::Named("str".into())),
            }),
        };
        assert_eq!(
            display_type(&type_ref),
            "fn(&mut i64, [u8; 4]) -> *const str"
        );
    }
}

#[cfg(test)]
mod borrowed_field_receiver_regressions {
    use super::*;

    #[test]
    fn dereferenced_borrowed_owner_fields_remain_addressable() {
        let borrowed_field = Expr::Field(
            Box::new(Expr::Deref(Box::new(Expr::Var("self".into())))),
            "value".into(),
        );
        assert!(addressable_receiver(&borrowed_field));

        // A computed owner is not an lvalue, including when used as a field
        // receiver. This keeps the temporary-borrow prohibition unchanged.
        let temporary = Expr::Call("make_value".into(), vec![]);
        assert!(!addressable_receiver(&temporary));
        assert!(!addressable_receiver(&Expr::Field(
            Box::new(temporary),
            "value".into(),
        )));
    }

    #[test]
    fn borrowed_method_can_call_method_on_own_field() {
        let source = r#"
            struct Text { amount: i64 }
            impl Text {
                fn value(self: &Text) -> i64 { return self.amount; }
            }
            struct Holder { text: Text }
            impl Holder {
                fn read(self: &Holder) -> i64 {
                    return self.text.value();
                }
            }
            fn main() -> i32 {
                let holder = new Holder { text: new Text { amount: 7 } };
                if holder.read() != 7 { return 1; }
                return 0;
            }
        "#;
        let parsed = crate::grammar::ProgramParser::new()
            .parse(source)
            .expect("borrowed field receiver example must parse");
        let lowered =
            lower(&parsed).expect("field of a borrowed owner must be implicitly borrowable");
        crate::hir::lower(&lowered).expect("borrowing a field must preserve ownership checks");
    }
}


#[cfg(test)]
mod imported_type_resolution_regressions {
    use super::*;

    fn stub_struct(name: &str) -> StructDef {
        StructDef {
            decl: ast::Struct {
                name: name.rsplit("::").next().unwrap_or(name).to_owned(),
                generics: vec![],
                concept_bounds: vec![],
                fields: vec![],
                attributes: vec![],
                is_class: false,
                parent: None,
            },
            namespace: name.rsplit_once("::").map(|(ns, _)| ns).unwrap_or("").to_owned(),
        }
    }

    #[test]
    fn resolves_unique_imported_std_type_when_short_name_survives_expansion() {
        let mut engine = Engine::default();
        engine.structs.insert(
            "std::text::string::String".into(),
            stub_struct("std::text::string::String"),
        );

        assert_eq!(
            engine.resolve_declared_type_name("String", "lib::value"),
            "std::text::string::String"
        );
    }

    #[test]
    fn prefers_explicit_import_when_short_type_names_are_ambiguous() {
        let mut engine = Engine::default();
        engine.structs.insert(
            "std::text::string::String".into(),
            stub_struct("std::text::string::String"),
        );
        engine.structs.insert(
            "example::String".into(),
            stub_struct("example::String"),
        );
        engine.imports.push((
            "lib::value".into(),
            "std::text::string::String".into(),
        ));

        assert_eq!(
            engine.resolve_declared_type_name("String", "lib::value"),
            "std::text::string::String"
        );
    }
}

#[cfg(test)]
mod recursive_enum_variant_self_regressions {
    use super::*;

    #[test]
    fn resolves_self_inside_generic_container_for_enum_constructor() {
        let source = r#"
            struct Vec<T> { value: T }
            enum JsonValue { array(Vec<Self>) }
            fn build(values: Vec<JsonValue>) -> JsonValue {
                return JsonValue::array(values);
            }
        "#;
        let parsed = crate::grammar::ProgramParser::new()
            .parse(source)
            .expect("recursive enum constructor example must parse");
        lower(&parsed).expect("Self in enum payload must resolve to the concrete enum type");
    }
}

#[cfg(test)]
mod deferred_generic_destructor_regressions {
    use super::*;

    #[test]
    fn nested_type_instantiation_tracks_depth_and_defers_destructors() {
        let mut engine = Engine::default();
        engine.type_instantiation_depth = 1;
        let pending = (
            "std::collections::vec::Vec::drop".to_owned(),
            vec![TypeRef::Named("lib__value__JsonValue".into())],
        );
        engine.pending_destructors.push(pending.clone());
        if !engine.pending_destructors.contains(&pending) {
            engine.pending_destructors.push(pending.clone());
        }

        assert_eq!(engine.pending_destructors.len(), 1);
        assert_eq!(engine.type_instantiation_depth, 1);
        assert!(!engine.flushing_deferred_destructors);
    }
}

#[cfg(test)]
mod mutable_borrowed_enum_match_regressions {
    use super::*;

    #[test]
    fn match_mut_preserves_mutability_of_enum_payload_bindings() {
        let source = r#"
            enum Payload { Array(Vec<i64>), Empty }

            fn main() -> i32 {
                let mut value = Payload::Array(Vec::new::<i64>());
                match &mut value {
                    Payload::Array(items) => {
                        items.push(7);
                    },
                    Payload::Empty => {}
                }
                return 0;
            }
        "#;

        let parsed = crate::grammar::ProgramParser::new()
            .parse(source)
            .expect("mutable borrowed enum match example must parse");
        let lowered = lower(&parsed)
            .expect("surface lowering must preserve &mut on enum payloads");
        crate::hir::lower(&lowered)
            .expect("HIR and ownership must accept mutation through a borrowed enum payload");
    }

    #[test]
    fn match_shared_keeps_enum_payload_bindings_immutable() {
        let source = r#"
            enum Payload { Array(Vec<i64>), Empty }

            fn main() -> i32 {
                let value = Payload::Array(Vec::new::<i64>());
                match &value {
                    Payload::Array(items) => {
                        let count: i64 = items.len() as i64;
                        if count < 0 { return 1; }
                    },
                    Payload::Empty => {}
                }
                return 0;
            }
        "#;

        let parsed = crate::grammar::ProgramParser::new()
            .parse(source)
            .expect("shared borrowed enum match example must parse");
        let lowered = lower(&parsed)
            .expect("surface lowering must preserve shared enum payload borrows");
        crate::hir::lower(&lowered)
            .expect("HIR and ownership must accept reads through a shared enum payload");
    }
}
