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
    // Monomorphization stores fully qualified types in their symbol form,
    // e.g. std__old__string__String. Concept checking must resolve that to the
    // original declared type; otherwise storable<String> fails even though
    // String is an owned struct containing only storable pointer fields.
    // Preserve lexical name lookup first to avoid hijacking an actual local
    // identifier containing double underscores.
    let mut current = ns;
    loop {
        if let Some(x) = definitions.get_key_value(&qualified(current, name)) {
            return Some(x);
        }
        if current.is_empty() {
            break;
        }
        current = current
            .rsplit_once("::")
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
    if name.contains("__") {
        return definitions.get_key_value(&name.replace("__", "::"));
    }
    None
}
fn intrinsic(name: &str) -> Option<usize> {
    match name {
        "integer" | "signed" | "unsigned" | "floating" | "numeric" | "boolean" | "pointer"
        | "reference" | "copyable" | "storable" | "sendable" | "syncable"
        | "default_constructible" | "destructible" => Some(1),
        "same_as" | "implements" | "has_field" | "has_method" | "has_static_method"
        | "has_associated_type" => Some(2),
        "associated_type_is" => Some(3),
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
    trait_associated: HashMap<String, Vec<String>>,
    /// Which trait method implements a token; declared by #[operator] in .nova.
    operator_methods: HashMap<String, Vec<(String, String)>>,
    implementations: Vec<(String, TypeRef, String)>,
    assertions: Vec<(ConceptExpr, String)>,
    structs: HashMap<String, (ast::Struct, String)>,
    enums: HashMap<String, (ast::Enum, String)>,
    aliases: HashMap<String, (TypeRef, String)>,
    methods: Vec<(TypeRef, Vec<ast::Function>, String)>,
    associated_bindings: Vec<(TypeRef, String, TypeRef, String)>,
}
impl Registry {
    pub fn collect(program: &ast::Program) -> Result<Self, String> {
        let mut registry = Self::default();
        registry.gather(&program.items, "")?;
        registry.validate_associated_bindings(program)?;
        registry.validate()?;
        registry.validate_capabilities()?;
        registry.validate_function_bounds(&program.items, "")?;
        Ok(registry)
    }
    fn validate_associated_bindings(&self, program: &ast::Program) -> Result<(), String> {
        fn walk(registry: &Registry, items: &[Item], ns: &str) -> Result<(), String> {
            for item in items {
                match item {
                    Item::Namespace {name,items} => walk(registry,items,&qualified(ns,name))?,
                    Item::Public(inner) => walk(registry,std::slice::from_ref(inner.as_ref()),ns)?,
                    Item::Impl(block) if block.trait_name.is_some() => {
                        let trait_name=block.trait_name.as_ref().unwrap();
                        let canonical=lookup(&registry.trait_associated,ns,trait_name);
                        let Some((full, declared))=canonical else {
                            // Existing semantic analysis reports unknown traits.
                            continue;
                        };
                        let mut seen=HashSet::new();
                        for (item,_) in &block.associated_types {
                            if !seen.insert(item) {
                                return Err(format!("duplicate associated type '{item}' in impl {full}"));
                            }
                            if !declared.contains(item) {
                                return Err(format!("unknown associated type '{item}' in impl {full}"));
                            }
                        }
                        for item in declared {
                            if !seen.contains(item) {
                                return Err(format!("missing associated type '{item}' in impl {full}"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        walk(self,&program.items,"")
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
                    let trait_name = qualified(ns, &decl.name);
                    self.trait_associated.insert(trait_name.clone(),decl.associated_types.clone());
                    if self.trait_parents.insert(trait_name.clone(), decl.parents.clone()).is_some() {
                        return Err(format!("duplicate trait '{trait_name}'"));
                    }
                    for attr in &decl.attributes {
                        if matches!(attr.name.as_str(), "meta_attribute" | "meta_effect" | "meta_protocol" | "meta_derive" | "meta_derive_impl") {
                            continue; // declarations verified by Nova Meta before concept solving
                        }
                        if attr.name != "operator" {
                            return Err(format!("unknown trait attribute '#[{}]' on '{trait_name}'",attr.name));
                        }
                        if attr.args.is_empty() {
                            return Err(format!("#[operator] on '{trait_name}' needs an operator name"));
                        }
                        for key in &attr.args {
                            if !ast::valid_operator_key(key) {
                                return Err(format!("unsupported operator '{key}' in '{trait_name}'"));
                            }
                            // The Nova definition controls the method name. The
                            // method matching each operator key is declarative.
                            let method_name = ast::operator_trait_method(decl,key).ok_or_else(||
                                format!("#[operator({key})] on '{trait_name}' needs an associated method"))?;
                            let method = decl.methods.iter().find(|m|m.name == method_name).unwrap();
                            if method.params.len() != 2 {
                                return Err(format!("operator '{key}' on '{trait_name}' needs two parameters"));
                            }
                            self.operator_methods.entry(key.clone()).or_default()
                                .push((trait_name.clone(),method.name.clone()));
                        }
                    }
                }
                Item::TypeAlias(alias) => {
                    self.aliases.insert(qualified(ns, &alias.name), (alias.target.clone(), ns.into()));
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
                Item::Enum(decl) => {
                    let key = qualified(ns, &decl.name);
                    if self.enums.insert(key.clone(), (decl.clone(), ns.into())).is_some() {
                        return Err(format!("duplicate enum '{key}'"));
                    }
                }
                Item::Impl(decl) => {
                    for (name, ty) in &decl.associated_types {
                        self.associated_bindings.push((decl.target.clone(), name.clone(), ty.clone(), ns.into()));
                    }
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
    /// Resolve only applicable .nova contracts; multiple traits may use `|`
    /// but one concrete type must have an unambiguous implementation.
    pub fn operator_contract<'a>(&'a self,key:&str,receiver:&TypeRef,ns:&str)
        -> Result<Option<(&'a str,&'a str)>,String>
    {
        let candidates = self.operator_methods.get(key).into_iter().flat_map(|items|items.iter())
            .filter(|(trait_name,_)|self.implements_trait(receiver,trait_name,ns))
            .collect::<Vec<_>>();
        if candidates.is_empty() {return Ok(None);}
        if candidates.len() == 1 {
            let (name,method) = candidates[0];
            return Ok(Some((name.as_str(),method.as_str())));
        }
        Err(format!("error[NOVA-OP004]: ambiguous operator '{key}' for {receiver:?}; implemented traits: {}",
            candidates.iter().map(|(t,_)|t.as_str()).collect::<Vec<_>>().join(", ")))
    }
    pub fn implements_trait(&self, ty: &TypeRef, trait_name: &str, namespace: &str) -> bool {
        self.implementations.iter().any(|(provided, target, impl_namespace)| {
            normalized(ty) == normalized(target) &&
            self.parent_contains(provided, trait_name, impl_namespace, &mut HashSet::new())
        }) || self.implementations.iter().any(|(provided, target, impl_namespace)| {
            // Check fully-qualified and lexical type names as the source
            // frequently writes `impl Trait for LocalType` inside namespaces.
            let declared = match target {
                TypeRef::Named(n) => TypeRef::Named(qualified(impl_namespace,n)),
                TypeRef::Applied(n,args) => TypeRef::Applied(qualified(impl_namespace,n),args.clone()),
                _ => target.clone(),
            };
            normalized(&declared) == normalized(ty) &&
            self.parent_contains(provided, trait_name, namespace, &mut HashSet::new())
        })
    }
    fn validate_capabilities(&self) -> Result<(), String> {
        for (type_name, (decl, namespace)) in &self.structs {
            let attrs = decl.attributes.iter().filter(|a| a.name == "capabilities")
                .collect::<Vec<_>>();
            if attrs.len() > 1 {
                return Err(format!("duplicate #[capabilities] on '{type_name}'"));
            }
            for attr in attrs {
                if attr.args.is_empty() {
                    return Err(format!("#[capabilities] on '{type_name}' cannot be empty"));
                }
                let mut unique = HashSet::new();
                for trait_name in &attr.args {
                    if !unique.insert(trait_name) {
                        return Err(format!("duplicate capability '{trait_name}' on '{type_name}'"));
                    }
                    if lookup(&self.trait_parents, namespace, trait_name).is_none() {
                        return Err(format!("unknown capability trait '{trait_name}' on '{type_name}'"));
                    }
                    let target = if decl.generics.is_empty() {
                        TypeRef::Named(decl.name.clone())
                    } else {
                        TypeRef::Applied(decl.name.clone(),decl.generics.iter().map(|x|TypeRef::Named(x.clone())).collect())
                    };
                    if !self.implements_trait(&target, trait_name, namespace) {
                        return Err(format!("error[NOVA-OP001]: '{type_name}' declares #[capabilities({trait_name})] but has no `impl {trait_name} for {type_name}`"));
                    }
                }
            }
            // #[requires(Concept)] is an executable compile-time contract
            // written on the Nova type. A unary concept is applied to Self.
            for attr in decl.attributes.iter().filter(|a|a.name == "requires") {
                if attr.args.is_empty() {
                    return Err(format!("#[requires] on '{type_name}' needs a concept"));
                }
                let ty = if decl.generics.is_empty() {
                    TypeRef::Named(decl.name.clone())
                } else {
                    TypeRef::Applied(decl.name.clone(),decl.generics.iter().map(|x|TypeRef::Named(x.clone())).collect())
                };
                for concept in &attr.args {
                    let (_, entry) = lookup(&self.entries,namespace,concept)
                        .ok_or_else(||format!("unknown #[requires] concept '{concept}' on '{type_name}'"))?;
                    if entry.decl.generics.len() != 1 {
                        return Err(format!("#[requires({concept})] on '{type_name}' needs a unary concept"));
                    }
                    if !self.eval(&ConceptExpr::Apply{name:concept.clone(),args:vec![ty.clone()]},namespace,&HashMap::new(),0)? {
                        return Err(format!("error[NOVA-OP005]: '{type_name}' does not satisfy #[requires({concept})]"));
                    }
                }
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
                        Requirement::AssociatedType { .. } => {},
                        Requirement::StaticMethod { .. } => {},
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
                Item::Struct(s) => self.validate_bounds(&s.concept_bounds, ns)?,
                Item::Enum(e) => self.validate_bounds(&e.concept_bounds, ns)?,
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
        self.find_method_signature(ty,name,&[],ns)
    }
    /// Structural concept constraint with argument and result types. The
    /// compiler evaluates this while monomorphizing; there is no runtime vtable.
    fn find_method_signature(&self, ty: &TypeRef, name: &str, args: &[TypeRef], ns: &str) -> Option<TypeRef> {
        for (target, methods, method_ns) in &self.methods {
            let mut bindings = HashMap::new();
            let matches_type = match (target,ty) {
                (TypeRef::Applied(a,variables),TypeRef::Applied(b,concrete))
                    if variables.len()==concrete.len() && (a==b || qualified(method_ns,a)==*b) => {
                    for (formal,actual) in variables.iter().zip(concrete) {
                        if let TypeRef::Named(v)=formal {bindings.insert(v.clone(),actual.clone());}
                    }
                    true
                }
                (TypeRef::Named(a),TypeRef::Named(b)) => a==b || qualified(method_ns,a)==*b
                    || normalized(target)==normalized(ty)
                    || normalized(&TypeRef::Named(qualified(method_ns,a)))==normalized(ty),
                _ => normalized(target)==normalized(ty),
            };
            if !matches_type {continue;}
            if let Some(method)=methods.iter().find(|method| {
                let is_sugar = method.name.strip_prefix("__nova_op__")
                    .and_then(|key|self.operator_methods.get(key))
                    .is_some_and(|bindings|bindings.iter().any(|(_,mapped)|mapped==name));
                (method.name==name || is_sugar)
                    && method.params.first().is_some_and(|p|p.name=="self")
                    && method.params.len()==args.len()+1
                    && method.params.iter().skip(1).zip(args).all(|(formal,actual)|
                        normalized(&sub_type(&formal.ty,&bindings))==normalized(actual))
            }) {
                return Some(sub_type(&method.result,&bindings));
            }
        }
        let _=ns;
        None
    }
    // Typedefs are transparent to compile-time concepts, including when
    // nested in pointers, arrays and generic type arguments. Never recurse
    // forever on cyclic aliases (the main type checker reports the cycle).
    fn expand_alias(&self, ty: &TypeRef, ns: &str, seen: &mut HashSet<String>) -> TypeRef {
        match ty {
            TypeRef::Named(name) => {
                if let Some((canonical,(target,alias_ns)))=lookup(&self.aliases,ns,name) {
                    if !seen.insert(canonical.clone()) { return ty.clone(); }
                    let concrete=self.expand_alias(target,alias_ns,seen);
                    seen.remove(canonical);
                    concrete
                } else { ty.clone() }
            }
            TypeRef::Applied(name,args) => TypeRef::Applied(name.clone(),args.iter()
                .map(|arg|self.expand_alias(arg,ns,seen)).collect()),
            TypeRef::Array(item,len) => TypeRef::Array(Box::new(self.expand_alias(item,ns,seen)),*len),
            TypeRef::Pointer{mutable,inner} => TypeRef::Pointer{mutable:*mutable,
                inner:Box::new(self.expand_alias(inner,ns,seen))},
            TypeRef::Reference{mutable,inner} => TypeRef::Reference{mutable:*mutable,
                inner:Box::new(self.expand_alias(inner,ns,seen))},
            TypeRef::Function{params,result} => TypeRef::Function{
                params:params.iter().map(|arg|self.expand_alias(arg,ns,seen)).collect(),
                result:Box::new(self.expand_alias(result,ns,seen))},
        }
    }
    fn associated_type(&self, owner: &TypeRef, name: &str, ns: &str) -> Option<TypeRef> {
        let mut matches = self.associated_bindings.iter().filter(|(target, item, _, item_ns)| {
            if item != name { return false; }
            if normalized(target) == normalized(owner) { return true; }
            let qualified_target = match target {
                TypeRef::Named(value) => TypeRef::Named(qualified(item_ns, value)),
                TypeRef::Applied(value, args) => TypeRef::Applied(qualified(item_ns, value), args.clone()),
                _ => target.clone(),
            };
            normalized(&qualified_target) == normalized(owner)
        });
        let first = matches.next()?;
        if matches.next().is_some() { return None; } // ambiguous implementations
        let _ = ns;
        Some(first.2.clone())
    }
    fn has_default_constructor(&self, ty: &TypeRef, ns: &str) -> bool {
        self.methods.iter().any(|(target,methods,method_ns)| {
            let qualified_target = match target {
                TypeRef::Named(name) => TypeRef::Named(qualified(method_ns,name)),
                TypeRef::Applied(name,args) => TypeRef::Applied(qualified(method_ns,name),args.clone()),
                _ => target.clone(),
            };
            if normalized(target) != normalized(ty) && normalized(&qualified_target) != normalized(ty) {
                return false;
            }
            methods.iter().any(|method| method.name == "new"
                && method.attributes.iter().any(|attr|attr.name == "static")
                && method.params.iter().all(|param|param.default.is_some())
                && (normalized(&method.result)==normalized(ty)
                    || method.result == TypeRef::Named("Self".into())))
        }) || self.is_copyable(ty,ns,&mut HashSet::new()) && self.structs.iter().any(|(type_name,(decl,_))|
            normalized(ty)==normalized(&TypeRef::Named(type_name.clone())) && decl.fields.is_empty())
    }
    fn has_static_method(&self, ty:&TypeRef, name:&str, _ns:&str)->bool {
        self.methods.iter().any(|(target,methods,method_ns)| {
            let same=normalized(target)==normalized(ty)
                || matches!((target,ty),(TypeRef::Named(a),TypeRef::Named(b)) if qualified(method_ns,a)==*b)
                || matches!((target,ty),(TypeRef::Applied(a,params),TypeRef::Applied(b,args))
                    if params.len()==args.len() && (a==b || qualified(method_ns,a)==*b));
            same && methods.iter().any(|m|m.name==name && m.attributes.iter().any(|a|a.name=="static"))
        })
    }
    fn find_static_signature(&self, owner:&TypeRef, method_name:&str,
        arguments:&[TypeRef], namespace:&str) -> Option<TypeRef> {
        let owner=self.expand_alias(owner,namespace,&mut HashSet::new());
        for (target,methods,method_ns) in &self.methods {
            let mut vars=HashMap::new();
            let matched=match (target,&owner) {
                (TypeRef::Applied(formal,parameters),TypeRef::Applied(actual,types))
                    if parameters.len()==types.len() &&
                        (formal==actual || qualified(method_ns,formal)==*actual ||
                         qualified(method_ns,formal).replace("::","__")==*actual) => {
                    for (parameter,ty) in parameters.iter().zip(types) {
                        if let TypeRef::Named(name)=parameter {
                            vars.insert(name.clone(),ty.clone());
                        }
                    }
                    true
                },
                (TypeRef::Named(formal),TypeRef::Named(actual)) =>
                    formal==actual || qualified(method_ns,formal)==*actual ||
                    // Concrete generic specializations may carry the compiler's
                    // mangled module identity (users__Record) instead of its
                    // source spelling (users::Record). Both denote one type.
                    qualified(method_ns,formal).replace("::","__")==*actual ||
                    normalized(target)==normalized(&owner),
                _ => normalized(target)==normalized(&owner),
            };
            if !matched {continue;}
            for method in methods {
                if method.name!=method_name ||
                    !method.attributes.iter().any(|a|a.name=="static") ||
                    method.params.len()!=arguments.len() {continue;}
                if !method.params.iter().zip(arguments).all(|(formal,actual)|
                    normalized(&sub_type(&formal.ty,&vars))==normalized(actual)) {continue;}
                let result=if method.result==TypeRef::Named("Self".into()) {
                    owner.clone()
                } else {
                    sub_type(&method.result,&vars)
                };
                return Some(result);
            }
        }
        None
    }

    fn binary_method_result(&self, ty: &TypeRef, method: &str, ns: &str) -> Option<TypeRef> {
        for (target,methods,method_ns) in &self.methods {
            let matches_type = normalized(target)==normalized(ty)
                || matches!((target,ty),(TypeRef::Named(a),TypeRef::Named(b)) if qualified(method_ns,a)==*b);
            if !matches_type {continue;}
            if let Some(found) = methods.iter().find(|m| {
                let alias = m.name.strip_prefix("__nova_op__")
                    .and_then(|key|self.operator_methods.get(key))
                    .map(|bound|bound.iter().any(|(_,name)|name==method))
                    .unwrap_or(false);
                (m.name==method || alias) && m.params.len()==2
            }) {return Some(found.result.clone());}
        }
        let _ = ns;
        None
    }
    /// `copyable<T>` is a compile-time property, never a runtime callback.
    /// A value is bytewise-copyable when it has no Drop implementation and all
    /// fields are copyable. Borrowed `str`/references cannot be put in an
    /// untracked owning allocation until the language has lifetime parameters.
    ///
    /// This intentionally does NOT equate 'has no drop' to 'safe to move out of
    /// arbitrary pointers'. `Vec<T>` still operates only on copyable values.
    fn is_copyable(&self, ty: &TypeRef, ns: &str, seen: &mut HashSet<String>) -> bool {
        match ty {
            TypeRef::Named(name) if matches!(name.as_str(),
                "i8"|"i16"|"i32"|"i64"|"isize"|"u8"|"u16"|"u32"|"u64"|"usize"|
                "f32"|"f64"|"bool"|"char") => true,
            TypeRef::Pointer { .. } | TypeRef::Function { .. } => true,
            TypeRef::Array(inner, count) => *count != 0 && self.is_copyable(inner,ns,seen),
            TypeRef::Reference { .. } => false,
            TypeRef::Named(_) | TypeRef::Applied(..) => {
                let (type_name,args) = match ty {
                    TypeRef::Named(name) => (name.as_str(), &[][..]),
                    TypeRef::Applied(name,args) => (name.as_str(), args.as_slice()),
                    _ => unreachable!(),
                };
                if let Some((alias_name,(target,alias_ns))) = lookup(&self.aliases,ns,type_name) {
                    // Typedefs are transparent and introduce no runtime value.
                    if !args.is_empty() || !seen.insert(alias_name.clone()) {return false;}
                    let result=self.is_copyable(target,alias_ns,seen);
                    seen.remove(alias_name);
                    return result;
                }
                if let Some((enum_name,(decl,decl_ns)))=lookup(&self.enums,ns,type_name) {
                    if decl.generics.len()!=args.len() {return false;}
                    let identity=format!("enum:{enum_name}<{args:?}>");
                    if !seen.insert(identity.clone()) {return false;}
                    let has_drop=self.methods.iter().any(|(target,methods,method_ns)| {
                        let n=match target {TypeRef::Named(n)|TypeRef::Applied(n,_)=>n,_=>return false};
                        let candidate=if n.contains("::") {n.clone()} else {qualified(method_ns,n)};
                        candidate==enum_name.as_str() && methods.iter().any(|m|m.name=="drop")
                    });
                    let map=decl.generics.iter().cloned().zip(args.iter().cloned())
                        .collect::<HashMap<_,_>>();
                    let allowed=!has_drop && decl.variants.iter().all(|variant|
                        variant.payload.iter().all(|field|
                            self.is_copyable(&sub_type(field,&map),decl_ns,seen)));
                    seen.remove(&identity);
                    return allowed;
                }
                let Some((canonical_type,(definition,decl_namespace))) = lookup(&self.structs,ns,type_name) else {
                    return false;
                };
                if definition.generics.len()!=args.len() {return false;}
                let identity=format!("{canonical_type}<{args:?}>");
                if !seen.insert(identity.clone()) {return false;}
                let has_destructor=self.methods.iter().any(|(target,methods,method_namespace)| {
                    let target_name=match target {
                        TypeRef::Named(name)|TypeRef::Applied(name,_)=>name,
                        _=>return false,
                    };
                    let canonical=if target_name.contains("::") {
                        target_name.clone()
                    } else {
                        qualified(method_namespace,target_name)
                    };
                    canonical == canonical_type.as_str() && methods.iter().any(|method|method.name=="drop")
                });
                let allowed=!has_destructor && !definition.is_class && {
                    let substitutions=definition.generics.iter().cloned()
                        .zip(args.iter().cloned()).collect::<HashMap<_,_>>();
                    let fields_copyable=definition.fields.iter().all(|field|
                        self.is_copyable(&sub_type(&field.ty,&substitutions),decl_namespace,seen));
                    let parent_copyable=definition.parent.as_ref().map(|parent|
                        self.is_copyable(&sub_type(parent,&substitutions),decl_namespace,seen)).unwrap_or(true);
                    fields_copyable && parent_copyable
                };
                seen.remove(&identity);
                allowed
            }
        }
    }
    /// `storable<T>` permits owned structs in raw Nova allocations. It rejects
    /// borrowed values until aggregate lifetime parameters are supported.
    /// Actual moves/destruction are still checked by HIR and must use unsafe
    /// raw-memory operations; it does not imply Copy.
    fn is_storable(&self, ty: &TypeRef, ns: &str, seen: &mut HashSet<String>) -> bool {
        match ty {
            TypeRef::Named(name) if matches!(name.as_str(),
                "i8"|"i16"|"i32"|"i64"|"isize"|"u8"|"u16"|"u32"|"u64"|"usize"|
                "f32"|"f64"|"bool"|"char") => true,
            TypeRef::Pointer { .. } | TypeRef::Function { .. } => true,
            TypeRef::Array(inner, n) => *n != 0 && self.is_storable(inner,ns,seen),
            TypeRef::Reference { .. } => false,
            TypeRef::Named(..) | TypeRef::Applied(..) => {
                let (name,args)=match ty {
                    TypeRef::Named(name)=>(name.as_str(),&[][..]),
                    TypeRef::Applied(name,args)=>(name.as_str(),args.as_slice()),
                    _=>unreachable!(),
                };
                if let Some((alias_name,(target,alias_ns)))=lookup(&self.aliases,ns,name) {
                    if !args.is_empty() || !seen.insert(alias_name.clone()) {return false;}
                    let result=self.is_storable(target,alias_ns,seen);
                    seen.remove(alias_name);
                    return result;
                }
                if let Some((enum_name,(decl,decl_ns)))=lookup(&self.enums,ns,name) {
                    if decl.generics.len()!=args.len() {return false;}
                    let id=format!("enum:{enum_name}<{args:?}>");
                    if !seen.insert(id.clone()) {return false;}
                    let map=decl.generics.iter().cloned().zip(args.iter().cloned())
                        .collect::<HashMap<_,_>>();
                    let valid=decl.variants.iter().all(|variant|
                        variant.payload.iter().all(|field|
                            self.is_storable(&sub_type(field,&map),decl_ns,seen)));
                    seen.remove(&id);
                    return valid;
                }
                let Some((qualified,(definition,decl_ns)))=lookup(&self.structs,ns,name) else {
                    return false;
                };
                if definition.generics.len()!=args.len() {return false;}
                let id=format!("{qualified}<{args:?}>");
                if !seen.insert(id.clone()) {return false;}
                let map=definition.generics.iter().cloned().zip(args.iter().cloned())
                    .collect::<HashMap<_,_>>();
                let valid=!definition.is_class &&
                    definition.fields.iter().all(|field|
                        self.is_storable(&sub_type(&field.ty,&map),decl_ns,seen)) &&
                    definition.parent.as_ref().map(|parent|
                        self.is_storable(&sub_type(parent,&map),decl_ns,seen)).unwrap_or(true);
                seen.remove(&id);
                valid
            }
        }
    }
    fn check_intrinsic(&self, name: &str, args: &[TypeRef], ns: &str) -> Result<bool, String> {
        let concrete: Vec<TypeRef> = args.iter().map(|t|self.expand_alias(t,ns,&mut HashSet::new())).collect();
        let args = concrete.as_slice();
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
            "copyable" => self.is_copyable(first, ns, &mut HashSet::new()),
            "storable" => self.is_storable(first, ns, &mut HashSet::new()),
            "sendable" => self.is_sendable(first, ns, &mut HashSet::new()),
            "syncable" => self.is_syncable(first, ns, &mut HashSet::new()),
            "destructible" => self.is_storable(first,ns,&mut HashSet::new()) ||
                self.is_copyable(first,ns,&mut HashSet::new()),
            "default_constructible" => self.has_default_constructor(first,ns),
            "reference" => matches!(first, TypeRef::Reference { .. }),
            "same_as" => normalized(first) == normalized(&args[1]),
            "has_field" => {
                let field =
                    bare_name(&args[1]).ok_or("has_field<T,field> needs a field identifier")?;
                self.find_field(first, field, ns).is_some()
            }
            "has_static_method" => {
                let method=bare_name(&args[1]).ok_or("has_static_method<T,name> requires a method name")?;
                self.has_static_method(first,method,ns)
            }
            "has_associated_type" => {
                let item=bare_name(&args[1]).ok_or("has_associated_type<T,Item> needs an identifier")?;
                self.associated_type(first,item,ns).is_some()
            }
            "associated_type_is" => {
                let item=bare_name(&args[1]).ok_or("associated_type_is<T,Item,U> needs an identifier")?;
                self.associated_type(first,item,ns).is_some_and(|actual|
                    normalized(&actual)==normalized(&args[2]))
            }
            "has_method" => {
                let name =
                    bare_name(&args[1]).ok_or("has_method<T,method> needs a method identifier")?;
                self.find_method(first, name, ns).is_some()
                    || self.binary_method_result(first, name, ns).is_some()
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
                self.implements_trait(first,trait_name,ns)
            }
            _ => return Err(format!("unknown intrinsic concept '{name}'")),
        })
    }

    /// A conservative cross-thread safety proof. Raw pointers and borrowed
    /// references are rejected until Nova has explicit lifetime and aliasing
    /// capabilities. Function values are safe only when they are the
    /// compiler's non-capturing function type.
    fn is_sendable(&self, ty: &TypeRef, ns: &str, seen: &mut HashSet<String>) -> bool {
        match ty {
            TypeRef::Named(name) if matches!(name.as_str(),
                "i8"|"i16"|"i32"|"i64"|"isize"|"u8"|"u16"|"u32"|"u64"|"usize"|
                "f32"|"f64"|"bool"|"char"|"()") => true,
            TypeRef::Function { params, result } =>
                params.iter().all(|param| self.is_sendable(param, ns, seen))
                    && self.is_sendable(result, ns, seen),
            TypeRef::Pointer { .. } | TypeRef::Reference { .. } => false,
            TypeRef::Array(inner, count) => *count != 0 && self.is_sendable(inner, ns, seen),
            TypeRef::Named(..) | TypeRef::Applied(..) => {
                let (name, args) = match ty {
                    TypeRef::Named(name) => (name.as_str(), &[][..]),
                    TypeRef::Applied(name, args) => (name.as_str(), args.as_slice()),
                    _ => unreachable!(),
                };
                if let Some((alias, (target, alias_ns))) = lookup(&self.aliases, ns, name) {
                    if !args.is_empty() || !seen.insert(alias.clone()) { return false; }
                    let result = self.is_sendable(target, alias_ns, seen);
                    seen.remove(alias);
                    return result;
                }
                if let Some((enum_name, (decl, decl_ns))) = lookup(&self.enums, ns, name) {
                    if decl.generics.len() != args.len() { return false; }
                    let id = format!("send:enum:{enum_name}<{args:?}>");
                    if !seen.insert(id.clone()) { return false; }
                    let substitutions = decl.generics.iter().cloned().zip(args.iter().cloned())
                        .collect::<HashMap<_, _>>();
                    let result = decl.variants.iter().all(|variant| variant.payload.iter()
                        .all(|field| self.is_sendable(&sub_type(field, &substitutions), decl_ns, seen)));
                    seen.remove(&id);
                    return result;
                }
                let Some((type_name, (decl, decl_ns))) = lookup(&self.structs, ns, name) else {
                    return false;
                };
                if decl.generics.len() != args.len() || decl.is_class { return false; }
                let id = format!("send:{type_name}<{args:?}>");
                if !seen.insert(id.clone()) { return false; }
                let substitutions = decl.generics.iter().cloned().zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                let result = decl.fields.iter().all(|field|
                    self.is_sendable(&sub_type(&field.ty, &substitutions), decl_ns, seen))
                    && decl.parent.as_ref().map(|parent|
                        self.is_sendable(&sub_type(parent, &substitutions), decl_ns, seen))
                        .unwrap_or(true);
                seen.remove(&id);
                result
            }
        }
    }

    fn is_syncable(&self, ty: &TypeRef, ns: &str, seen: &mut HashSet<String>) -> bool {
        // Sync and Send share the same conservative structural proof until
        // shared immutable references receive explicit lifetime bounds.
        self.is_sendable(ty, ns, seen)
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
                        Requirement::AssociatedType { owner, item } => {
                            let ty = subst.get(owner).cloned()
                                .or_else(||locals.get(owner).cloned())
                                .unwrap_or_else(|| TypeRef::Named(owner.clone()));
                            self.associated_type(&ty,item,ns).is_some()
                        }
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
                            arguments,
                            result,
                        } => {
                            let ty = locals
                                .get(variable)
                                .ok_or_else(|| format!("unknown requires variable '{variable}'"))?;
                            let args=arguments.iter().map(|arg|sub_type(arg,subst)).collect::<Vec<_>>();
                            self.find_method_signature(ty, method, &args, ns)
                                .map(|found| {
                                    normalized(&found) == normalized(&sub_type(result, subst))
                                })
                                .unwrap_or(false)
                        }
                        Requirement::StaticMethod {owner,method,arguments,result} => {
                            let ty=subst.get(owner).cloned()
                                .or_else(||locals.get(owner).cloned())
                                .unwrap_or_else(||TypeRef::Named(owner.clone()));
                            let args=arguments.iter().map(|t|sub_type(t,subst)).collect::<Vec<_>>();
                            self.find_static_signature(&ty,method,&args,ns)
                                .is_some_and(|found|normalized(&found)==normalized(&sub_type(result,subst)))
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
                            let builtin = normalized(a) == normalized(b)
                                && normalized(a) == normalized(&expected)
                                && self.check_intrinsic("numeric",std::slice::from_ref(a),ns)?
                                && matches!(operation,ast::BinaryOp::Add | ast::BinaryOp::Sub
                                    | ast::BinaryOp::Mul | ast::BinaryOp::Div);
                            let contract = ast::operator_key(*operation)
                                .map(|key|self.operator_contract(key,a,ns)).transpose()?.flatten();
                            let custom = contract.map(|(_,method)| {
                                self.binary_method_result(a,method,ns).map(|actual|
                                    normalized(&actual)==normalized(&expected)).unwrap_or(false)
                                    && normalized(a)==normalized(b)
                            }).unwrap_or(false);
                            builtin || custom
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
    /// Check a named unary concept without instantiating it. Nova Meta uses
    /// this to validate derive declarations even when nobody derives them.
    pub fn require_unary_concept(&self, name: &str, ns: &str) -> Result<(), String> {
        let (qualified, entry) = lookup(&self.entries, ns, name)
            .ok_or_else(||format!("unknown derive concept '{name}'"))?;
        if entry.decl.generics.len() != 1 {
            return Err(format!("derive concept '{qualified}' must accept exactly one type parameter"));
        }
        Ok(())
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
    #[test]
    fn json_decodable_matches_derived_static_method_across_module_names() {
        let registry=registry(r#"
            namespace std {
                namespace old {
                    namespace json {
                        struct JsonError {}
                        concept JsonDecodable<T> = requires(T obj) {
                            { T::from_json(str) } -> std::old::result::Result<T,std::old::json::JsonError>;
                        };
                    }
                }
            }
            namespace users {
                struct Record { id:i64 }
                impl Record {
                    static fn from_json(text:str)
                        -> std::old::result::Result<users::Record,std::old::json::JsonError> {
                        return 0;
                    }
                }
                struct Wrong { id:i64 }
                impl Wrong {
                    static fn from_json(text:str)
                        -> std::old::result::Result<users::Record,std::old::json::JsonError> {
                        return 0;
                    }
                }
            }
        "#).expect("cross-module JSON concept fixture must parse");
        let bound=|ty:&str|[ConceptBound {
            name:"std::old::json::JsonDecodable".into(),
            args:vec![TypeRef::Named(ty.into())],
        }];
        assert!(registry.check(&bound("users__Record"),"",&HashMap::new(),"json_parse").is_ok(),
            "mangled user DTO must match a qualified static from_json method");
        assert!(registry.check(&bound("users::Record"),"",&HashMap::new(),"json_parse").is_ok(),
            "source-qualified type and its mangled identity must agree");
        assert!(registry.check(&bound("users__Wrong"),"",&HashMap::new(),"json_parse").is_err(),
            "a codec that returns a different owner must still be rejected");
    }

    #[test]
    fn storable_owned_struct_resolves_mangled_type_identity() {
        let r=registry(r#"
            namespace std {
                namespace old {
                    namespace string {
                        struct String { data: *mut u8 }
                        impl String { fn drop(self: String) -> () { return; } }
                    }
                    namespace vec {
                        concept VectorElement<T> = storable<T>;
                    }
                }
            }
        "#).unwrap();
        let bounds=[ConceptBound {
            name: "std::old::vec::VectorElement".into(),
            args: vec![TypeRef::Named("std__old__string__String".into())],
        }];
        assert!(r.check(&bounds, "", &HashMap::new(), "new_vec").is_ok(),
            "the specialized std__old__string__String must satisfy storable<T>");
    }

    #[test]
    fn recursive_storage_concepts_respect_typedef_and_drop() {
        let r=registry(r#"
            typedef Token = i64;
            struct Plain { id: Token, active: bool }
            struct Nested<T> { item: T }
            struct Owner { id: Token }
            impl Owner { fn drop(self: Owner) -> () { return; } }
            struct Borrowed { text: str }
            enum Maybe<T> { Just(T), Empty }
            concept CanCopy<T> = copyable<T>;
            concept CanStore<T> = storable<T>;
        "#).unwrap();
        let check=|concept:&str, ty:TypeRef| {
            r.check(&[ConceptBound{name:concept.into(),args:vec![ty]}],
                "",&HashMap::new(),"recursive_storage_concepts").is_ok()
        };
        let named=|name:&str|TypeRef::Named(name.into());
        assert!(check("CanCopy",named("Plain")));
        assert!(check("CanStore",named("Plain")));
        assert!(check("CanStore",named("Owner")));
        assert!(!check("CanCopy",named("Owner")));
        assert!(check("CanStore",TypeRef::Applied("Nested".into(),vec![named("Owner")])));
        assert!(!check("CanCopy",TypeRef::Applied("Nested".into(),vec![named("Owner")])));
        assert!(!check("CanStore",named("Borrowed")));
        assert!(!check("CanStore",named("str")));
        assert!(check("CanCopy",TypeRef::Applied("Maybe".into(),vec![named("Plain")])));
        assert!(!check("CanCopy",TypeRef::Applied("Maybe".into(),vec![named("Owner")])));
        assert!(check("CanStore",TypeRef::Applied("Maybe".into(),vec![named("Owner")])));
        assert!(!check("CanCopy",TypeRef::Reference{mutable:false,inner:Box::new(named("Owner"))}));
        assert!(check("CanCopy",TypeRef::Array(Box::new(named("Plain")),3)));
        assert!(!check("CanStore",TypeRef::Array(Box::new(named("Plain")),0)));
    }

    #[test]
    fn send_and_sync_are_structural_and_reject_borrowed_state() {
        let r = registry(r#"
            struct Plain { value:i64 }
            struct Borrowed { value:&i64 }
            struct Raw { value:*mut i64 }
            concept CanSend<T> = sendable<T>;
            concept CanSync<T> = syncable<T>;
        "#).unwrap();
        let bound = |name: &str, ty: &str| vec![ConceptBound {
            name: name.into(),
            args: vec![TypeRef::Named(ty.into())],
        }];

        assert!(r.check(&bound("CanSend", "Plain"), "", &HashMap::new(), "spawn").is_ok());
        assert!(r.check(&bound("CanSync", "Plain"), "", &HashMap::new(), "spawn").is_ok());
        assert!(r.check(&bound("CanSend", "Borrowed"), "", &HashMap::new(), "spawn").is_err());
        assert!(r.check(&bound("CanSync", "Raw"), "", &HashMap::new(), "spawn").is_err());
        assert!(r.check(&bound("CanSend", "i64"), "", &HashMap::new(), "spawn").is_ok());
    }

}
