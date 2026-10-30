//! Compile-time type aliases and structural trait contracts.
//!
//! Aliases are transparent: they create no runtime type and are expanded
//! before monomorphization. Traits impose compile-time obligations on explicit
//! `impl Trait for Type` blocks; they do not use dynamic vtables yet.
use crate::ast::{self, Expr, Item, Stmt, TypeRef};
use std::collections::{HashMap, HashSet};

fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}::{name}")
    }
}
fn lexical<'a, T>(
    definitions: &'a HashMap<String, T>,
    namespace: &str,
    name: &str,
) -> Option<(&'a String, &'a T)> {
    if name.contains("::") {
        return definitions.get_key_value(name);
    }
    let mut scope = namespace;
    loop {
        let candidate = qualify(scope, name);
        if let Some(found) = definitions.get_key_value(&candidate) {
            return Some(found);
        }
        if scope.is_empty() {
            return None;
        }
        scope = scope
            .rsplit_once("::")
            .map(|(prefix, _)| prefix)
            .unwrap_or("");
    }
}
#[derive(Default)]
struct TypesAndTraits {
    aliases: HashMap<String, (TypeRef, String)>,
    traits: HashMap<String, (ast::Trait, String)>,
}
impl TypesAndTraits {
    fn gather(&mut self, items: &[Item], namespace: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    self.gather(std::slice::from_ref(inner.as_ref()), namespace)?
                }
                Item::Namespace { name, items } => self.gather(items, &qualify(namespace, name))?,
                Item::TypeAlias(alias) => {
                    let qualified_name = qualify(namespace, &alias.name);
                    if self
                        .aliases
                        .insert(
                            qualified_name.clone(),
                            (alias.target.clone(), namespace.into()),
                        )
                        .is_some()
                    {
                        return Err(format!("duplicate typedef '{qualified_name}'"));
                    }
                }
                Item::Trait(definition) => {
                    let qualified_name = qualify(namespace, &definition.name);
                    if self
                        .traits
                        .insert(
                            qualified_name.clone(),
                            (definition.clone(), namespace.into()),
                        )
                        .is_some()
                    {
                        return Err(format!("duplicate trait '{qualified_name}'"));
                    }
                }
                _ => {}
            }
        }
        for name in self.aliases.keys() {
            if self.traits.contains_key(name) {
                return Err(format!("'{name}' cannot be both a trait and typedef"));
            }
        }
        Ok(())
    }
    fn type_ref(
        &self,
        value: &TypeRef,
        namespace: &str,
        stack: &mut Vec<String>,
    ) -> Result<TypeRef, String> {
        match value {
            TypeRef::Named(name) => {
                if let Some((qualified_name, (target, alias_namespace))) =
                    lexical(&self.aliases, namespace, name)
                {
                    if stack.contains(qualified_name) {
                        return Err(format!(
                            "cyclic typedef: {} -> {qualified_name}",
                            stack.join(" -> ")
                        ));
                    }
                    stack.push(qualified_name.clone());
                    let expanded = self.type_ref(target, alias_namespace, stack);
                    stack.pop();
                    return expanded;
                }
                Ok(value.clone())
            }
            TypeRef::Applied(name, parameters) => {
                let mut arguments = Vec::new();
                for parameter in parameters {
                    arguments.push(self.type_ref(parameter, namespace, stack)?);
                }
                if lexical(&self.aliases, namespace, name).is_some() {
                    return Err(format!("typedef '{name}' does not accept type arguments"));
                }
                Ok(TypeRef::Applied(name.clone(), arguments))
            }
            TypeRef::Array(inner, length) => Ok(TypeRef::Array(
                Box::new(self.type_ref(inner, namespace, stack)?),
                *length,
            )),
            TypeRef::Pointer { mutable, inner } => Ok(TypeRef::Pointer {
                mutable: *mutable,
                inner: Box::new(self.type_ref(inner, namespace, stack)?),
            }),
            TypeRef::Reference { mutable, inner } => Ok(TypeRef::Reference {
                mutable: *mutable,
                inner: Box::new(self.type_ref(inner, namespace, stack)?),
            }),
            TypeRef::Function { params, result } => Ok(TypeRef::Function {
                params: params
                    .iter()
                    .map(|ty| self.type_ref(ty, namespace, stack))
                    .collect::<Result<_, _>>()?,
                result: Box::new(self.type_ref(result, namespace, stack)?),
            }),
        }
    }
    fn alias(&self, value: &mut TypeRef, namespace: &str) -> Result<(), String> {
        *value = self.type_ref(value, namespace, &mut Vec::new())?;
        Ok(())
    }
    fn expression(&self, expression: &mut Expr, namespace: &str) -> Result<(), String> {
        match expression {
            Expr::If { condition, then_body, else_body } => {
                self.expression(condition,namespace)?;
                self.statements(then_body,namespace)?;
                self.statements(else_body,namespace)?;
            }
            Expr::Block(body) => self.statements(body,namespace)?,
            Expr::Lambda {
                params,
                result,
                body,
            } => {
                for parameter in params {
                    self.alias(&mut parameter.ty, namespace)?;
                }
                self.alias(result, namespace)?;
                self.statements(body, namespace)?;
            }
            Expr::GenericCall(_, types, arguments) => {
                for value in types {
                    self.alias(value, namespace)?;
                }
                for argument in arguments {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::GenericStructInit(_, types, fields) => {
                for value in types {
                    self.alias(value, namespace)?;
                }
                for (_, value) in fields {
                    self.expression(value, namespace)?;
                }
            }
            Expr::StructInit(_, fields) => {
                for (_, value) in fields {
                    self.expression(value, namespace)?;
                }
            }
            Expr::Call(_, arguments) | Expr::MacroCall(_, arguments) | Expr::Array(arguments) => {
                for argument in arguments {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::MethodCall(receiver, _, arguments) => {
                self.expression(receiver, namespace)?;
                for argument in arguments {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::CallIndirect(callee, arguments) => {
                self.expression(callee, namespace)?;
                for argument in arguments {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::Binary(left, _, right) | Expr::Index(left, right) => {
                self.expression(left, namespace)?;
                self.expression(right, namespace)?;
            }
            Expr::Unary(_, value)
            | Expr::Deref(value)
            | Expr::Yield(value)
            | Expr::Await(value)
            | Expr::Spread(value)
            | Expr::Field(value, _)
            | Expr::AddressOf { value, .. } => self.expression(value, namespace)?,
            Expr::Cast(value, target) => {
                self.expression(value, namespace)?;
                self.alias(target, namespace)?;
            }
            Expr::Zero(target) => self.alias(target, namespace)?,
            _ => {}
        }
        Ok(())
    }
    fn statements(&self, statements: &mut [Stmt], namespace: &str) -> Result<(), String> {
        for statement in statements {
            match statement {
                Stmt::Let {
                    ty, init, defer, ..
                } => {
                    if let Some(value) = ty {
                        self.alias(value, namespace)?;
                    }
                    self.expression(init, namespace)?;
                    if let Some(value) = defer {
                        self.expression(value, namespace)?;
                    }
                }
                Stmt::Defer(value)
                | Stmt::Set { value, .. }
                | Stmt::FieldSet { value, .. }
                | Stmt::DerefSet { value, .. }
                | Stmt::CompoundSet { value, .. }
                | Stmt::Expr(value) | Stmt::Tail(value)
                | Stmt::Return(Some(value)) => self.expression(value, namespace)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.expression(condition, namespace)?;
                    self.statements(then_body, namespace)?;
                    if let Some(body) = else_body {
                        self.statements(body, namespace)?;
                    }
                }
                Stmt::For {
                    start, end, body, ..
                } => {
                    self.expression(start, namespace)?;
                    self.expression(end, namespace)?;
                    self.statements(body, namespace)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    self.expression(iterable, namespace)?;
                    self.statements(body, namespace)?;
                }
                Stmt::While { condition, body } => {
                    self.expression(condition, namespace)?;
                    self.statements(body, namespace)?;
                }
                Stmt::Loop(body) | Stmt::Unsafe(body) => self.statements(body, namespace)?,
                Stmt::Match { value, arms } => {
                    self.expression(value, namespace)?;
                    for arm in arms {
                        self.statements(&mut arm.body, namespace)?;
                    }
                }
                Stmt::Break | Stmt::Continue | Stmt::Return(None) => {}
            }
        }
        Ok(())
    }
    fn function(&self, function: &mut ast::Function, namespace: &str) -> Result<(), String> {
        for parameter in &mut function.params {
            self.alias(&mut parameter.ty, namespace)?;
        }
        self.alias(&mut function.result, namespace)?;
        for bound in &mut function.concept_bounds {
            for arg in &mut bound.args {
                self.alias(arg, namespace)?;
            }
        }
        self.statements(&mut function.body, namespace)
    }
    fn substitute_parameter(value: &TypeRef, type_parameter: &str, target: &TypeRef) -> TypeRef {
        match value {
            TypeRef::Named(name) if name == type_parameter || name == "Self" => target.clone(),
            TypeRef::Applied(name, params) => TypeRef::Applied(
                name.clone(),
                params
                    .iter()
                    .map(|ty| Self::substitute_parameter(ty, type_parameter, target))
                    .collect(),
            ),
            TypeRef::Array(inner, count) => TypeRef::Array(
                Box::new(Self::substitute_parameter(inner, type_parameter, target)),
                *count,
            ),
            TypeRef::Pointer { mutable, inner } => TypeRef::Pointer {
                mutable: *mutable,
                inner: Box::new(Self::substitute_parameter(inner, type_parameter, target)),
            },
            TypeRef::Reference { mutable, inner } => TypeRef::Reference {
                mutable: *mutable,
                inner: Box::new(Self::substitute_parameter(inner, type_parameter, target)),
            },
            TypeRef::Function { params, result } => TypeRef::Function {
                params: params
                    .iter()
                    .map(|ty| Self::substitute_parameter(ty, type_parameter, target))
                    .collect(),
                result: Box::new(Self::substitute_parameter(result, type_parameter, target)),
            },
            _ => value.clone(),
        }
    }
    // Resolve an associated type after substituting the concrete Self type.
    // This first implementation supports non-generic `type Item` only;
    // generic associated types/lifetimes need a separate type constructor.
    fn assoc_type(ty: &TypeRef, self_type: &TypeRef, bindings: &[(String, TypeRef)]) -> TypeRef {
        match ty {
            TypeRef::Named(name) if name == "Self" => self_type.clone(),
            TypeRef::Named(name) if name.starts_with("Self::") => {
                let key = &name[6..];
                bindings
                    .iter()
                    .find(|(n, _)| n == key)
                    .map(|(_, value)| Self::substitute_parameter(value, "Self", self_type))
                    .unwrap_or_else(|| ty.clone())
            }
            TypeRef::Named(_) => ty.clone(),
            TypeRef::Applied(name, args) => TypeRef::Applied(
                name.clone(),
                args.iter()
                    .map(|a| Self::assoc_type(a, self_type, bindings))
                    .collect(),
            ),
            TypeRef::Array(t, n) => {
                TypeRef::Array(Box::new(Self::assoc_type(t, self_type, bindings)), *n)
            }
            TypeRef::Pointer { mutable, inner } => TypeRef::Pointer {
                mutable: *mutable,
                inner: Box::new(Self::assoc_type(inner, self_type, bindings)),
            },
            TypeRef::Reference { mutable, inner } => TypeRef::Reference {
                mutable: *mutable,
                inner: Box::new(Self::assoc_type(inner, self_type, bindings)),
            },
            TypeRef::Function { params, result } => TypeRef::Function {
                params: params
                    .iter()
                    .map(|p| Self::assoc_type(p, self_type, bindings))
                    .collect(),
                result: Box::new(Self::assoc_type(result, self_type, bindings)),
            },
        }
    }
    fn check_trait(
        &self,
        name: &str,
        namespace: &str,
        target: &TypeRef,
        methods: &[ast::Function],
        bindings: &[(String, TypeRef)],
        chain: &mut Vec<String>,
    ) -> Result<(), String> {
        let (qualified_name, (definition, trait_namespace)) =
            lexical(&self.traits, namespace, name)
                .ok_or_else(|| format!("unknown trait '{name}'"))?;
        if chain.contains(qualified_name) {
            return Err(format!(
                "cyclic trait inheritance: {} -> {qualified_name}",
                chain.join(" -> ")
            ));
        }
        chain.push(qualified_name.clone());
        let result = (|| -> Result<(), String> {
            let type_parameter = definition
                .restriction
                .as_ref()
                .map(|restriction| restriction.type_parameter.as_str())
                .unwrap_or("Self");
            if let Some(restriction) = &definition.restriction {
                if restriction.permitted_types.is_empty() {
                    return Err(format!("trait '{name}' has an empty permitted type set"));
                }
                let mut allowed = Vec::new();
                for ty in &restriction.permitted_types {
                    allowed.push(self.type_ref(ty, trait_namespace, &mut Vec::new())?);
                }
                if !allowed.contains(target) {
                    return Err(format!("trait '{name}' cannot be implemented for {target:?}; permitted types: {allowed:?}"));
                }
            }
            for expected in &definition.associated_types {
                if !bindings.iter().any(|(name, _)| name == expected) {
                    return Err(format!(
                        "impl {name} for {target:?} is missing associated type '{expected}'"
                    ));
                }
            }
            let mut seen = HashSet::new();
            for requirement in &definition.methods {
                if !seen.insert(&requirement.name) {
                    return Err(format!(
                        "trait '{name}' declares duplicate method '{}'",
                        requirement.name
                    ));
                }
                let implementation = methods
                    .iter()
                    .find(|method| method.name == requirement.name)
                    .ok_or_else(|| {
                        format!(
                            "impl {name} for {target:?} is missing method '{}'",
                            requirement.name
                        )
                    })?;
                let expected_params = requirement
                    .params
                    .iter()
                    .map(|parameter| {
                        let ty = Self::substitute_parameter(&parameter.ty, type_parameter, target);
                        let ty = Self::assoc_type(&ty, target, bindings);
                        self.type_ref(&ty, trait_namespace, &mut Vec::new())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let result =
                    Self::substitute_parameter(&requirement.result, type_parameter, target);
                let expected_result = self.type_ref(
                    &Self::assoc_type(&result, target, bindings),
                    trait_namespace,
                    &mut Vec::new(),
                )?;
                let actual_params = implementation
                    .params
                    .iter()
                    .map(|param| param.ty.clone())
                    .collect::<Vec<_>>();
                if expected_params != actual_params
                    || expected_result != implementation.result
                    || implementation.variadic
                    || !implementation.generics.is_empty()
                {
                    return Err(format!("method '{}::{}' signature mismatch: expected ({expected_params:?}) -> {expected_result:?}, found ({actual_params:?}) -> {:?}",name,requirement.name,implementation.result));
                }
            }
            for parent in &definition.parents {
                self.check_trait(parent, trait_namespace, target, methods, bindings, chain)?;
            }
            Ok(())
        })();
        chain.pop();
        result
    }
    // Collect default methods from the transitive trait inheritance graph.
    // A derived trait cannot silently override inherited defaults: the impl
    // must provide its own method to resolve an inherited-name collision.
    fn inject_defaults(
        &self,
        trait_name: &str,
        namespace: &str,
        target: &TypeRef,
        methods: &mut Vec<ast::Function>,
        bindings: &[(String, TypeRef)],
        visited: &mut Vec<String>,
        explicit_methods: &HashSet<String>,
    ) -> Result<(), String> {
        let (resolved, (definition, trait_namespace)) =
            lexical(&self.traits, namespace, trait_name)
                .ok_or_else(|| format!("unknown trait '{trait_name}'"))?;
        if visited.contains(resolved) {
            return Err(format!(
                "cyclic trait inheritance while resolving defaults of '{resolved}'"
            ));
        }
        visited.push(resolved.clone());
        for parent in &definition.parents {
            self.inject_defaults(
                parent,
                trait_namespace,
                target,
                methods,
                bindings,
                visited,
                explicit_methods,
            )?;
        }
        let type_parameter = definition
            .restriction
            .as_ref()
            .map(|restriction| restriction.type_parameter.as_str())
            .unwrap_or("Self");
        for required in &definition.methods {
            let Some(body) = &required.default_body else {
                continue;
            };
            if methods.iter().any(|method| method.name == required.name) {
                if !explicit_methods.contains(&required.name) {
                    return Err(format!("conflicting inherited default for method '{}'; override it explicitly in the impl",required.name));
                }
                continue;
            }
            let mut expanded_body = body.clone();
            self.statements(&mut expanded_body, trait_namespace)?;
            let expanded_params = required
                .params
                .iter()
                .map(|parameter| {
                    let expanded =
                        Self::substitute_parameter(&parameter.ty, type_parameter, target);
                    let expanded = Self::assoc_type(&expanded, target, bindings);
                    Ok(ast::Param {
                        name: parameter.name.clone(),
                        ty: self.type_ref(&expanded, trait_namespace, &mut Vec::new())?,
                        default: None,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let result_type = Self::substitute_parameter(&required.result, type_parameter, target);
            let result_type = Self::assoc_type(&result_type, target, bindings);
            methods.push(ast::Function {
                mode: ast::FunctionMode::Normal,
                name: required.name.clone(),
                generics: vec![],
                concept_bounds: vec![],
                params: expanded_params,
                variadic: false,
                result: self.type_ref(&result_type, trait_namespace, &mut Vec::new())?,
                body: expanded_body,
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            });
        }
        visited.pop();
        Ok(())
    }

    fn validate_impl_coherence(&self, items: &[Item], namespace: &str, seen: &mut HashSet<String>) -> Result<(),String> {
        for item in items {
            match item {
                Item::Public(x) => self.validate_impl_coherence(std::slice::from_ref(x.as_ref()),namespace,seen)?,
                Item::Namespace{name,items} => self.validate_impl_coherence(items,&qualify(namespace,name),seen)?,
                Item::Impl(x) => if let Some(name) = &x.trait_name {
                    let (resolved,_) = lexical(&self.traits,namespace,name).ok_or_else(||format!("unknown trait '{name}'"))?;
                    let key = format!("{resolved} for {namespace}::{:?}",x.target);
                    if !seen.insert(key.clone()) {return Err(format!("error[NOVA-OP003]: overlapping impl {key}"));}
                },
                _ => {}
            }
        }
        Ok(())
    }
    fn validate_trait_graph(
        &self,
        name: &str,
        visited: &mut Vec<String>,
        complete: &mut HashSet<String>,
    ) -> Result<(), String> {
        if complete.contains(name) {
            return Ok(());
        }
        if visited.iter().any(|entry| entry == name) {
            return Err(format!(
                "cyclic trait inheritance: {} -> {name}",
                visited.join(" -> ")
            ));
        }
        let (definition, namespace) = self
            .traits
            .get(name)
            .ok_or_else(|| format!("unknown trait '{name}'"))?;
        let mut operator_keys = HashSet::new();
        for attribute in &definition.attributes {
            if matches!(attribute.name.as_str(), "meta_attribute" | "meta_effect" | "meta_protocol" | "meta_derive" | "meta_derive_impl") {
                continue; // Nova Meta validated these source declarations.
            }
            if attribute.name != "operator" || attribute.args.is_empty() {
                return Err(format!("unknown or empty trait attribute on '{name}'"));
            }
            for key in &attribute.args {
                if !ast::valid_operator_key(key) || !operator_keys.insert(key) {
                    return Err(format!("invalid or duplicated operator '{key}' declared on trait '{name}'"));
                }
                if ast::operator_trait_method(definition,key).is_none() {
                    return Err(format!("operator '{key}' on trait '{name}' has no corresponding method"));
                }
            }
        }
        let mut declared = HashSet::new();
        for method in &definition.methods {
            if !declared.insert(&method.name) {
                return Err(format!(
                    "trait '{name}' declares duplicate method '{}'",
                    method.name
                ));
            }
        }
        let mut associated = HashSet::new();
        for item in &definition.associated_types {
            if !associated.insert(item) {
                return Err(format!(
                    "trait '{name}' duplicates associated type '{item}'"
                ));
            }
        }
        visited.push(name.into());
        for parent in &definition.parents {
            let (parent_name, _) = lexical(&self.traits, namespace, parent)
                .ok_or_else(|| format!("trait '{name}' extends unknown trait '{parent}'"))?;
            self.validate_trait_graph(parent_name, visited, complete)?;
        }
        visited.pop();
        complete.insert(name.into());
        Ok(())
    }
    fn items(&self, items: &[Item], namespace: &str) -> Result<Vec<Item>, String> {
        let mut result = Vec::new();
        for item in items {
            match item {
                Item::TypeAlias(_) | Item::Trait(_) | Item::Concept(_) | Item::ConceptAssert(_) => {
                }
                Item::Public(inner) => {
                    for transformed in
                        self.items(std::slice::from_ref(inner.as_ref()), namespace)?
                    {
                        result.push(Item::Public(Box::new(transformed)));
                    }
                }
                Item::Namespace { name, items } => result.push(Item::Namespace {
                    name: name.clone(),
                    items: self.items(items, &qualify(namespace, name))?,
                }),
                Item::Struct(value) => {
                    let mut concrete = value.clone();
                    for field in &mut concrete.fields {
                        self.alias(&mut field.ty, namespace)?;
                    }
                    if let Some(parent) = &mut concrete.parent {
                        self.alias(parent, namespace)?;
                    }
                    result.push(Item::Struct(concrete));
                }
                Item::Enum(value) => {
                    let mut concrete = value.clone();
                    for variant in &mut concrete.variants {
                        for ty in &mut variant.payload {
                            self.alias(ty, namespace)?;
                        }
                    }
                    result.push(Item::Enum(concrete));
                }
                Item::Function(function) => {
                    let mut transformed = function.clone();
                    self.function(&mut transformed, namespace)?;
                    result.push(Item::Function(transformed));
                }
                Item::Impl(block) => {
                    let mut transformed = block.clone();
                    self.alias(&mut transformed.target, namespace)?;
                    if transformed.trait_name.is_none() && !transformed.associated_types.is_empty()
                    {
                        return Err("associated types are only valid in trait impl blocks".into());
                    }
                    let mut seen_associated = HashSet::new();
                    for (name, ty) in &mut transformed.associated_types {
                        if !seen_associated.insert(name.clone()) {
                            return Err(format!("duplicate associated type '{name}' in impl"));
                        }
                        self.alias(ty, namespace)?;
                    }
                    // `operator |(...)` is only an alias for the normal Nova
                    // trait method named in the corresponding #[operator(bit_or)].
                    // No separate runtime/operator implementation is created.
                    for method in &mut transformed.methods {
                        if let Some(key) = method.name.strip_prefix("__nova_op__") {
                            let trait_name = block.trait_name.as_ref().ok_or_else(||
                                format!("operator '{key}' requires `impl Trait for Type`"))?;
                            let (_, (definition, _)) = lexical(&self.traits, namespace, trait_name)
                                .ok_or_else(||format!("unknown operator trait '{trait_name}'"))?;
                            method.name = ast::operator_trait_method(definition,key)
                                .ok_or_else(||format!("trait '{trait_name}' does not bind #[operator({key})]"))?
                                .to_string();
                        }
                    }
                    let bindings = transformed.associated_types.clone();
                    for method in &mut transformed.methods {
                        self.function(method, namespace)?;
                        for parameter in &mut method.params {
                            parameter.ty = Self::substitute_parameter(
                                &parameter.ty,
                                "Self",
                                &transformed.target,
                            );
                            parameter.ty =
                                Self::assoc_type(&parameter.ty, &transformed.target, &bindings);
                        }
                        method.result =
                            Self::substitute_parameter(&method.result, "Self", &transformed.target);
                        method.result =
                            Self::assoc_type(&method.result, &transformed.target, &bindings);
                    }
                    if let Some(name) = &block.trait_name {
                        let explicit_methods = transformed
                            .methods
                            .iter()
                            .map(|method| method.name.clone())
                            .collect::<HashSet<_>>();
                        self.inject_defaults(
                            name,
                            namespace,
                            &transformed.target,
                            &mut transformed.methods,
                            &bindings,
                            &mut Vec::new(),
                            &explicit_methods,
                        )?;
                        self.check_trait(
                            name,
                            namespace,
                            &transformed.target,
                            &transformed.methods,
                            &bindings,
                            &mut Vec::new(),
                        )?;
                    }
                    transformed.trait_name = None;
                    result.push(Item::Impl(transformed));
                }
                Item::Extern(external) => {
                    let mut transformed = external.clone();
                    for parameter in &mut transformed.params {
                        self.alias(&mut parameter.ty, namespace)?;
                    }
                    self.alias(&mut transformed.result, namespace)?;
                    result.push(Item::Extern(transformed));
                }
                _ => result.push(item.clone()),
            }
        }
        Ok(result)
    }
}

pub fn lower(program: &ast::Program) -> Result<ast::Program, String> {
    let mut analyzer = TypesAndTraits::default();
    analyzer.gather(&program.items, "")?;
    // Check all aliases, including aliases not referenced by another declaration.
    for (name, (target, namespace)) in &analyzer.aliases {
        let mut stack = vec![name.clone()];
        analyzer.type_ref(target, namespace, &mut stack)?;
    }
    analyzer.validate_impl_coherence(&program.items,"",&mut HashSet::new())?;
    let mut completed = HashSet::new();
    for trait_name in analyzer.traits.keys() {
        analyzer.validate_trait_graph(trait_name, &mut Vec::new(), &mut completed)?;
    }
    Ok(ast::Program {
        items: analyzer.items(&program.items, "")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn compile(source: &str) -> Result<ast::Program, String> {
        let program = grammar::ProgramParser::new()
            .parse(source)
            .map_err(|err| format!("{err:?}"))?;
        lower(&program)
    }
    #[test]
    fn aliases_and_trait_inheritance_lower_to_concrete_methods() {
        let source = r#"
            typedef Integer = i32;
            trait Scalar { fn double(self: Self)->Self; }
            trait<T in i8, i16, i32, i64> IntegerMath extends Scalar {
                fn add(self: T, other: T)->T;
            }
            impl IntegerMath for Integer {
                fn double(self: Integer)->Integer {return self+self;}
                fn add(self: Integer, other: Integer)->Integer{return self+other;}
            }
            fn main()->Integer {return 42;}
        "#;
        let lowered = compile(source).unwrap();
        assert!(lowered
            .items
            .iter()
            .all(|item| !matches!(item, Item::Trait(_) | Item::TypeAlias(_))));
        let Item::Impl(block) = &lowered.items[0] else {
            panic!("expected impl after lowering")
        };
        assert_eq!(block.target, TypeRef::Named("i32".into()));
    }
    #[test]
    fn trait_rejects_ineligible_types_and_missing_methods() {
        let allowed = "trait<T in i8,i16,i32,i64> Integer {fn checked(self:T)->T;}";
        assert!(compile(&format!(
            "{allowed} impl Integer for f32 {{fn checked(self:f32)->f32 {{return self;}}}}"
        ))
        .unwrap_err()
        .contains("permitted types"));
        assert!(compile(&format!(
            "{allowed} impl Integer for i32 {{fn other(self:i32)->i32 {{return self;}}}}"
        ))
        .unwrap_err()
        .contains("missing method"));
    }
    #[test]
    fn cyclic_alias_and_cyclic_traits_fail() {
        assert!(compile("typedef A = B; typedef B = A;")
            .unwrap_err()
            .contains("cyclic typedef"));
        assert!(compile(
            "trait First extends Second {} trait Second extends First {} impl First for i32 {}"
        )
        .unwrap_err()
        .contains("cyclic trait"));
    }
    #[test]
    fn trait_default_methods_are_injected_into_concrete_impls() {
        let program = compile(
            r#"
            trait Base {
                fn double(self: Self)->Self {return self+self;}
            }
            trait Numeric extends Base {
                fn triple(self: Self)->Self;
            }
            impl Numeric for i32 {
                fn triple(self:i32)->i32 {return self+self+self;}
            }
            fn main()->i32 {return 0;}
        "#,
        )
        .unwrap();
        let block = program
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(block) => Some(block),
                _ => None,
            })
            .unwrap();
        assert!(block.methods.iter().any(|method| method.name == "double"));
        assert!(block.methods.iter().any(|method| method.name == "triple"));
    }
    #[test]
    fn concrete_method_overrides_a_trait_default() {
        let program = compile(
            r#"
            trait Number {fn value(self:Self)->i32{return 10;}}
            impl Number for i32 {fn value(self:i32)->i32{return 20;}}
        "#,
        )
        .unwrap();
        let block = program
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(block) => Some(block),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            block
                .methods
                .iter()
                .filter(|method| method.name == "value")
                .count(),
            1
        );
    }
}
