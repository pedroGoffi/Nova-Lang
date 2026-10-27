//! C++20-inspired compile-time constraints. Concepts are evaluated on types
//! during generic specialization and erased before HIR/MIR/LLVM.
//! No user expression executes to decide a concept.
use crate::ast::{self, ConceptBound, ConceptExpr, Item, Requirement, TypeRef};
use std::collections::{HashMap, HashSet};

fn qualified(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.into()
    } else {
        format!("{ns}::{name}")
    }
}
fn lookup<'a, T>(
    definitions: &'a HashMap<String, T>,
    ns: &str,
    name: &str,
) -> Option<(&'a String, &'a T)> {
    if name.contains("::") {
        return definitions.get_key_value(name);
    }
    let mut current = ns;
    loop {
        if let Some(x) = definitions.get_key_value(&qualified(current, name)) {
            return Some(x);
        }
        if current.is_empty() {
            return None;
        }
        current = current
            .rsplit_once("::")
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
}
fn intrinsic(name: &str) -> Option<usize> {
    match name {
        "integer" | "signed" | "unsigned" | "floating" | "numeric" | "boolean" | "pointer"
        | "reference" => Some(1),
        "same_as" | "implements" | "has_field" | "has_method" => Some(2),
        _ => None,
    }
}
fn sub_type(ty: &TypeRef, subst: &HashMap<String, TypeRef>) -> TypeRef {
    match ty {
        TypeRef::Named(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        TypeRef::Applied(name, args) => TypeRef::Applied(
            name.clone(),
            args.iter().map(|a| sub_type(a, subst)).collect(),
        ),
        TypeRef::Array(item, n) => TypeRef::Array(Box::new(sub_type(item, subst)), *n),
        TypeRef::Pointer { mutable, inner } => TypeRef::Pointer {
            mutable: *mutable,
            inner: Box::new(sub_type(inner, subst)),
        },
        TypeRef::Reference { mutable, inner } => TypeRef::Reference {
            mutable: *mutable,
            inner: Box::new(sub_type(inner, subst)),
        },
        TypeRef::Function { params, result } => TypeRef::Function {
            params: params.iter().map(|a| sub_type(a, subst)).collect(),
            result: Box::new(sub_type(result, subst)),
        },
    }
}
fn normalized(ty: &TypeRef) -> TypeRef {
    match ty {
        TypeRef::Named(name) => TypeRef::Named(name.replace("::", "__")),
        TypeRef::Applied(name, args) => TypeRef::Applied(
            name.replace("::", "__"),
            args.iter().map(normalized).collect(),
        ),
        TypeRef::Array(item, n) => TypeRef::Array(Box::new(normalized(item)), *n),
        TypeRef::Pointer { mutable, inner } => TypeRef::Pointer {
            mutable: *mutable,
            inner: Box::new(normalized(inner)),
        },
        TypeRef::Reference { mutable, inner } => TypeRef::Reference {
            mutable: *mutable,
            inner: Box::new(normalized(inner)),
        },
        TypeRef::Function { params, result } => TypeRef::Function {
            params: params.iter().map(normalized).collect(),
            result: Box::new(normalized(result)),
        },
    }
}
fn bare_name(ty: &TypeRef) -> Option<&str> {
    if let TypeRef::Named(name) = ty {
        Some(name)
    } else {
        None
    }
}

#[derive(Clone, Debug)]
struct Entry {
    decl: ast::Concept,
    namespace: String,
}
#[derive(Default, Clone, Debug)]
pub struct Registry {
    entries: HashMap<String, Entry>,
    trait_parents: HashMap<String, Vec<String>>,
    implementations: Vec<(String, TypeRef, String)>,
    assertions: Vec<(ConceptExpr, String)>,
    structs: HashMap<String, (ast::Struct, String)>,
    methods: Vec<(TypeRef, Vec<ast::Function>, String)>,
}
impl Registry {
    pub fn collect(program: &ast::Program) -> Result<Self, String> {
        let mut registry = Self::default();
        registry.gather(&program.items, "")?;
        registry.validate()?;
        registry.validate_function_bounds(&program.items, "")?;
        Ok(registry)
    }
    fn gather(&mut self, items: &[Item], ns: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => self.gather(std::slice::from_ref(inner.as_ref()), ns)?,
                Item::Namespace { name, items } => self.gather(items, &qualified(ns, name))?,
                Item::ConceptAssert(expr) => self.assertions.push((expr.clone(), ns.into())),
                Item::Concept(decl) => {
                    let name = qualified(ns, &decl.name);
                    if self
                        .entries
                        .insert(
                            name.clone(),
                            Entry {
                                decl: decl.clone(),
                                namespace: ns.into(),
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("duplicate concept '{name}'"));
                    }
                }
                Item::Trait(decl) => {
                    self.trait_parents
                        .insert(qualified(ns, &decl.name), decl.parents.clone());
                }
                Item::Struct(decl) => {
                    let key = qualified(ns, &decl.name);
                    if self
                        .structs
                        .insert(key.clone(), (decl.clone(), ns.into()))
                        .is_some()
                    {
                        return Err(format!("duplicate struct '{key}'"));
                    }
                }
                Item::Impl(decl) => {
                    if let Some(name) = &decl.trait_name {
                        self.implementations
                            .push((name.clone(), decl.target.clone(), ns.into()));
                    }
                    self.methods
                        .push((decl.target.clone(), decl.methods.clone(), ns.into()));
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn validate_expr(
        &self,
        expr: &ConceptExpr,
        ns: &str,
        stack: &mut Vec<String>,
        done: &mut HashSet<String>,
    ) -> Result<(), String> {
        match expr {
            ConceptExpr::Literal(_) => Ok(()),
            ConceptExpr::Requires { variables, rules } => {
                let mut names = HashSet::new();
                for (name, _) in variables {
                    if !names.insert(name) {
                        return Err(format!("duplicate requires parameter '{name}'"));
                    }
                }
                if rules.is_empty() {
                    return Err("requires expression must contain at least one requirement".into());
                }
                for rule in rules {
                    match rule {
                        Requirement::Field { variable, .. }
                        | Requirement::Method { variable, .. } => {
                            if !names.contains(variable) {
                                return Err(format!("unknown requires parameter '{variable}'"));
                            }
                        }
                        Requirement::Binary { left, right, .. } => {
                            if !names.contains(left) || !names.contains(right) {
                                return Err(format!(
                                    "unknown requires operands '{left}' and '{right}'"
                                ));
                            }
                        }
                    }
                }
                Ok(())
            }
            ConceptExpr::Not(x) => self.validate_expr(x, ns, stack, done),
            ConceptExpr::And(a, b) | ConceptExpr::Or(a, b) => {
                self.validate_expr(a, ns, stack, done)?;
                self.validate_expr(b, ns, stack, done)
            }
            ConceptExpr::Apply { name, args } => {
                if let Some(arity) = intrinsic(name) {
                    if args.len() != arity {
                        return Err(format!(
                            "concept intrinsic '{name}' expects {arity} type arguments, got {}",
                            args.len()
                        ));
                    }
                    return Ok(());
                }
                let (canonical, entry) = lookup(&self.entries, ns, name)
                    .ok_or_else(|| format!("unknown concept '{name}'"))?;
                if entry.decl.generics.len() != args.len() {
                    return Err(format!(
                        "concept '{canonical}' expects {} type arguments, got {}",
                        entry.decl.generics.len(),
                        args.len()
                    ));
                }
                self.validate_entry(canonical, stack, done)
            }
        }
    }
    fn validate_entry(
        &self,
        name: &str,
        stack: &mut Vec<String>,
        done: &mut HashSet<String>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if stack.iter().any(|s| s == name) {
            return Err(format!("cyclic concept: {} -> {name}", stack.join(" -> ")));
        }
        let entry = &self.entries[name];
        stack.push(name.into());
        self.validate_expr(&entry.decl.predicate, &entry.namespace, stack, done)?;
        stack.pop();
        done.insert(name.into());
        Ok(())
    }
    fn validate(&self) -> Result<(), String> {
        let mut done = HashSet::new();
        for (name, entry) in &self.entries {
            if entry.decl.generics.is_empty() {
                return Err(format!(
                    "concept '{name}' requires at least one type parameter"
                ));
            }
            let mut names = HashSet::new();
            for param in &entry.decl.generics {
                if !names.insert(param) {
                    return Err(format!(
                        "concept '{name}' has duplicate type parameter '{param}'"
                    ));
                }
            }
            if intrinsic(name).is_some() {
                return Err(format!("concept '{name}' shadows a built-in concept"));
            }
            self.validate_entry(name, &mut Vec::new(), &mut done)?;
        }
        for (expr, ns) in &self.assertions {
            self.validate_expr(expr, ns, &mut Vec::new(), &mut done)?;
            if !self.eval(expr, ns, &HashMap::new(), 0)? {
                return Err(format!("static_assert failed: compile-time concept predicate is false in namespace '{ns}'"));
            }
        }
        Ok(())
    }
    fn validate_function_bounds(&self, items: &[Item], ns: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    self.validate_function_bounds(std::slice::from_ref(inner.as_ref()), ns)?
                }
                Item::Namespace { name, items } => {
                    self.validate_function_bounds(items, &qualified(ns, name))?
                }
                Item::Function(f) => self.validate_bounds(&f.concept_bounds, ns)?,
                Item::Impl(block) => {
                    for f in &block.methods {
                        self.validate_bounds(&f.concept_bounds, ns)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn validate_bounds(&self, bounds: &[ConceptBound], ns: &str) -> Result<(), String> {
        for bound in bounds {
            if let Some(arity) = intrinsic(&bound.name) {
                if bound.args.len() != arity {
                    return Err(format!(
                        "concept '{}' requires {arity} arguments",
                        bound.name
                    ));
                }
            } else {
                let (canonical, decl) = lookup(&self.entries, ns, &bound.name)
                    .ok_or_else(|| format!("unknown concept '{}' in where clause", bound.name))?;
                if decl.decl.generics.len() != bound.args.len() {
                    return Err(format!(
                        "concept '{canonical}' expects {} type arguments",
                        decl.decl.generics.len()
                    ));
                }
            }
        }
        Ok(())
    }
    fn parent_contains(
        &self,
        child: &str,
        parent: &str,
        ns: &str,
        visited: &mut HashSet<String>,
    ) -> bool {
        if child == parent || child.rsplit("::").next() == Some(parent) {
            return true;
        }
        let key = if self.trait_parents.contains_key(child) {
            child.to_string()
        } else if self.trait_parents.contains_key(&qualified(ns, child)) {
            qualified(ns, child)
        } else {
            child.into()
        };
        if !visited.insert(key.clone()) {
            return false;
        }
        self.trait_parents
            .get(&key)
            .map(|bases| {
                bases.iter().any(|base| {
                    let next = if base.contains("::") {
                        base.clone()
                    } else {
                        qualified(
                            key.rsplit_once("::").map(|(scope, _)| scope).unwrap_or(""),
                            base,
                        )
                    };
                    self.parent_contains(&next, parent, ns, visited)
                })
            })
            .unwrap_or(false)
    }
    /// Finds a field in a declared struct, substituting its type parameters.
    fn find_field(&self, ty: &TypeRef, name: &str, ns: &str) -> Option<TypeRef> {
        let (struct_name, args) = match ty {
            TypeRef::Named(name) => (name.as_str(), &[][..]),
            TypeRef::Applied(name, args) => (name.as_str(), args.as_slice()),
            _ => return None,
        };
        let (_, (decl, _)) = lookup(&self.structs, ns, struct_name)?;
        if decl.generics.len() != args.len() {
            return None;
        }
        let params = decl
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        decl.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| sub_type(&field.ty, &params))
    }
    fn find_method(&self, ty: &TypeRef, name: &str, ns: &str) -> Option<TypeRef> {
        for (target, methods, method_ns) in &self.methods {
            let matches_type = normalized(target) == normalized(ty)
                || matches!((target,ty),(TypeRef::Named(a),TypeRef::Named(b)) if qualified(method_ns,a)==*b);
            if !matches_type {
                continue;
            }
            if let Some(method) = methods
                .iter()
                .find(|method| method.name == name && method.params.len() == 1)
            {
                return Some(method.result.clone());
            }
        }
        // Avoid resolving unrelated impl methods through lexical type names.
        let _ = ns;
        None
    }
    fn check_intrinsic(&self, name: &str, args: &[TypeRef], ns: &str) -> Result<bool, String> {
        let first = &args[0];
        let ty = bare_name(first).unwrap_or("");
        let is_signed = matches!(ty, "i8" | "i16" | "i32" | "i64" | "isize");
        let is_unsigned = matches!(ty, "u8" | "u16" | "u32" | "u64" | "usize");
        let is_float = matches!(ty, "f32" | "f64");
        Ok(match name {
            "integer" => is_signed || is_unsigned,
            "signed" => is_signed,
            "unsigned" => is_unsigned,
            "floating" => is_float,
            "numeric" => is_signed || is_unsigned || is_float,
            "boolean" => ty == "bool",
            "pointer" => matches!(first, TypeRef::Pointer { .. }),
            "reference" => matches!(first, TypeRef::Reference { .. }),
            "same_as" => normalized(first) == normalized(&args[1]),
            "has_field" => {
                let field =
                    bare_name(&args[1]).ok_or("has_field<T,field> needs a field identifier")?;
                self.find_field(first, field, ns).is_some()
            }
            "has_method" => {
                let name =
                    bare_name(&args[1]).ok_or("has_method<T,method> needs a method identifier")?;
                self.find_method(first, name, ns).is_some()
            }
            "implements" => {
                let trait_name = bare_name(&args[1])
                    .ok_or_else(|| "implements<T,Trait> requires a named trait".to_string())?;
                if !self.trait_parents.contains_key(trait_name)
                    && lookup(&self.trait_parents, ns, trait_name).is_none()
                {
                    return Err(format!(
                        "implements<T,{trait_name}> references unknown trait"
                    ));
                }
                self.implementations
                    .iter()
                    .any(|(provided, for_type, source_ns)| {
                        normalized(for_type) == normalized(first)
                            && self.parent_contains(
                                provided,
                                trait_name,
                                source_ns,
                                &mut HashSet::new(),
                            )
                    })
            }
            _ => return Err(format!("unknown intrinsic concept '{name}'")),
        })
    }
    fn eval(
        &self,
        expr: &ConceptExpr,
        ns: &str,
        subst: &HashMap<String, TypeRef>,
        depth: usize,
    ) -> Result<bool, String> {
        if depth > 64 {
            return Err("concept recursion depth exceeded".into());
        }
        match expr {
            ConceptExpr::Literal(v) => Ok(*v),
            ConceptExpr::Requires { variables, rules } => {
                let locals = variables
                    .iter()
                    .map(|(name, ty)| (name.clone(), sub_type(ty, subst)))
                    .collect::<HashMap<_, _>>();
                for rule in rules {
                    let valid = match rule {
                        Requirement::Field {
                            variable,
                            field,
                            result,
                        } => {
                            let ty = locals
                                .get(variable)
                                .ok_or_else(|| format!("unknown requires variable '{variable}'"))?;
                            self.find_field(ty, field, ns)
                                .map(|found| {
                                    normalized(&found) == normalized(&sub_type(result, subst))
                                })
                                .unwrap_or(false)
                        }
                        Requirement::Method {
                            variable,
                            method,
                            result,
                        } => {
                            let ty = locals
                                .get(variable)
                                .ok_or_else(|| format!("unknown requires variable '{variable}'"))?;
                            self.find_method(ty, method, ns)
                                .map(|found| {
                                    normalized(&found) == normalized(&sub_type(result, subst))
                                })
                                .unwrap_or(false)
                        }
                        Requirement::Binary {
                            left,
                            operation,
                            right,
                            result,
                        } => {
                            let a = locals
                                .get(left)
                                .ok_or_else(|| format!("unknown requires operand '{left}'"))?;
                            let b = locals
                                .get(right)
                                .ok_or_else(|| format!("unknown requires operand '{right}'"))?;
                            let expected = sub_type(result, subst);
                            // The bootstrap arithmetic operators are defined for
                            // matching numeric operands; user-defined operator
                            // dispatch is checked separately during specialization.
                            normalized(a) == normalized(b)
                                && normalized(a) == normalized(&expected)
                                && self.check_intrinsic("numeric", std::slice::from_ref(a), ns)?
                                && matches!(
                                    operation,
                                    ast::BinaryOp::Add
                                        | ast::BinaryOp::Sub
                                        | ast::BinaryOp::Mul
                                        | ast::BinaryOp::Div
                                )
                        }
                    };
                    if !valid {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            ConceptExpr::Not(x) => Ok(!self.eval(x, ns, subst, depth + 1)?),
            ConceptExpr::And(a, b) => {
                Ok(self.eval(a, ns, subst, depth + 1)? && self.eval(b, ns, subst, depth + 1)?)
            }
            ConceptExpr::Or(a, b) => {
                Ok(self.eval(a, ns, subst, depth + 1)? || self.eval(b, ns, subst, depth + 1)?)
            }
            ConceptExpr::Apply { name, args } => {
                let concrete = args.iter().map(|t| sub_type(t, subst)).collect::<Vec<_>>();
                if intrinsic(name).is_some() {
                    return self.check_intrinsic(name, &concrete, ns);
                }
                let (canonical, entry) = lookup(&self.entries, ns, name)
                    .ok_or_else(|| format!("unknown concept '{name}'"))?;
                if concrete.len() != entry.decl.generics.len() {
                    return Err(format!("concept '{canonical}' argument count mismatch"));
                }
                let bindings = entry
                    .decl
                    .generics
                    .iter()
                    .cloned()
                    .zip(concrete)
                    .collect::<HashMap<_, _>>();
                self.eval(
                    &entry.decl.predicate,
                    &entry.namespace,
                    &bindings,
                    depth + 1,
                )
            }
        }
    }
    /// Validate constraints only after generic substitutions are concrete.
    /// Rejection happens before HIR/MIR emission and LLVM code generation.
    pub fn check(
        &self,
        bounds: &[ConceptBound],
        ns: &str,
        subst: &HashMap<String, TypeRef>,
        owner: &str,
    ) -> Result<(), String> {
        for bound in bounds {
            let concrete = bound
                .args
                .iter()
                .map(|a| sub_type(a, subst))
                .collect::<Vec<_>>();
            let expr = ConceptExpr::Apply {
                name: bound.name.clone(),
                args: concrete.clone(),
            };
            if !self.eval(&expr, ns, &HashMap::new(), 0)? {
                return Err(format!(
                    "concept constraint '{}' is not satisfied for {} in '{}'; required {}<{:?}>",
                    bound.name,
                    concrete
                        .iter()
                        .map(|t| format!("{t:?}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    owner,
                    bound.name,
                    concrete
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn registry(source: &str) -> Result<Registry, String> {
        let ast = grammar::ProgramParser::new()
            .parse(source)
            .map_err(|e| format!("{e:?}"))?;
        Registry::collect(&ast)
    }
    #[test]
    fn composed_concepts_and_failures() {
        let r=registry("concept Number<T> = integer<T> || floating<T>; concept Tiny<T> = Number<T> && !same_as<T, f64>;").unwrap();
        let constraint = vec![ConceptBound {
            name: "Tiny".into(),
            args: vec![TypeRef::Named("T".into())],
        }];
        assert!(r
            .check(
                &constraint,
                "",
                &HashMap::from([("T".into(), TypeRef::Named("i64".into()))]),
                "test"
            )
            .is_ok());
        assert!(r
            .check(
                &constraint,
                "",
                &HashMap::from([("T".into(), TypeRef::Named("f64".into()))]),
                "test"
            )
            .unwrap_err()
            .contains("not satisfied"));
    }
    #[test]
    fn concept_cycles_and_unknowns() {
        assert!(registry("concept A<T> = B<T>; concept B<T> = A<T>;")
            .unwrap_err()
            .contains("cyclic concept"));
        assert!(registry("concept A<T> = Missing<T>;")
            .unwrap_err()
            .contains("unknown concept"));
        assert!(registry("concept A<T> = same_as<T>;")
            .unwrap_err()
            .contains("expects 2"));
    }
    #[test]
    fn trait_requirements_are_checked() {
        let r=registry("trait Display { fn show(self:Self)->i32; } impl Display for i32 {fn show(self:i32)->i32{return self;}} concept Showable<T> = implements<T,Display>;").unwrap();
        let bound = vec![ConceptBound {
            name: "Showable".into(),
            args: vec![TypeRef::Named("i32".into())],
        }];
        assert!(r.check(&bound, "", &HashMap::new(), "test").is_ok());
        let not = vec![ConceptBound {
            name: "Showable".into(),
            args: vec![TypeRef::Named("f64".into())],
        }];
        assert!(r.check(&not, "", &HashMap::new(), "test").is_err());
    }
}
