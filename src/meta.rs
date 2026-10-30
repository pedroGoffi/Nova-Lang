//! Nova Meta: source-authored attribute, effect and language-protocol declarations.
//!
//! The compiler owns *verification*; Nova libraries own the vocabulary and
//! constraints through marker traits. This phase does not execute user code,
//! manufacture unsafe capabilities or permit arbitrary layout/ownership changes.
use crate::ast::{self, Attribute, Item, Program};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct AttributeSchema {
    pub targets: HashSet<String>,
    pub definition: String,
    pub lint_actions: HashSet<String>,
    pub min_args: usize,
    pub max_args: Option<usize>,
    pub min_fields: Option<usize>,
    pub max_fields: Option<usize>,
    pub required_fields: HashSet<String>,
    pub field_types: BTreeMap<String,String>,
    pub required_methods: HashSet<String>,
    pub requires: HashSet<String>,
    pub conflicts: HashSet<String>,
}

#[derive(Clone, Debug)]
struct DeriveImpl {
    origin: String,
    constraints: Vec<String>,
}

#[derive(Default, Debug)]
pub struct Registry {
    attributes: BTreeMap<String, AttributeSchema>,
    effects: BTreeMap<String, String>,
    protocols: BTreeMap<String, (String, String)>,
    derive_aliases: BTreeMap<String, (String, Vec<String>)>,
    /// Nova-defined traits whose default methods can be materialized via #[derive].
    derive_impls: BTreeMap<String, DeriveImpl>,
    reflection_primitives: BTreeMap<String,String>,
}

fn builtin_attribute(name: &str, target: &str) -> bool {
    match name {
        "derive" | "repr" | "capabilities" | "requires" => target == "struct" || target == "enum",
        "operator" => target == "trait",
        "meta_attribute" | "meta_effect" | "meta_protocol" | "meta_derive" | "meta_derive_impl" => target == "trait",
        "unsafe" | "pure" | "io" | "async" | "const" | "comptime" | "comptime_assert" | "meta_intrinsic" | "meta_proc_intrinsic" | "proc_derive" | "proc_attribute" | "proc_macro" => target == "function",
        "effects" => target == "function",
        _ => false,
    }
}

impl Registry {
    pub fn collect(program: &Program) -> Result<Self, String> {
        let mut registry = Self::default();
        registry.declarations(&program.items, "")?;
        registry.check_aliases()?;
        registry.verify(&program.items, "")?;
        Ok(registry)
    }

