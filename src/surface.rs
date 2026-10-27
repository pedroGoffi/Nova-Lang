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
}

pub fn lower(input: &ast::Program) -> Result<ast::Program, String> {
    let mut engine = Engine::default();
    let expanded = crate::macros::expand(input)?;
    let expanded = crate::concurrency::lower_and_check(&expanded)?;
    let expanded = crate::call_arguments::lower(&expanded)?;
    crate::effects::validate(&expanded)?;
    let expanded = crate::consteval::fold(&expanded)?;
    engine.concepts = crate::concepts::Registry::collect(&expanded)?;
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
    for name in concrete_types {
        engine.instantiate_type(&name, &[])?;
    }
    for name in concrete {
        engine.instantiate_function(&name, &[], None)?;
    }
    for (ns, path) in &engine.imports {
        let q = join(ns, path);
        let found = engine
            .funcs
            .keys()
            .chain(engine.structs.keys())
            .chain(engine.enums.keys())
            .any(|name| name.starts_with(&format!("{q}::")) || name == &q)
            || engine
                .funcs
                .keys()
                .chain(engine.structs.keys())
                .chain(engine.enums.keys())
                .any(|name| name.starts_with(&format!("{path}::")));
        if !found {
            return Err(format!("import '{path}' cannot be resolved"));
        }
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
}
fn join(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.into()
    } else {
        format!("{ns}::{name}")
    }
}
fn ident(name: &str) -> String {
    name.replace("::", "__")
}
fn type_suffix(t: &TypeRef) -> String {
    match t {
        TypeRef::Named(name) => ident(name),
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
// Human-readable Nova types in type-inference errors. Preserve the AST's
// structured types internally; never require the user to interpret Debug output.
fn display_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named(name) => name.replace("__", "::"),
        TypeRef::Applied(name, args) => format!(
            "{}<{}>",
            name.replace("__", "::"),
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

// Structural type unification used by generic-call inference. Never silently
// accept an incompatible array length, pointer mutability or concrete type.
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
            } if mutable == got => type_variables(a, p, generics, map),
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
        || (matches!(from, TypeRef::Function { .. }) && to == &TypeRef::Named("u64".into()))
        || (from == &TypeRef::Named("u64".into()) && matches!(to, TypeRef::Function { .. }))
        || (matches!(from, TypeRef::Pointer { .. }) && matches!(to, TypeRef::Pointer { .. }))
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
                src_bits < dst_bits
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
        Expr::MethodCall(base, _, args) => {
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
                parent_method.params[0].ty = derived_ty.clone();
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
                method_decl.params[0].ty = derived_ty.clone();
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
                    if self
                        .funcs
                        .insert(
                            name.clone(),
                            FuncDef {
                                decl: f.clone(),
                                namespace: ns.into(),
                                impl_generics: vec![],
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("duplicate function '{name}'"));
                    }
                }
                Item::Impl(block) => {
                    check_params(&block.generics)?;
                    let target_name = match &block.target {
                        TypeRef::Named(n) | TypeRef::Applied(n, _) => n,
                        _ => return Err("impl target must be a named type".into()),
                    };
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
                        if self
                            .funcs
                            .insert(
                                name.clone(),
                                FuncDef {
                                    decl: method.clone(),
                                    namespace: ns.into(),
                                    impl_generics: block.generics.clone(),
                                },
                            )
                            .is_some()
                        {
                            return Err(format!("duplicate method '{name}'"));
                        }
                        if method.params.first().is_none() {
                            return Err(format!(
                                "method '{name}' requires explicit self: Type parameter"
                            ));
                        }
                        if method.params[0].name != "self" {
                            return Err(format!(
                                "method '{name}' must declare self: Type as first parameter"
                            ));
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
    fn qualify(&self, name: &str, ns: &str) -> String {
        if name.contains("::") || ns.is_empty() {
            name.into()
        } else {
            let near = join(ns, name);
            if self.structs.contains_key(&near)
                || self.enums.contains_key(&near)
                || self.funcs.contains_key(&near)
                || self.externs.contains_key(&near)
            {
                near
            } else {
                name.into()
            }
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
                let q = self.qualify(&n, ns);
                if self.structs.contains_key(&q) || self.enums.contains_key(&q) {
                    self.instantiate_type(&q, &[])
                } else if self.structs.contains_key(&n) || self.enums.contains_key(&n) {
                    self.instantiate_type(&n, &[])
                } else {
                    Ok(TypeRef::Named(n))
                }
            }
            TypeRef::Applied(n, args) => {
                let args = args
                    .iter()
                    .map(|x| self.resolved_type(x, ns, subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let q = self.qualify(&n, ns);
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
        // The stage-0 coroutine runtime transports a single signed 64-bit ABI
        // word. Generic owners are a *compile-time* type contract, not a way
        // to reinterpret an arbitrary struct, pointer or f64 as that word.
        // Keep the restriction in monomorphization so all construction paths,
        // including explicitly instantiated globals/fields, are validated.
        if matches!(
            name,
            "std::task::Task"
                | "std::task::Future"
                | "std::generator::Generator"
                | "std::coro::Coroutine"
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
        let (fields, params, attributes, ns, is_enum) = if let Some(s) = self.structs.get(name) {
            (
                s.decl.fields.clone(),
                s.decl.generics.clone(),
                s.decl.attributes.clone(),
                s.namespace.clone(),
                false,
            )
        } else if let Some(e) = self.enums.get(name) {
            (
                vec![],
                e.decl.generics.clone(),
                e.decl.attributes.clone(),
                e.namespace.clone(),
                true,
            )
        } else {
            return Err(format!("unknown generic type '{name}'"));
        };
        if params.len() != args.len() {
            return Err(format!(
                "type '{name}' expects {} template parameters, got {}",
                params.len(),
                args.len()
            ));
        }
        let subst = params
            .into_iter()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        validate_attrs(&attributes, is_enum)?;
        self.type_cache
            .insert(concrete.clone(), TypeRef::Named(concrete.clone()));
        self.instance_args.insert(concrete.clone(), args.to_vec());
        let mut concrete_fields = Vec::<ast::Field>::new();
        if is_enum {
            let variants = self.enums[name].decl.variants.clone();
            concrete_fields.push(ast::Field {
                name: "__nova_tag".into(),
                ty: TypeRef::Named("i32".into()),
            });
            let mut metas = vec![];
            for v in variants {
                let mut payload = vec![];
                for (i, typ) in v.payload.iter().enumerate() {
                    let ty = self.resolved_type(typ, &ns, &subst)?;
                    let field = format!("{}_{}", v.name, i);
                    concrete_fields.push(ast::Field {
                        name: field.clone(),
                        ty: ty.clone(),
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
            fields: concrete_fields.clone(),
            attributes: if emits.is_empty() {
                vec![]
            } else {
                vec![ast::Attribute {
                    name: "derive".into(),
                    args: emits,
                }]
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
                }],
                variadic: false,
                result: name_ty.clone(),
                body: vec![Stmt::Return(Some(Expr::Var("value".into())))],
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            }));
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
                    self.instantiate_function(&destructor, args, None)?;
                }
            }
        }
        Ok(TypeRef::Named(concrete))
    }
    fn zero_expr(&mut self, ty: &TypeRef) -> Result<Expr, String> {
        match ty {
            TypeRef::Named(n) if n == "bool" => Ok(Expr::Bool(false)),
            TypeRef::Named(n) if n == "str" => Ok(Expr::Str("\"\"".into())),
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
            _ => Err(format!("type '{ty:?}' has no safe default value")),
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
        let result = self.resolved_type(&template.decl.result, &template.namespace, &subst)?;
        if !self.func_cache.insert(key.clone()) {
            return Ok((key, result));
        }
        let mut params = vec![];
        let mut scope = Scope {
            vars: HashMap::new(),
            namespace: template.namespace.clone(),
            subst: subst.clone(),
            returns: result.clone(),
        };
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
            });
        }
        // Preserve the real monomorphization/call-lowering chain for diagnostics.
        // These frames are compile-time context, not a runtime stack trace.
        let body = self
            .statements(&template.decl.body, &mut scope)
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
                            return Err(format!("local '{name}' expected {t:?}, found {got:?}"));
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
                    let expected = scope
                        .vars
                        .get(name)
                        .ok_or_else(|| format!("unknown local '{name}'"))?
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
                    let expected = fields
                        .iter()
                        .find(|f| f.name == *field)
                        .ok_or_else(|| format!("'{name}' has no field '{field}'"))?
                        .ty
                        .clone();
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
                    let (value, _) = self.expression(value, scope, Some(&expected))?;
                    output.push(Stmt::CompoundSet {
                        name: name.clone(),
                        op: *op,
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
                        return Err(format!("for range bounds must have equal types; variable '{name}' starts as {ty:?}, but ends as {upper_ty:?}"));
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
                Stmt::Match { value, arms } => {
                    let (value, ty) = self.expression(value, scope, None)?;
                    let TypeRef::Named(type_name) = ty else {
                        return Err("match currently requires an enum value".into());
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
                        ty: Some(TypeRef::Named(type_name)),
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
                                for (binding, (field, ty)) in bindings.iter().zip(fields) {
                                    child.vars.insert(binding.clone(), ty.clone());
                                    prefix.push(Stmt::Let {
                                        name: binding.clone(),
                                        mutable: false,
                                        ty: Some(ty.clone()),
                                        init: Expr::Field(
                                            Box::new(Expr::Var(tmp.clone())),
                                            field.clone(),
                                        ),
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
            TypeRef::Named(name) => match name.as_str() {
                "bool" | "i8" | "u8" => Ok((1, 1)),
                "i16" | "u16" => Ok((2, 2)),
                "i32" | "u32" | "f32" => Ok((4, 4)),
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
            if &actual != want && implicit_numeric_widen(&actual, want) {
                return Ok((Expr::Cast(Box::new(value), want.clone()), want.clone()));
            }
        }
        Ok((value, actual))
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
                let result =
                    self.resolved_type(&func.decl.result, &func.namespace, &scope.subst)?;
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
                let target = self.resolved_type(target, &scope.namespace, &scope.subst)?;
                let (value, from) = self.expression(value, scope, None)?;
                if !castable(&from, &target) {
                    return Err(format!("cannot cast {from:?} to {target:?}; numeric or explicit function-handle casts only"));
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
                let (mut a, ta) = self.expression(a, scope, hint)?;
                // Evaluate a non-literal RHS on its own: literal RHS keeps LHS context
                // so `i64_value + 2` does not force an unnecessary conversion.
                let right_hint = if matches!(b.as_ref(), Expr::Int(_) | Expr::Float(_)) {
                    Some(&ta)
                } else {
                    None
                };
                let (mut b, tb) = self.expression(b, scope, right_hint)?;
                // Struct operators dispatch through regular, typechecked trait
                // methods. There is no special-purpose LLVM/runtime operator.
                // Primitive operators always retain their native behavior.
                if let TypeRef::Named(receiver_type) = &ta {
                    if self.concrete_fields.contains_key(receiver_type) {
                        let method = match op {
                            BinaryOp::Add => Some("add"),
                            BinaryOp::Sub => Some("sub"),
                            BinaryOp::Mul => Some("mul"),
                            BinaryOp::Div => Some("div"),
                            BinaryOp::Rem => Some("rem"),
                            BinaryOp::Eq => Some("eq"),
                            BinaryOp::Ne => Some("ne"),
                            BinaryOp::Lt => Some("lt"),
                            BinaryOp::Le => Some("le"),
                            BinaryOp::Gt => Some("gt"),
                            BinaryOp::Ge => Some("ge"),
                            BinaryOp::BitAnd => Some("bit_and"),
                            BinaryOp::BitOr => Some("bit_or"),
                            BinaryOp::BitXor => Some("bit_xor"),
                            BinaryOp::Shl => Some("shift_left"),
                            BinaryOp::Shr => Some("shift_right"),
                            BinaryOp::And | BinaryOp::Or => None,
                        };
                        if let Some(method) = method {
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
                                return Err(format!("operator {op:?} on '{receiver_type}' requires a matching '{method}' method (typically via std::ops trait)"));
                            }
                            return self.expression(
                                &Expr::MethodCall(Box::new(a), method.into(), vec![b]),
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
                    let to=promote_numeric(&ta,&tb).ok_or_else(||format!("no lossless implicit numeric conversion between {ta:?} and {tb:?}; use 'as'"))?;
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
                // `format!` returns an owned std::string::String. A chain of
                // handle-returning native append operations evaluates each
                // argument exactly once, left-to-right, then installs the
                // finished handle into the ordinary RAII String owner.
                if name == crate::format_macros::BUILD {
                    let mut handle =
                        Expr::Call("std::string::native_string_create_empty".into(), vec![]);
                    for part in original_args {
                        let Expr::Call(marker, data) = part else {
                            return Err("error[NOVA-FMT005]: malformed format AST part".into());
                        };
                        if marker == crate::format_macros::TEXT_PART {
                            if data.len() != 1 || !matches!(&data[0], Expr::Str(_)) {
                                return Err("error[NOVA-FMT005]: malformed format literal".into());
                            }
                            handle = Expr::Call(
                                "std::string::native_format_append_text".into(),
                                vec![handle, data[0].clone()],
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
                            .map_err(|_| "error[NOVA-FMT005]: invalid format precision")?;
                        let Expr::Bool(debug) = &data[2] else {
                            return Err("error[NOVA-FMT005]: debug mode must be static".into());
                        };
                        let (_, ty) = self.expression(&data[0], scope, None)?;
                        let TypeRef::Named(kind) = &ty else {
                            return Err(format!(
                                "error[NOVA-FMT006]: type {} is not formattable",
                                display_type(&ty)
                            ));
                        };
                        if precision >= 0 && kind != "f32" && kind != "f64" {
                            return Err(format!("error[NOVA-FMT004]: decimal precision applies only to floating values, not {}", display_type(&ty)));
                        }
                        let value = data[0].clone();
                        let (suffix, forwarded) = match kind.as_str() {
                            "str" => (if *debug { "debug_str" } else { "text" }, vec![value]),
                            "bool" => ("bool", vec![value]),
                            "i8" | "i16" | "i32" | "i64" | "isize" => (
                                "i64",
                                vec![Expr::Cast(Box::new(value), TypeRef::Named("i64".into()))],
                            ),
                            "u8" | "u16" | "u32" | "u64" | "usize" => (
                                "u64",
                                vec![Expr::Cast(Box::new(value), TypeRef::Named("u64".into()))],
                            ),
                            "f32" | "f64" => (
                                "f64",
                                vec![
                                    Expr::Cast(Box::new(value), TypeRef::Named("f64".into())),
                                    Expr::Int(precision.to_string()),
                                ],
                            ),
                            _ => {
                                let is_string = kind == "std__string__String";
                                // A temporary owned String cannot be implicitly borrowed
                                // by `.to_string()`. Consume the temporary in a named
                                // STD parameter instead; its RAII drop runs *after* the
                                // borrowed text has been appended to the destination.
                                if is_string && !matches!(&value, Expr::Var(_)) {
                                    let function = if *debug {
                                        "std::string::format_append_owned_debug"
                                    } else {
                                        "std::string::format_append_owned_text"
                                    };
                                    handle = Expr::Call(function.into(), vec![handle, value]);
                                    continue;
                                }
                                let method = if *debug && !is_string {
                                    "to_debug_string"
                                } else {
                                    "to_string"
                                };
                                let (_, result) = self.expression(
                                    &Expr::MethodCall(Box::new(value.clone()), method.into(), vec![]),
                                    scope,
                                    Some(&TypeRef::Named("str".into())),
                                ).map_err(|error|format!("error[NOVA-FMT006]: type {} cannot be formatted; implement `{method}() -> str`\n  = {error}", display_type(&ty)))?;
                                if result != TypeRef::Named("str".into()) {
                                    return Err(format!(
                                        "error[NOVA-FMT006]: `{method}()` must return str, got {}",
                                        display_type(&result)
                                    ));
                                }
                                (
                                    if *debug && is_string {
                                        "debug_str"
                                    } else {
                                        "text"
                                    },
                                    vec![Expr::MethodCall(Box::new(value), method.into(), vec![])],
                                )
                            }
                        };
                        let mut args = vec![handle];
                        args.extend(forwarded);
                        handle =
                            Expr::Call(format!("std::string::native_format_append_{suffix}"), args);
                    }
                    let owned = Expr::StructInit(
                        "std::string::String".into(),
                        vec![("handle".into(), handle)],
                    );
                    return self.expression(&owned, scope, expected).map_err(|error| {
                        if error.contains("unknown function") || error.contains("unknown struct") {
                            format!("error[NOVA-FMT007]: `format!` requires the built-in STD (declare std in Nova.toml)\n  = {error}")
                        } else { error }
                    });
                }
                // Format output is expanded in the macro AST pass, but only
                // the surface checker knows the concrete type of each value.
                // Dispatch to typed STD intrinsics here: no printf varargs,
                // runtime dynamic type tags, or duplicated evaluations.
                if name == crate::format_macros::WRITE_VALUE {
                    if original_args.len() != 4 {
                        return Err("error[NOVA-FMT005]: malformed internal format write".into());
                    }
                    let Expr::Int(precision_text) = &original_args[1] else {
                        return Err(
                            "error[NOVA-FMT005]: format precision must be a compile-time integer"
                                .into(),
                        );
                    };
                    let precision: i32 = precision_text
                        .parse()
                        .map_err(|_| "error[NOVA-FMT005]: invalid format precision")?;
                    let Expr::Bool(debug) = &original_args[2] else {
                        return Err("error[NOVA-FMT005]: debug mode must be static".into());
                    };
                    let Expr::Bool(stderr) = &original_args[3] else {
                        return Err("error[NOVA-FMT005]: output stream must be static".into());
                    };
                    let (_, ty) = self.expression(&original_args[0], scope, None)?;
                    // Use the source AST when building the final call: a typed
                    // AST returned above may contain already-monomorphized
                    // names which are not valid inputs to a second lowering.
                    // This rechecks types at compile time, but still emits
                    // exactly one runtime evaluation of the argument.
                    let value = original_args[0].clone();
                    let TypeRef::Named(kind) = &ty else {
                        return Err(format!("error[NOVA-FMT006]: type {} is not printable; use a type with `to_string() -> str`", display_type(&ty)));
                    };
                    if precision >= 0 && kind != "f32" && kind != "f64" {
                        return Err(format!("error[NOVA-FMT004]: decimal precision applies only to floating values, not {}", display_type(&ty)));
                    }
                    let suffix = if *stderr { "eprint" } else { "print" };
                    let (callee, forwarded) = match kind.as_str() {
                        "str" => (
                            format!(
                                "std::io::native_{suffix}{}",
                                if *debug { "_debug_str" } else { "" }
                            ),
                            vec![value],
                        ),
                        "bool" => (format!("std::io::native_{suffix}_bool"), vec![value]),
                        "i8" | "i16" | "i32" | "i64" | "isize" => (
                            format!("std::io::native_{suffix}_i64"),
                            vec![Expr::Cast(Box::new(value), TypeRef::Named("i64".into()))],
                        ),
                        "u8" | "u16" | "u32" | "u64" | "usize" => (
                            format!("std::io::native_{suffix}_u64"),
                            vec![Expr::Cast(Box::new(value), TypeRef::Named("u64".into()))],
                        ),
                        "f32" | "f64" => (
                            format!("std::io::native_{suffix}_f64"),
                            vec![
                                Expr::Cast(Box::new(value), TypeRef::Named("f64".into())),
                                Expr::Int(precision.to_string()),
                            ],
                        ),
                        _ => {
                            // For records, respect the user's own method
                            // contract; normal method resolution validates its
                            // signature and the returned borrowed `str`.
                            // The standard owned String exposes `to_string`
                            // as a borrowed str. Debug quoting still uses
                            // the native escaped-str printer, rather than
                            // requiring a redundant String::to_debug_string.
                            let is_owned_string = kind == "std__string__String";
                            // Keep named String values borrowed (never move the
                            // caller's owner). An rvalue String needs a named
                            // receiver during conversion, so hand ownership to
                            // a STD helper whose parameter owns the temporary.
                            if is_owned_string && !matches!(&value, Expr::Var(_)) {
                                let helper = match (*stderr, *debug) {
                                    (false, false) => "std::io::print_owned_string",
                                    (false, true) => "std::io::print_owned_string_debug",
                                    (true, false) => "std::io::eprint_owned_string",
                                    (true, true) => "std::io::eprint_owned_string_debug",
                                };
                                return self.expression(
                                    &Expr::Call(helper.into(), vec![value]),
                                    scope,
                                    expected,
                                );
                            }
                            let method = if *debug && !is_owned_string {
                                "to_debug_string"
                            } else {
                                "to_string"
                            };
                            let (formatted, result) = self.expression(
                                &Expr::MethodCall(Box::new(value), method.into(), vec![]),
                                scope,
                                Some(&TypeRef::Named("str".into())),
                            ).map_err(|error|format!("error[NOVA-FMT006]: type {} cannot be formatted with `{{{}}}`; implement `{method}() -> str`\n  = {error}",display_type(&ty),if *debug{":?"}else{""}))?;
                            if result != TypeRef::Named("str".into()) {
                                return Err(format!(
                                    "error[NOVA-FMT006]: `{method}()` must return str, got {}",
                                    display_type(&result)
                                ));
                            }
                            (
                                format!(
                                    "std::io::native_{suffix}{}",
                                    if *debug && is_owned_string {
                                        "_debug_str"
                                    } else {
                                        ""
                                    }
                                ),
                                vec![formatted],
                            )
                        }
                    };
                    return self.expression(&Expr::Call(callee,forwarded),scope,expected).map_err(|error| {
                        if error.contains("unknown function") {
                            format!("error[NOVA-FMT007]: print macros require the built-in STD (add `std = {{ builtin = \"std\" }}` to Nova.toml)\n  = {error}")
                        } else {error}
                    });
                }
                let args = self.expand_call_args(original_args, scope)?;
                // Coroutine keywords are lowered to legacy runtime calls before
                // type inference. When the operand is a typed owner, dispatch
                // to the generic method instead of exposing its raw ABI handle.
                // This preserves `await task -> T`, `join task -> T` and
                // `next generator -> T` without changing the stage-0 runtime.
                if args.len() == 1 {
                    let method = match name.as_str() {
                        "std::task::native_await_value" => {
                            Some(("std__task__Task__", "std__task__Future__", "await_result"))
                        }
                        "std::task::native_block_on" => {
                            Some(("std__task__Task__", "std__task__Future__", "wait"))
                        }
                        "std::task::native_spawn" => {
                            Some(("std__task__Task__", "std__task__Future__", "schedule"))
                        }
                        "std::generator::native_next" => Some((
                            "std__generator__Generator__",
                            "std__generator__Generator__",
                            "next",
                        )),
                        "std::generator::native_done" => Some((
                            "std__generator__Generator__",
                            "std__generator__Generator__",
                            "finished",
                        )),
                        "std::coro::native_drop" => {
                            Some(("std__task__Task__", "std__task__Future__", "drop"))
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
                                ),
                                scope,
                                expected,
                            );
                        }
                    }
                    if matches!(
                        name.as_str(),
                        "std::generator::native_done" | "std::coro::native_drop"
                    ) {
                        let (_, type_ref) = self.expression(&args[0], scope, None)?;
                        if let TypeRef::Named(owner) = type_ref {
                            let dispatch = match name.as_str() {
                                "std::generator::native_done"
                                    if owner.starts_with("std__coro__Coroutine__") =>
                                {
                                    Some("completed")
                                }
                                "std::coro::native_drop"
                                    if owner.starts_with("std__generator__Generator__")
                                        || owner.starts_with("std__coro__Coroutine__") =>
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
                                    ),
                                    scope,
                                    expected,
                                );
                            }
                        }
                    }
                }
                if args.len() == 2 && name == "std::coro::native_resume" {
                    let (_, type_ref) = self.expression(&args[0], scope, None)?;
                    if matches!(&type_ref,TypeRef::Named(owner) if owner.starts_with("std__coro__Coroutine__"))
                    {
                        return self.expression(
                            &Expr::MethodCall(
                                Box::new(args[0].clone()),
                                "resume".into(),
                                vec![args[1].clone()],
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
                let q = self.qualify(name, &scope.namespace);
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
                let template = self
                    .funcs
                    .get(&q)
                    .ok_or_else(|| format!("unknown function '{name}'"))?
                    .clone();
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
                    let pattern = if generics.is_empty() {
                        concrete_hint.as_ref().unwrap_or(&p.ty)
                    } else {
                        &p.ty
                    };
                    type_variables(&ty,pattern,&generics,&mut map).map_err(|error|format!(
                        "{error}\n  = argument {} (`{}`) of call `{name}`\n  = declared parameter: {}\n  = supplied argument: {}",
                        i + 1, p.name, display_type(pattern), display_type(&ty)
                    ))?;
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
                    &q,
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
                            "call '{name}' produces {result:?}, expected {want:?}"
                        ));
                    }
                }
                Ok((Expr::Call(fun, actual), result))
            }
            Expr::GenericCall(name, types, original_args) => {
                let args = self.expand_call_args(original_args, scope)?;
                let q = self.qualify(name, &scope.namespace);
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
                // Compiler intrinsic: an ABI-correct allocation size for native
                // memory. The runtime is intentionally unaware of Nova types.
                if q == "std::raw::size_of" || q == "std::raw::align_of" {
                    if types.len() != 1 || !args.is_empty() {
                        return Err(
                            "std::raw::{size_of,align_of}::<T>() expects one type and no arguments"
                                .into(),
                        );
                    }
                    let concrete_type =
                        self.resolved_type(&types[0], &scope.namespace, &scope.subst)?;
                    let (size, alignment) = self.layout_of(&concrete_type, &mut Vec::new())?;
                    let requested_value = if q == "std::raw::size_of" {
                        size
                    } else {
                        alignment
                    };
                    return Ok((
                        Expr::Int(requested_value.to_string()),
                        TypeRef::Named("u64".into()),
                    ));
                }
                let concrete = types
                    .iter()
                    .map(|t| self.resolved_type(t, &scope.namespace, &scope.subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let template = self
                    .funcs
                    .get(&q)
                    .ok_or_else(|| format!("unknown generic function '{name}'"))?
                    .clone();
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
                    &q,
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
                let info = defs
                    .iter()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| format!("'{n}' has no field '{field}'"))?;
                Ok((Expr::Field(Box::new(base), field.clone()), info.ty.clone()))
            }
            Expr::MethodCall(receiver, name, args) => {
                if matches!(receiver.as_ref(),Expr::Var(n) if n=="super") {
                    if !scope.vars.contains_key("self") {
                        return Err("super.method() requires an instance method".into());
                    }
                    let call = Expr::MethodCall(
                        Box::new(Expr::Var("self".into())),
                        format!("__super__{name}"),
                        args.clone(),
                    );
                    return self.expression(&call, scope, expected);
                }
                let (base, ty) = self.expression(receiver, scope, None)?;
                if name == "toJson" || name == "writeJson" {
                    if !args.is_empty() {
                        return Err(format!("{name}() takes no arguments"));
                    }
                    let TypeRef::Named(concrete) = &ty else {
                        return Err(format!("{name}() requires a struct value, not {ty:?}"));
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
                    .filter(|f| f.ends_with(&format!("::{name}")))
                    .cloned()
                    .collect::<Vec<_>>();
                candidates.sort();
                let selected=candidates.into_iter().find(|f| {
                    let target=f.rsplit_once("::").unwrap().0;
                    let right_type=concrete_name==ident(target) || concrete_name.starts_with(&format!("{}__",ident(target)));
                    let wants_pointer=matches!(self.funcs[f].decl.params.first().map(|p|&p.ty),Some(TypeRef::Pointer{..}|TypeRef::Reference{..}));
                    let can_auto_borrow=!pointer_receiver && matches!(self.funcs[f].decl.params.first().map(|p|&p.ty),Some(TypeRef::Reference{..}));
                    right_type && (pointer_receiver==wants_pointer || can_auto_borrow)
                }).ok_or_else(||format!("method '{name}' is not defined for '{concrete_name}' with this receiver type"))?;
                let template = self.funcs[&selected].clone();
                // Method receivers can borrow a named local for the duration of
                // the call, without transferring ownership to a by-value self.
                // Field/temp borrows require place projections in typed MIR and
                // are deliberately rejected instead of making an alias copy.
                let base = if !pointer_receiver {
                    if let Some(ast::Param {
                        ty: TypeRef::Reference { mutable, .. },
                        ..
                    }) = template.decl.params.first()
                    {
                        if !matches!(&base, Expr::Var(_)) {
                            return Err(format!(
                                "method '{name}' requires a named receiver for implicit borrow (receiver: '{concrete_name}', resolved method: '{selected}'); the receiver is a field or temporary expression"
                            ));
                        }
                        Expr::AddressOf {
                            mutable: *mutable,
                            value: Box::new(base),
                        }
                    } else {
                        base
                    }
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
                let base_args = self
                    .instance_args
                    .get(&concrete_name)
                    .cloned()
                    .unwrap_or_default();
                if template.impl_generics.len() != base_args.len() {
                    return Err(format!(
                        "method '{name}' does not match receiver generic arity"
                    ));
                }
                for (param, arg) in template.impl_generics.iter().zip(base_args) {
                    map.insert(param.clone(), arg);
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
                let TypeRef::Array(element, _) = ty else {
                    return Err("indexing requires an array".into());
                };
                let (index, _) = self.expression(index, scope, Some(&i32t()))?;
                Ok((Expr::Index(Box::new(value), Box::new(index)), *element))
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
        let params = self
            .structs
            .get(&q)
            .map(|d| d.decl.generics.clone())
            .ok_or_else(|| format!("unknown struct '{name}'"))?;
        let resolved = if args.is_empty() && !params.is_empty() {
            if let Some(expected) = expected {
                let want = self.resolved_type(expected, &scope.namespace, &scope.subst)?;
                if let TypeRef::Named(n) = want {
                    if self.concrete_fields.contains_key(&n) {
                        return Err("generic struct initializer needs explicit <T>".into());
                    }
                }
            }
            return Err(format!(
                "generic struct '{name}' requires explicit type arguments: new {name}<T> {{ ... }}"
            ));
        } else {
            self.instantiate_type(&q, args)?
        };
        let TypeRef::Named(ref n) = resolved else {
            unreachable!()
        };
        if let Some(want) = expected {
            if want != &resolved {
                return Err(format!(
                    "struct constructor for '{name}' produces {resolved:?}, expected {want:?}"
                ));
            }
        }
        let definitions = self.concrete_fields[n].clone();
        let mut actual = vec![];
        for (field, e) in fields {
            let ty = definitions
                .iter()
                .find(|f| f.name == *field)
                .ok_or_else(|| format!("unknown field '{field}' in '{name}'"))?
                .ty
                .clone();
            actual.push((field.clone(), self.expression(e, scope, Some(&ty))?.0));
        }
        Ok((Expr::StructInit(n.clone(), actual), resolved))
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
                }
            }
        }
        let mut actual = vec![];
        for (e, p) in args.iter().zip(&definition.payload) {
            let expected_ty = match p {
                TypeRef::Named(n) => map.get(n),
                _ => None,
            };
            let (expr, ty) = self.expression(e, scope, expected_ty)?;
            type_variables(&ty, p, &def.decl.generics, &mut map)?;
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
            "repr" if a.args.len() == 1 && a.args[0] == "C" && !is_enum => {
                if repr {
                    return Err("duplicate #[repr(C)]".into());
                }
                repr = true;
            }
            _ => {
                return Err(format!(
                    "unknown or unsupported attribute '#[{}({})]'",
                    a.name,
                    a.args.join(", ")
                ))
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
        let mutex = TypeRef::Named("std__sync__MutexI64".to_owned());
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
            mismatch.contains("expected std::sync::MutexI64, found &std::sync::MutexI64"),
            "{mismatch}"
        );
        assert_eq!(display_type(&nested), "&&std::sync::MutexI64");
        assert!(!mismatch.contains("Named("), "{mismatch}");
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