    fn declarations(&mut self, items: &[Item], ns: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => self.declarations(std::slice::from_ref(inner.as_ref()), ns)?,
                Item::Namespace {name, items} => self.declarations(items, &qual(ns, name))?,
                Item::Function(f) => {
                    for marker in &f.attributes {
                        if marker.name != "meta_intrinsic" {continue;}
                        let origin=qual(ns,&f.name);
                        if ns != "std::meta" || marker.args.len()!=1 ||
                            !matches!(marker.args[0].as_str(),"field_count"|"has_field"|"type_name"|"field_type"|"type_kind"|"variant_count"|"has_variant") {
                            return Err(format!("error[NOVA-META023]: invalid reflection primitive declaration '{origin}'"));
                        }
                        if self.reflection_primitives.insert(marker.args[0].clone(),origin.clone()).is_some() {
                            return Err(format!("error[NOVA-META023]: duplicate reflection primitive '{}'",marker.args[0]));
                        }
                    }
                }
                Item::Trait(t) => {
                    let origin = qual(ns, &t.name);
                    let mut kinds = 0;
                    for a in &t.attributes {
                        match a.name.as_str() {
                            "meta_attribute" => {
                                kinds += 1;
                                if a.args.is_empty() {return Err(format!("error[NOVA-META001]: attribute '{origin}' needs target kinds"));}
                                let mut targets = HashSet::new();
                                let mut lint_actions = HashSet::new();
                                let mut min_args = 0;
                                let mut max_args = Some(0);
                                let mut explicit_arity = false;
                                let mut min_fields = None;
                                let mut max_fields = None;
                                let mut required_fields = HashSet::new();
                                let mut field_types = BTreeMap::new();
                                let mut required_methods = HashSet::new();
                                let mut requires = HashSet::new();
                                let mut conflicts = HashSet::new();
                                let mut directives = HashSet::new();
                                for directive in &a.args {
                                    if !directives.insert(directive.as_str()) {
                                        return Err(format!("error[NOVA-META002]: repeated directive '{directive}' in '{origin}'"));
                                    }
                                    if ["lint_unused", "lint_call"].contains(&directive.as_str()) {
                                        lint_actions.insert(directive.clone());
                                    } else if ["struct", "enum", "trait", "function"].contains(&directive.as_str()) {
                                        targets.insert(directive.clone());
                                    } else if directive=="args_any" {
                                        if explicit_arity {return Err(format!("error[NOVA-META012]: conflicting argument contracts on '{origin}'"));}
                                        explicit_arity=true;min_args=0;max_args=None;
                                    } else if let Some(raw)=directive.strip_prefix("args_exact_") {
                                        if explicit_arity {return Err(format!("error[NOVA-META012]: conflicting argument contracts on '{origin}'"));}
                                        let count=parse_meta_number(raw,&origin,directive)?;
                                        explicit_arity=true;min_args=count;max_args=Some(count);
                                    } else if let Some(raw)=directive.strip_prefix("args_min_") {
                                        if explicit_arity {return Err(format!("error[NOVA-META012]: conflicting argument contracts on '{origin}'"));}
                                        explicit_arity=true;min_args=parse_meta_number(raw,&origin,directive)?;max_args=None;
                                    } else if let Some(raw)=directive.strip_prefix("args_max_") {
                                        if explicit_arity {return Err(format!("error[NOVA-META012]: conflicting argument contracts on '{origin}'"));}
                                        explicit_arity=true;min_args=0;max_args=Some(parse_meta_number(raw,&origin,directive)?);
                                    } else if let Some(raw)=directive.strip_prefix("fields_min_") {
                                        min_fields=Some(parse_meta_number(raw,&origin,directive)?);
                                    } else if let Some(raw)=directive.strip_prefix("fields_max_") {
                                        max_fields=Some(parse_meta_number(raw,&origin,directive)?);
                                    } else if let Some(raw)=directive.strip_prefix("type_") {
                                        let (field,ty)=raw.rsplit_once('_').ok_or_else(||format!("error[NOVA-META012]: '{directive}' must be type_FIELD_TYPE on '{origin}'"))?;
                                        validate_meta_ident(field,&origin,directive)?;
                                        validate_meta_ident(ty,&origin,directive)?;
                                        if field_types.insert(field.to_string(),ty.to_string()).is_some() {
                                            return Err(format!("error[NOVA-META012]: duplicate field type constraint for '{field}' in '{origin}'"));
                                        }
                                    } else if let Some(raw)=directive.strip_prefix("field_") {
                                        validate_meta_ident(raw,&origin,directive)?;
                                        required_fields.insert(raw.to_string());
                                    } else if let Some(raw)=directive.strip_prefix("method_") {
                                        validate_meta_ident(raw,&origin,directive)?;
                                        required_methods.insert(raw.to_string());
                                    } else if let Some(raw)=directive.strip_prefix("requires_") {
                                        validate_meta_ident(raw,&origin,directive)?;
                                        requires.insert(raw.to_string());
                                    } else if let Some(raw)=directive.strip_prefix("conflicts_") {
                                        validate_meta_ident(raw,&origin,directive)?;
                                        conflicts.insert(raw.to_string());
                                    } else {
                                        return Err(format!("error[NOVA-META002]: unknown meta directive '{directive}' in '{origin}'"));
                                    }
                                }
                                if (min_fields.is_some()||max_fields.is_some()||!required_fields.is_empty()||!field_types.is_empty()) && !targets.contains("struct") {
                                    return Err(format!("error[NOVA-META012]: field contracts require struct target on '{origin}'"));
                                }
                                if !required_methods.is_empty() && !targets.contains("trait") {
                                    return Err(format!("error[NOVA-META012]: method contracts require trait target on '{origin}'"));
                                }
                                if min_fields.zip(max_fields).is_some_and(|(a,b)|a>b) {
                                    return Err(format!("error[NOVA-META012]: contradictory field counts on '{origin}'"));
                                }
                                if requires.iter().any(|name|conflicts.contains(name)) {
                                    return Err(format!("error[NOVA-META012]: contradictory requires/conflicts contracts on '{origin}'"));
                                }
                                if targets.is_empty() {return Err(format!("error[NOVA-META002]: '{origin}' must declare at least one target"));}
                                if lint_actions.contains("lint_unused") && !targets.contains("function") && !targets.contains("struct") && !targets.contains("enum") {
                                    return Err(format!("error[NOVA-META002]: lint_unused requires a function or type target in '{origin}'"));
                                }
                                if lint_actions.contains("lint_call") && !targets.contains("function") {
                                    return Err(format!("error[NOVA-META002]: lint_call requires a function target in '{origin}'"));
                                }
                                if builtin_attribute(&t.name, "struct") || builtin_attribute(&t.name, "trait") || builtin_attribute(&t.name, "function") {
                                    return Err(format!("error[NOVA-META003]: '{origin}' attempts to replace a core attribute"));
                                }
                                if self.attributes.insert(t.name.clone(), AttributeSchema {targets, definition: origin.clone(), lint_actions, min_args, max_args, min_fields, max_fields, required_fields, field_types, required_methods, requires, conflicts}).is_some() {
                                    return Err(format!("error[NOVA-META003]: attribute '#[{}]' defined more than once",t.name));
                                }
                            }
                            "meta_derive" => {
                                kinds += 1;
                                if a.args.is_empty() {return Err(format!("error[NOVA-META016]: derive alias '{origin}' needs at least one derive"));}
                                if builtin_attribute(&t.name, "struct") || builtin_attribute(&t.name,"trait") || builtin_attribute(&t.name,"function") {
                                    return Err(format!("error[NOVA-META016]: derive alias '{origin}' shadows a compiler attribute"));
                                }
                                if self.derive_aliases.insert(origin.clone(),(origin.clone(),a.args.clone())).is_some() {
                                    return Err(format!("error[NOVA-META016]: duplicate derive alias '{}'",t.name));
                                }
                            }
                            "meta_derive_impl" => {
                                kinds += 1;
                                // Arguments are unary concept obligations. They
                                // are checked by the existing concept solver for
                                // each concrete (possibly generic) target.
                                if a.args.iter().any(|name| name.is_empty()) {
                                    return Err(format!("error[NOVA-META020]: invalid concept name on '{origin}'"));
                                }
                                if t.methods.is_empty() || t.methods.iter().any(|method| method.default_body.is_none()) {
                                    return Err(format!("error[NOVA-META020]: derive trait '{origin}' must declare at least one method and supply a default body for every method"));
                                }
                                if !t.associated_types.is_empty() || !t.parents.is_empty() || t.restriction.is_some() {
                                    return Err(format!("error[NOVA-META020]: derive trait '{origin}' cannot require associated types, parents or type restrictions"));
                                }
                                if self.derive_impls.insert(origin.clone(), DeriveImpl {origin:origin.clone(), constraints:a.args.clone()}).is_some() {
                                    return Err(format!("error[NOVA-META020]: duplicate derive implementation '{}', from '{origin}'", t.name));
                                }
                            }
                            "meta_effect" => {
                                kinds += 1;
                                if !a.args.is_empty() {return Err(format!("error[NOVA-META004]: #[meta_effect] on '{origin}' accepts no arguments"));}
                                if self.effects.insert(t.name.clone(),origin.clone()).is_some() {
                                    return Err(format!("error[NOVA-META004]: duplicate effect '{}'",t.name));
                                }
                            }
                            "meta_protocol" => {
                                kinds += 1;
                                if a.args.len()!=2 {return Err(format!("error[NOVA-META005]: #[meta_protocol] on '{origin}' expects (protocol_key, method_name)"));}
                                let method = &a.args[1];
                                if !t.methods.iter().any(|m|m.name==*method) {
                                    return Err(format!("error[NOVA-META005]: protocol '{}' requires method '{method}' in trait '{origin}'",a.args[0]));
                                }
                                if self.protocols.insert(a.args[0].clone(),(origin.clone(),method.clone())).is_some() {
                                    return Err(format!("error[NOVA-META005]: duplicate protocol key '{}'",a.args[0]));
                                }
                            }
                            _ => {}
                        }
                    }
                    if kinds>1 {return Err(format!("error[NOVA-META006]: '{origin}' declares multiple meta roles"));}
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn verify_attributes(&self, attrs: &[Attribute], target: &str, owner: &str, shape: Shape<'_>) -> Result<(), String> {
        let mut seen = HashSet::new();
        for a in attrs {
            if !seen.insert(a.name.as_str()) {
                // A type may have both `with Trait` and explicit capabilities
                // but duplicate declarations should always be rejected.
                return Err(format!("error[NOVA-META007]: duplicate {} attribute '#[{}]' on {owner}",a.name,a.name));
            }
            if builtin_attribute(&a.name,target) {
                if a.name=="effects" {
                    if a.args.is_empty() {return Err(format!("error[NOVA-META008]: #[effects] on {owner} needs at least one effect"));}
                    let mut unique=HashSet::new();
                    for effect in &a.args {
                        if !unique.insert(effect) {return Err(format!("error[NOVA-META008]: duplicate effect '{effect}' on {owner}"));}
                        if !self.effects.contains_key(effect) {
                            return Err(format!("error[NOVA-META008]: unknown effect '{effect}' on {owner}; declare a trait with #[meta_effect] in .nova"));
                        }
                    }
                }
                continue;
            }
            let Some(schema)=self.attributes.get(&a.name) else {
                return Err(format!("error[NOVA-META009]: unsupported function attribute '{}' on {owner}; declare #[meta_attribute(...)] trait {} in .nova",a.name,a.name));
            };
            if !schema.targets.contains(target) {
                return Err(format!("error[NOVA-META010]: '#[{}]' is not valid for {target} '{owner}'; declared in {}",a.name,schema.definition));
            }
            if a.args.len()<schema.min_args || schema.max_args.is_some_and(|max|a.args.len()>max) {
                return Err(format!("error[NOVA-META011]: '#[{}]' on {owner} takes {}..{} arguments; got {}",a.name,schema.min_args,schema.max_args.map(|n|n.to_string()).unwrap_or_else(||"any".to_string()),a.args.len()));
            }
            if let Shape::Fields(fields)=shape {
                if schema.min_fields.is_some_and(|n|fields.len()<n) || schema.max_fields.is_some_and(|n|fields.len()>n) {
                    return Err(format!("error[NOVA-META013]: '#[{}]' on {owner} requires {}..{} fields; got {}",a.name,schema.min_fields.unwrap_or(0),schema.max_fields.map(|n|n.to_string()).unwrap_or_else(||"any".to_string()),fields.len()));
                }
                for required in &schema.required_fields {
                    if !fields.iter().any(|f|f.name==*required) {
                        return Err(format!("error[NOVA-META014]: '#[{}]' requires field '{required}' on {owner}",a.name));
                    }
                }
                for (field,ty) in &schema.field_types {
                    let match_type=fields.iter().find(|f|f.name==*field).map(|f|matches!(&f.ty,ast::TypeRef::Named(actual) if actual==ty));
                    if match_type!=Some(true) {
                        return Err(format!("error[NOVA-META019]: '#[{}]' requires field '{field}: {ty}' on {owner}",a.name));
                    }
                }
            }
            if let Shape::Methods(methods)=shape {
                for required in &schema.required_methods {
                    if !methods.iter().any(|f|f.name==*required) {
                        return Err(format!("error[NOVA-META014]: '#[{}]' requires method '{required}' on {owner}",a.name));
                    }
                }
            }
            for required in &schema.requires {
                if !attrs.iter().any(|other|other.name==*required) {
                    return Err(format!("error[NOVA-META015]: '#[{}]' on {owner} requires '#[{required}]'",a.name));
                }
            }
            for forbidden in &schema.conflicts {
                if attrs.iter().any(|other|other.name==*forbidden) {
                    return Err(format!("error[NOVA-META015]: '#[{}]' on {owner} conflicts with '#[{forbidden}]'",a.name));
                }
            }
        }
        Ok(())
    }
    fn verify(&self, items: &[Item], ns: &str) -> Result<(),String> {
        for item in items {
            match item {
                Item::Public(inner) => self.verify(std::slice::from_ref(inner.as_ref()),ns)?,
                Item::Namespace{name,items} => self.verify(items,&qual(ns,name))?,
                Item::Struct(t) => self.verify_attributes(&t.attributes,"struct",&qual(ns,&t.name),Shape::Fields(&t.fields))?,
                Item::Enum(t) => self.verify_attributes(&t.attributes,"enum",&qual(ns,&t.name),Shape::None)?,
                Item::Trait(t) => self.verify_attributes(&t.attributes,"trait",&qual(ns,&t.name),Shape::Methods(&t.methods))?,
                Item::Function(f) => self.verify_attributes(&f.attributes,"function",&qual(ns,&f.name),Shape::None)?,
                Item::Extern(f) => self.verify_attributes(&f.attributes,"function",&qual(ns,&f.name),Shape::None)?,
                Item::Impl(block) => for f in &block.methods {
                    self.verify_attributes(&f.attributes,"function",&format!("method {}",f.name),Shape::None)?;
                },
                _ => {}
            }
        }
        Ok(())
    }
    fn check_aliases(&self)->Result<(),String> {
        for name in self.derive_aliases.keys() {
            if self.attributes.contains_key(name) || self.effects.contains_key(name) || self.derive_impls.contains_key(name) {
                return Err(format!("error[NOVA-META016]: derive alias '{name}' conflicts with an existing metadata role"));
            }
        }
        for (name,(origin,_)) in &self.derive_aliases {
            let mut trail=Vec::new();
            self.resolve_derive(name,origin.rsplit_once("::").map(|(scope,_)|scope).unwrap_or(""),&mut trail).map_err(|err|format!("{err} (declared in {origin})"))?;
        }
        for name in self.derive_impls.keys() {
            if self.attributes.contains_key(name) || self.effects.contains_key(name) || matches!(name.as_str(),"Clone"|"Default"|"Debug"|"Serialize"|"Deserialize"|"Json") {
                return Err(format!("error[NOVA-META020]: derive trait '{name}' conflicts with a reserved metadata or builtin derive name"));
            }
        }
        for (name,schema) in &self.attributes {
            for related in schema.requires.iter().chain(schema.conflicts.iter()) {
                if !self.attributes.contains_key(related) && !matches!(related.as_str(),"derive"|"requires"|"capabilities"|"repr") {
                    return Err(format!("error[NOVA-META015]: #[{name}] references undeclared attribute '#[{related}]'"));
                }
            }
        }
        Ok(())
    }
    /// Resolve derive declarations lexically, without a global-name fallback.
    /// This prevents a package-local recipe from accidentally shadowing the
    /// same short name in an unrelated namespace or dependency.
    fn derive_key(&self, name: &str, namespace: &str) -> Option<String> {
        if name.contains("::") {
            return (self.derive_aliases.contains_key(name) || self.derive_impls.contains_key(name))
                .then(||name.to_string());
        }
        let mut scope = namespace;
        loop {
            let candidate=qual(scope,name);
            if self.derive_aliases.contains_key(&candidate) || self.derive_impls.contains_key(&candidate) {
                return Some(candidate);
            }
            if scope.is_empty() {break;}
            scope=scope.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
        }
        None
    }
    fn resolve_derive(&self,name:&str,namespace:&str,trail:&mut Vec<String>)->Result<Vec<String>,String> {
        const BUILTIN: &[&str]=&["Serialize","Deserialize","Json","Debug","Clone","Default"];
        if BUILTIN.contains(&name) {return Ok(vec![name.to_string()]);}
        let key=self.derive_key(name,namespace)
            .ok_or_else(||format!("error[NOVA-META017]: unknown derive '{name}' in namespace '{namespace}'"))?;
        if self.derive_impls.contains_key(&key) {return Ok(vec![key]);}
        if trail.contains(&key) {
            return Err(format!("error[NOVA-META018]: recursive derive alias: {} -> {key}",trail.join(" -> ")));
        }
        let (_,subderives)=&self.derive_aliases[&key];
        let declared_scope=key.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
        trail.push(key.clone());
        let mut flattened=Vec::new();
        for child in subderives {
            for entry in self.resolve_derive(child,declared_scope,trail)? {
                if !flattened.contains(&entry){flattened.push(entry);}
            }
        }
        trail.pop();
        Ok(flattened)
    }
    /// Expand Nova-authored derive recipes to real AST impls. The normal trait
    /// checker injects default method bodies, verifies signatures, and rejects
    /// overlapping impls. No code strings, template substitution or LLVM hook.
    pub fn expand_derive(&self,program:&Program)->Result<Program,String>{
        fn derive_type(attributes: &mut Vec<Attribute>, target: ast::TypeRef,
                       generics: &[String], registry: &Registry, ns: &str) -> Result<Vec<Item>, String> {
            let Some(index) = attributes.iter().position(|a|a.name=="derive") else {return Ok(vec![]);};
            let names = attributes[index].args.clone();
            let mut flattened = Vec::new();
            for name in &names {
                for resolved in registry.resolve_derive(name,ns,&mut Vec::new())? {
                    if !flattened.contains(&resolved) { flattened.push(resolved); }
                }
            }
            let mut builtins = Vec::new();
            let mut generated = Vec::new();
            for name in flattened {
                if let Some(trait_name) = registry.derive_impls.get(&name) {
                    generated.push(Item::Impl(ast::Impl {
                        generics: generics.to_vec(),
                        trait_name: Some(trait_name.origin.clone()),
                        target: target.clone(),
                        associated_types: vec![],
                        methods: vec![],
                    }));
                } else { builtins.push(name); }
            }
            if builtins.is_empty() { attributes.remove(index); }
            else {attributes[index].args = builtins;}
            Ok(generated)
        }
        fn walk(items: &[Item], registry:&Registry, ns:&str)->Result<Vec<Item>,String>{
            let mut out = Vec::new();
            for item in items {
                match item {
                    Item::Public(inner) => {
                        for transformed in walk(std::slice::from_ref(inner.as_ref()),registry,ns)? {
                            // Generated impls are not visibility declarations.
                            match transformed {
                                Item::Impl(_) => out.push(transformed),
                                other => out.push(Item::Public(Box::new(other))),
                            }
                        }
                    }
                    Item::Namespace{name,items} => out.push(Item::Namespace {
                        name:name.clone(), items:walk(items,registry,&qual(ns,name))?
                    }),
                    Item::Struct(source) => {
                        let mut value = source.clone();
                        let target = if value.generics.is_empty() {
                            ast::TypeRef::Named(value.name.clone())
                        } else {
                            ast::TypeRef::Applied(value.name.clone(),value.generics.iter()
                                .map(|n|ast::TypeRef::Named(n.clone())).collect())
                        };
                        let generated = derive_type(&mut value.attributes,target,&value.generics,registry,ns)?;
                        out.push(Item::Struct(value));
                        out.extend(generated);
                    }
                    Item::Enum(source) => {
                        let mut value = source.clone();
                        // Enum auto-derive is intentionally limited to the
                        // builtin derive rules; Nova trait templates require
                        // struct semantics until enum methods are validated.
                        if let Some(attr) = value.attributes.iter_mut().find(|a|a.name=="derive") {
                            let mut result=Vec::new();
                            for name in &attr.args {
                                for resolved in registry.resolve_derive(name,ns,&mut Vec::new())? {
                                    if registry.derive_impls.contains_key(&resolved) {
                                        return Err(format!("error[NOVA-META021]: #[derive({resolved})] on enum '{}' is not supported",value.name));
                                    }
                                    if !result.contains(&resolved) {result.push(resolved);}
                                }
                            }
                            attr.args=result;
                        }
                        out.push(Item::Enum(value));
                    }
                    other => out.push(other.clone()),
                }
            }
            Ok(out)
        }
        Ok(Program { items:walk(&program.items,self,"")? })
    }
    /// Check optional unary concepts declared on #[meta_derive_impl(...)]
    /// with the ordinary concept solver, not a second metadata evaluator.
    /// This is called after the generated impls enter the AST and the concept
    /// registry knows all user types and implementations.
    pub fn check_derive_contracts(&self, program: &Program,
        concepts: &crate::concepts::Registry) -> Result<(),String> {
        fn visit(items:&[Item], ns:&str, meta:&Registry,
            concepts:&crate::concepts::Registry) -> Result<(),String> {
            for item in items {
                match item {
                    Item::Public(inner) => visit(std::slice::from_ref(inner.as_ref()),ns,meta,concepts)?,
                    Item::Namespace{name,items} => visit(items,&qual(ns,name),meta,concepts)?,
                    Item::Impl(block) => {
                        let Some(name)=&block.trait_name else {continue;};
                        let Some(key)=meta.derive_key(name,ns) else {continue;};
                        let Some(def)=meta.derive_impls.get(&key) else {continue;};
                        if def.constraints.is_empty() {continue;}
                        let target=match &block.target {
                            ast::TypeRef::Named(n) if !n.contains("::") => ast::TypeRef::Named(qual(ns,n)),
                            ast::TypeRef::Applied(n,args) if !n.contains("::") =>
                                ast::TypeRef::Applied(qual(ns,n),args.clone()),
                            other => other.clone(),
                        };
                        let bounds=def.constraints.iter().map(|name| ast::ConceptBound {
                            name:name.clone(), args:vec![target.clone()]
                        }).collect::<Vec<_>>();
                        let declared_ns=key.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
                        concepts.check(&bounds,declared_ns,&HashMap::new(),&format!("derive {key}"))
                            .map_err(|err|format!("error[NOVA-META022]: derive '{}' for {:?} violates its declared concept contract: {err}",key,target))?;
                    }
                    _=>{}
                }
            }
            Ok(())
        }
        for (key, definition) in &self.derive_impls {
            let scope=key.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
            let mut seen=HashSet::new();
            for concept in &definition.constraints {
                if !seen.insert(concept) {
                    return Err(format!("error[NOVA-META022]: duplicate required concept '{concept}' on '{key}'"));
                }
                concepts.require_unary_concept(concept,scope)
                    .map_err(|err|format!("error[NOVA-META022]: invalid derive contract on '{key}': {err}"))?;
            }
        }
        visit(&program.items,"",self,concepts)
    }
    pub fn protocol_method(&self, key:&str)->Option<&str>{
        self.protocols.get(key).map(|(_,method)|method.as_str())
    }
    pub fn describe(&self) -> String {
        let mut out=String::from("Nova Meta declarations (.nova):\n");
        for (name,schema) in &self.attributes {
            let mut targets=schema.targets.iter().map(String::as_str).collect::<Vec<_>>();
            targets.sort_unstable();
            let mut actions=schema.lint_actions.iter().map(String::as_str).collect::<Vec<_>>();
            actions.sort_unstable();
            out.push_str(&format!("  attribute #[{name}] -> {} [{}] args {}..{} fields {}..{} requires {:?} conflicts {:?} ({})\n",targets.join(","),actions.join(","),schema.min_args,schema.max_args.map(|v|v.to_string()).unwrap_or_else(||"any".into()),schema.min_fields.unwrap_or(0),schema.max_fields.map(|v|v.to_string()).unwrap_or_else(||"any".into()),schema.requires,schema.conflicts,schema.definition));
        }
        for (name,(origin,derives)) in &self.derive_aliases {out.push_str(&format!("  derive {name} -> {} ({origin})\n",derives.join(",")));}
        for (name,definition) in &self.derive_impls {out.push_str(&format!("  derive impl {name} -> impl {} for Self; concepts {:?} (source-defined default methods)\n",definition.origin,definition.constraints));}
        for (name,origin) in &self.effects {out.push_str(&format!("  effect {name} ({origin})\n"));}
        for (name,(origin,method)) in &self.protocols {out.push_str(&format!("  protocol {name} -> {method} ({origin})\n"));}
        for (kind,origin) in &self.reflection_primitives {out.push_str(&format!("  reflection primitive {kind} -> {origin}\n"));}
        out
    }
}
#[derive(Clone,Copy)]
enum Shape<'a> {
    None,
    Fields(&'a [ast::Field]),
    Methods(&'a [ast::TraitMethod]),
}
fn parse_meta_number(value:&str,origin:&str,directive:&str)->Result<usize,String>{
    value.parse::<usize>().map_err(|_|format!("error[NOVA-META012]: invalid number in '{directive}' on '{origin}'"))
}
fn validate_meta_ident(value:&str,origin:&str,directive:&str)->Result<(),String>{
    let mut chars=value.chars();
    if !matches!(chars.next(),Some(c) if c.is_ascii_alphabetic()||c=='_')||!chars.all(|c|c.is_ascii_alphanumeric()||c=='_') {
        return Err(format!("error[NOVA-META012]: invalid name in '{directive}' on '{origin}'"));
    }
    Ok(())
}
pub fn expand(program:&Program)->Result<Program,String>{Registry::collect(program)?.expand_derive(program)}
fn qual(ns:&str,name:&str)->String {if ns.is_empty(){name.into()}else{format!("{ns}::{name}")}}

pub fn validate(program:&Program)->Result<(),String>{Registry::collect(program).map(|_|())}
pub fn describe(program:&Program)->Result<String,String>{Ok(Registry::collect(program)?.describe())}
/// Explain the actual AST-level impls produced by .nova derive recipes.
/// This output is a development aid; generated impls still go through the
/// normal type_features and ownership checks before code generation.
pub fn expansion_report(program:&Program)->Result<String,String>{
    let registry=Registry::collect(program)?;
    let expanded=registry.expand_derive(program)?;
    let mut report=String::from("Nova Meta expanded implementations (traits + procedural macros):\n");
    fn visit(items:&[Item],namespace:&str,out:&mut String){
        for item in items {
            match item {
                Item::Public(inner)=>visit(std::slice::from_ref(inner.as_ref()),namespace,out),
                Item::Namespace{name,items}=>visit(items,&qual(namespace,name),out),
                Item::Impl(block)=>{
                    if let Some(trait_name)=&block.trait_name {
                        out.push_str(&format!("  impl {trait_name} for {:?} (scope: {})\n",block.target,if namespace.is_empty(){"<root>"}else{namespace}));
                    } else if !block.methods.is_empty() {
                        let names=block.methods.iter().map(|m|m.name.as_str()).collect::<Vec<_>>().join(", ");
                        out.push_str(&format!("  impl {:?} {{ {} }} (scope: {})\n",block.target,names,if namespace.is_empty(){"<root>"}else{namespace}));
                    }
                }
                _=>{}
            }
        }
    }
    visit(&expanded.items,"",&mut report);
    Ok(report)
}

/// Evaluate source-defined advisory markers. Unlike type-safety checks these
/// diagnostics do not change code generation or silently discard a value.
/// The initial implementation checks direct calls and named result types;
/// method dispatch and aliases will be handled once fully resolved call
/// metadata is exposed to the linter.
pub fn lint(program:&Program)->Vec<String>{
    let Ok(registry)=Registry::collect(program) else {return Vec::new();};
    let mut functions=HashMap::<String,(bool,bool,String)>::new();
    let mut types=HashSet::<String>::new();
    collect_lints(&program.items,"",&registry,&mut functions,&mut types);
    let mut warnings=Vec::new();
    apply_lints(&program.items,"",&functions,&types,&mut warnings);
    warnings
}
fn has_lint(attributes:&[Attribute],registry:&Registry,rule:&str)->bool{
    attributes.iter().any(|a|registry.attributes.get(&a.name)
        .is_some_and(|schema|schema.lint_actions.contains(rule)))
}
fn collect_lints(items:&[Item],ns:&str,registry:&Registry,functions:&mut HashMap<String,(bool,bool,String)>,types:&mut HashSet<String>){
    for item in items {
        match item {
            Item::Public(inner)=>collect_lints(std::slice::from_ref(inner.as_ref()),ns,registry,functions,types),
            Item::Namespace{name,items}=>collect_lints(items,&qual(ns,name),registry,functions,types),
            Item::Struct(t) if has_lint(&t.attributes,registry,"lint_unused")=>{types.insert(qual(ns,&t.name));},
            Item::Enum(t) if has_lint(&t.attributes,registry,"lint_unused")=>{types.insert(qual(ns,&t.name));},
            Item::Function(f)=>{
                let ty=match &f.result {ast::TypeRef::Named(t)=>t.clone(),_=>String::new()};
                functions.insert(qual(ns,&f.name),(has_lint(&f.attributes,registry,"lint_unused"),has_lint(&f.attributes,registry,"lint_call"),ty));
            }
            _=>{}
        }
    }
}
fn resolved<'a,T>(name:&str,ns:&str,map:&'a HashMap<String,T>)->Option<&'a T>{
    if name.contains("::") {return map.get(name);}
    let mut scope=ns;
    loop {
        if let Some(entry)=map.get(&qual(scope,name)){return Some(entry);}
        if scope.is_empty(){return None;}
        scope=scope.rsplit_once("::").map(|(s,_)|s).unwrap_or("");
    }
}
fn is_must_use_type(name:&str,ns:&str,types:&HashSet<String>)->bool{
    name.contains("::") && types.contains(name)
        || (0..=ns.split("::").count()).any(|i| {
            let prefix=ns.split("::").take(i).collect::<Vec<_>>().join("::");
            types.contains(&qual(&prefix,name))
        })
}
fn inspect_stmt(body:&[ast::Stmt],ns:&str,owner:&str,functions:&HashMap<String,(bool,bool,String)>,types:&HashSet<String>,warn:&mut Vec<String>){
    for s in body {
        match s {
            ast::Stmt::Expr(expr)=>{
                if let ast::Expr::StructInit(name,_)|ast::Expr::GenericStructInit(name,_,_)=expr {
                    if is_must_use_type(name,ns,types) {
                        warn.push(format!("warning[NOVA-META-L001]: {owner} discards a result marked lint_unused of type '{name}'"));
                    }
                }
                if let ast::Expr::Call(name,_)|ast::Expr::GenericCall(name,_,_)=expr {
                    if let Some((must_use,deprecated,result))=resolved(name,ns,functions) {
                        if *must_use || is_must_use_type(result,ns,types) {
                            warn.push(format!("warning[NOVA-META-L001]: {owner} discards a result marked lint_unused from '{name}'"));
                        }
                        if *deprecated {warn.push(format!("warning[NOVA-META-L002]: {owner} calls a function marked lint_call: '{name}'"));}
                    }
                }
            }
            ast::Stmt::If{then_body,else_body,..}=>{
                inspect_stmt(then_body,ns,owner,functions,types,warn);
                if let Some(other)=else_body {inspect_stmt(other,ns,owner,functions,types,warn);}
            }
            ast::Stmt::While{body,..}|ast::Stmt::Loop(body)|ast::Stmt::For{body,..}|
            ast::Stmt::ForEach{body,..}|ast::Stmt::Unsafe(body)=>
                inspect_stmt(body,ns,owner,functions,types,warn),
            ast::Stmt::Match{arms,..}=>for arm in arms {
                inspect_stmt(&arm.body,ns,owner,functions,types,warn);
            },
            _=>{}
        }
    }
}
fn apply_lints(items:&[Item],ns:&str,functions:&HashMap<String,(bool,bool,String)>,types:&HashSet<String>,warn:&mut Vec<String>){
    for item in items {
        match item {
            Item::Public(inner)=>apply_lints(std::slice::from_ref(inner.as_ref()),ns,functions,types,warn),
            Item::Namespace{name,items}=>apply_lints(items,&qual(ns,name),functions,types,warn),
            Item::Function(f)=>inspect_stmt(&f.body,ns,&qual(ns,&f.name),functions,types,warn),
            _=>{}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test(src:&str)->Result<(),String>{let p=crate::grammar::ProgramParser::new().parse(src).map_err(|e|format!("{e:?}"))?;validate(&p)}
    #[test]
    fn must_use_markers_produce_lints() {
        let source="#[meta_attribute(function, lint_unused)] trait must_use {} #[must_use] fn checked()->i32{return 42;} fn main()->i32{checked();return 0;}";
        let program=crate::grammar::ProgramParser::new().parse(source).unwrap();
        validate(&program).unwrap();
        assert_eq!(lint(&program).len(),1);
        assert!(lint(&program)[0].contains("NOVA-META-L001"));
    }
    #[test]
    fn arbitrary_nova_attribute_can_define_lint_policy() {
        let src="#[meta_attribute(function, lint_unused)] trait should_consume {} #[should_consume] fn build()->i32{return 1;} fn main()->i32{build();return 0;}";
        let ast=crate::grammar::ProgramParser::new().parse(src).unwrap();
        validate(&ast).unwrap();
        assert!(lint(&ast).iter().any(|x|x.contains("NOVA-META-L001")));
    }
    #[test]
    fn source_defined_attribute_checks_target() {
        test("#[meta_attribute(struct)] trait tracked {} #[tracked] struct Node { value:i32 }").unwrap();
        assert!(test("#[meta_attribute(struct)] trait tracked {} #[tracked] fn main()->i32{return 0;}").unwrap_err().contains("NOVA-META010"));
    }
    #[test]
    fn source_defined_effect_requires_declaration() {
        test("#[meta_effect] trait network {} #[effects(network)] fn fetch()->i32{return 1;}").unwrap();
        assert!(test("#[effects(unknown)] fn fetch()->i32{return 1;}").unwrap_err().contains("NOVA-META008"));
    }
    #[test]
    fn protocol_contract_checks_method() {
        test("#[meta_protocol(for_loop, into_iter)] trait IntoIterator {fn into_iter(self:Self)->Self;}").unwrap();
        assert!(test("#[meta_protocol(for_loop, into_iter)] trait IntoIterator {}").unwrap_err().contains("NOVA-META005"));
    }
    #[test]
    fn unknown_and_duplicate_markers_are_rejected() {
        assert!(test("#[undefined] struct A{x:i32}").unwrap_err().contains("NOVA-META009"));
        assert!(test("#[meta_attribute(struct)] trait marker {} #[marker] #[marker] struct A{x:i32}").unwrap_err().contains("NOVA-META007"));
    }
}

#[cfg(test)]
mod rich_meta_tests {
    use super::*;
    fn parse(src:&str)->Program {
        crate::grammar::ProgramParser::new().parse(src).expect("parse Nova Meta sample")
    }
    fn check(src:&str)->Result<(),String>{Registry::collect(&parse(src)).map(|_|())}

    #[test]
    fn attribute_argument_arity_and_metadata_shape() {
        let decl="#[meta_attribute(struct, args_exact_2, fields_min_1, fields_max_3, field_id, type_id_i64)] trait audited {} ";
        check(&format!("{decl} #[audited(prod, audit)] struct Item {{ id:i64 }}")).unwrap();
        assert!(check(&format!("{decl} #[audited(prod)] struct Item {{ id:i64 }}")).unwrap_err().contains("NOVA-META011"));
        assert!(check(&format!("{decl} #[audited(prod,audit)] struct Item {{ id:i32 }}")).unwrap_err().contains("NOVA-META019"));
        assert!(check(&format!("{decl} #[audited(prod,audit)] struct Item {{ name:i64 }}")).unwrap_err().contains("NOVA-META014"));
        assert!(check(&format!("{decl} #[audited(prod,audit)] struct Item {{ id:i64,a:i32,b:i32,c:i32 }}")).unwrap_err().contains("NOVA-META013"));
    }

    #[test]
    fn attribute_dependencies_and_conflicts() {
        let base="#[meta_attribute(struct)] trait audited {} #[meta_attribute(struct, requires_audited)] trait persistent {} #[meta_attribute(struct, conflicts_persistent)] trait transient {} ";
        check(&format!("{base} #[audited] #[persistent] struct Entity {{ id:i64 }}")).unwrap();
        assert!(check(&format!("{base} #[persistent] struct Entity {{ id:i64 }}")).unwrap_err().contains("NOVA-META015"));
        assert!(check(&format!("{base} #[audited] #[persistent] #[transient] struct Entity {{ id:i64 }}")).unwrap_err().contains("NOVA-META015"));
    }

    #[test]
    fn trait_method_contracts_and_bad_rule_names() {
        check("#[meta_attribute(trait, method_apply)] trait verified {} #[verified] trait Work { fn apply(self:Self)->Self; }").unwrap();
        assert!(check("#[meta_attribute(trait, method_apply)] trait verified {} #[verified] trait Work {}").unwrap_err().contains("NOVA-META014"));
        assert!(check("#[meta_attribute(struct, args_exact_nope)] trait invalid {}").unwrap_err().contains("NOVA-META012"));
        assert!(check("#[meta_attribute(struct, fields_min_3, fields_max_2)] trait invalid {}").unwrap_err().contains("NOVA-META012"));
    }

    #[test]
    fn derive_recipe_expands_transitively_without_duplicate_generated_methods() {
        let program=parse("#[meta_derive(Clone, Default)] trait BaseRecord {} #[meta_derive(BaseRecord, Debug)] trait UserRecord {} #[derive(UserRecord, Clone)] struct User {id:i64}");
        let expanded=expand(&program).unwrap();
        let Item::Struct(record)=expanded.items.last().unwrap() else {panic!("expected struct")};
        let attrs=record.attributes.iter().find(|a|a.name=="derive").unwrap();
        assert_eq!(attrs.args,vec!["Clone","Default","Debug"]);
    }

    #[test]
    fn derive_recipe_cycle_and_unknown_backend_derive_are_errors() {
        assert!(check("#[meta_derive(B)] trait A {} #[meta_derive(A)] trait B {}").unwrap_err().contains("NOVA-META018"));
        assert!(check("#[meta_derive(Magic)] trait A {}").unwrap_err().contains("NOVA-META017"));
    }

    #[test]
    fn nova_authored_derive_generates_real_trait_impl() {
        let source=r#"
            #[meta_derive_impl]
            trait HasValue {
                fn value(self: &Self) -> i64 {return self.id;}
            }
            #[meta_derive(HasValue, Clone)] trait Model {}
            #[derive(Model)] struct Entry {id:i64}
        "#;
        let ast=parse(source);
        let expanded=expand(&ast).unwrap();
        let entity=expanded.items.iter().find_map(|item| match item {
            Item::Struct(t) if t.name=="Entry"=>Some(t), _=>None
        }).unwrap();
        assert_eq!(entity.attributes.iter().find(|a|a.name=="derive").unwrap().args,vec!["Clone"]);
        let impls=expanded.items.iter().filter_map(|item| match item {
            Item::Impl(imp) if imp.trait_name.as_deref()==Some("HasValue")=>Some(imp), _=>None
        }).collect::<Vec<_>>();
        assert_eq!(impls.len(),1);
        assert_eq!(impls[0].target,ast::TypeRef::Named("Entry".into()));
        assert!(impls[0].methods.is_empty()); // populated by type_features trait defaults
    }

    #[test]
    fn derive_uses_lexical_namespace_and_rejects_missing_defaults() {
        let source=r#"
            namespace first {
                #[meta_derive_impl] trait HasValue {
                    fn value(self:Self)->i64 {return 1;}
                }
                #[derive(HasValue)] struct A {id:i64}
            }
            namespace second {
                #[meta_derive_impl] trait HasValue {
                    fn value(self:Self)->i64 {return 2;}
                }
                #[derive(HasValue)] struct B {id:i64}
            }
        "#;
        let ast=parse(source);
        let expanded=expand(&ast).unwrap();
        let keys=expanded.items.iter().filter_map(|item| match item {
            Item::Namespace {items,..}=>Some(items.iter().filter_map(|x| match x {
                Item::Impl(imp)=>imp.trait_name.clone(),_=>None
            }).collect::<Vec<_>>()),_=>None
        }).flatten().collect::<Vec<_>>();
        assert_eq!(keys,vec!["first::HasValue", "second::HasValue"]);
        assert!(check("#[meta_derive_impl] trait Invalid {fn doit(self:Self)->i32;}").unwrap_err().contains("NOVA-META020"));
        let program=parse("#[meta_derive_impl(Available)] trait Ok {fn doit(self:Self)->i32{return 1;}}");
        assert!(Registry::collect(&program).is_ok());
    }

    #[test]
    fn derive_contracts_use_real_nova_concepts() {
        let valid=r#"
            concept HasId<T> = has_field<T,id>;
            #[meta_derive_impl(HasId)] trait Identifiable {
                fn identifier(self:Self)->i64{return self.id;}
            }
            #[derive(Identifiable)] struct Entry {id:i64}
        "#;
        let expanded=expand(&parse(valid)).unwrap();
        let concepts=crate::concepts::Registry::collect(&expanded).unwrap();
        Registry::collect(&expanded).unwrap().check_derive_contracts(&expanded,&concepts).unwrap();
        let invalid=valid.replace("{id:i64}","{name:i64}");
        let expanded=expand(&parse(&invalid)).unwrap();
        let concepts=crate::concepts::Registry::collect(&expanded).unwrap();
        let err=Registry::collect(&expanded).unwrap().check_derive_contracts(&expanded,&concepts).unwrap_err();
        assert!(err.contains("NOVA-META022"),"{err}");
    }

    #[test]
    fn user_derive_rejects_cyclic_recipe_and_unknown_target() {
        assert!(check("#[meta_derive(Missing)] trait Wrapper {}").unwrap_err().contains("NOVA-META017"));
        let source=r#"#[meta_derive_impl] trait Id {fn id(self:Self)->i64{return self.value;}} #[derive(Id, Id)] struct S {value:i64}"#;
        let expanded=expand(&parse(source)).unwrap();
        assert_eq!(expanded.items.iter().filter(|i|matches!(i,Item::Impl(_))).count(),1);
    }

    #[test]
    fn names_come_from_nova_not_builtin_target_map() {
        let src="#[meta_attribute(struct, args_any, fields_min_1)] trait custom_flag {} #[custom_flag(alpha,beta,gamma)] struct S {x:i32}";
        let ast=parse(src);
        assert!(describe(&ast).unwrap().contains("attribute #[custom_flag]"));
        assert!(check(src).is_ok());
    }
}

/// Read-only AST reflection for tooling. No runtime reflection metadata and no
/// arbitrary compiler code execution are needed for this query.
pub fn inspect(program:&Program)->Result<String,String>{
    let registry=Registry::collect(program)?;
    let mut out=String::from("Nova Meta: type reflection (compile-time AST)\n");
    fn format_ty(ty:&ast::TypeRef)->String{
        match ty {
            ast::TypeRef::Named(n)=>n.clone(),
            ast::TypeRef::Applied(n,params)=>format!("{n}<{}>",params.iter().map(format_ty).collect::<Vec<_>>().join(", ")),
            ast::TypeRef::Array(inner,n)=>format!("[{}; {n}]",format_ty(inner)),
            ast::TypeRef::Pointer{mutable,inner}=>format!("*{} {}",if *mutable {"mut"}else{"const"},format_ty(inner)),
            ast::TypeRef::Reference{mutable,inner}=>format!("&{}{}",if *mutable {"mut "}else{""},format_ty(inner)),
            ast::TypeRef::Function{params,result}=>format!("fn({}) -> {}",params.iter().map(format_ty).collect::<Vec<_>>().join(", "),format_ty(result)),
        }
    }
    fn visit(items:&[Item],ns:&str,registry:&Registry,out:&mut String)->Result<(),String>{
        for item in items {
            match item {
                Item::Public(inner)=>visit(std::slice::from_ref(inner.as_ref()),ns,registry,out)?,
                Item::Namespace{name,items}=>visit(items,&qual(ns,name),registry,out)?,
                Item::Struct(s)=>{
                    out.push_str(&format!("struct {}{}\n",qual(ns,&s.name),if s.generics.is_empty(){String::new()}else{format!("<{}>",s.generics.join(", "))}));
                    for f in &s.fields {out.push_str(&format!("  field {}: {}\n",f.name,format_ty(&f.ty)));}
                    for a in &s.attributes {
                        if a.name=="derive" {
                            let mut entries=Vec::new();
                            for name in &a.args {
                                for found in registry.resolve_derive(name,ns,&mut Vec::new())? {if !entries.contains(&found){entries.push(found);}}
                            }
                            out.push_str(&format!("  derive {} => {}\n",a.args.join(", "),entries.join(", ")));
                        }else{out.push_str(&format!("  attribute #[{}({})]\n",a.name,a.args.join(", ")));}
                    }
                },
                Item::Enum(e)=>{
                    out.push_str(&format!("enum {}\n",qual(ns,&e.name)));
                    for v in &e.variants {out.push_str(&format!("  variant {}({})\n",v.name,v.payload.iter().map(format_ty).collect::<Vec<_>>().join(", ")));}
                },
                Item::Trait(t)=>{
                    out.push_str(&format!("trait {}\n",qual(ns,&t.name)));
                    for m in &t.methods {out.push_str(&format!("  method {} -> {}\n",m.name,format_ty(&m.result)));}
                },
                _=>{}
            }
        }
        Ok(())
    }
    visit(&program.items,"",&registry,&mut out)?;
    Ok(out)
}
