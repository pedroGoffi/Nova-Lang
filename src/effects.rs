//! Compile-time effect contracts for Nova declarations.
//!
//! `[[pure]]` is a verified restricted effect: calls must target other `pure`
//! Nova functions and it forbids extern calls, unsafe blocks and suspension.
//! Effects are explicit capabilities. Calls are checked against the caller's
//! contract, inferred effects propagate through ordinary functions, and
//! unresolved method/function-pointer calls are rejected at strict boundaries.
//! `[[unsafe]]` is enforced by HIR at call sites and is also propagated here as
//! a real effect.
use crate::ast::{self, Attribute, Expr, Function, FunctionMode, Item, Stmt};
use std::collections::{HashMap, HashSet};

fn qual(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.into()
    } else {
        format!("{ns}::{name}")
    }
}
fn lookup<'a>(
    map: &'a HashMap<String, Vec<Attribute>>,
    ns: &str,
    name: &str,
) -> Option<&'a Vec<Attribute>> {
    if name.contains("::") {
        return map.get(name);
    }
    let mut here = ns;
    loop {
        if let Some(entry) = map.get(&qual(here, name)) {
            return Some(entry);
        }
        if here.is_empty() {
            return None;
        }
        here = here
            .rsplit_once("::")
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
}
fn has(attrs: &[Attribute], effect: &str) -> bool {
    attrs.iter().any(|a| a.name == effect)
}
fn same_contract(left: &[Attribute], right: &[Attribute]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.name == b.name && a.args == b.args)
}
fn attrs(attrs: &[Attribute], context: &str, mode: Option<&FunctionMode>) -> Result<(), String> {
    ast::function_is_unsafe(attrs, context)?;
    let mut seen = HashSet::new();
    for a in attrs {
        if !seen.insert(a.name.as_str()) {
            return Err(format!("duplicate effect '{}' on {context}", a.name));
        }
        // This pass validates effect contracts, not arbitrary Nova-authored
        // metadata. `meta::validate` already checks the target and argument
        // count of attributes such as `@GetMapping("/hello")` / `@Route(x)`.
        // Only the built-in argument-free effect markers belong to this rule.
        if !a.args.is_empty()
            && matches!(
                a.name.as_str(),
                "pure" | "io" | "unsafe" | "async" | "const"
                    | "comptime" | "comptime_assert" | "static"
            )
        {
            return Err(format!(
                "effect '{}' on {context} expects no arguments",
                a.name
            ));
        }
    }
    if (has(attrs, "pure") || has(attrs, "const") || has(attrs, "comptime"))
        && (has(attrs, "io") || has(attrs, "unsafe") || has(attrs, "async"))
    {
        return Err(format!(
            "{context}: [[pure]] cannot be combined with [[io]], [[unsafe]] or [[async]]"
        ));
    }
    if (has(attrs, "const") || has(attrs, "comptime")) && !matches!(mode, Some(FunctionMode::Normal)) {
        return Err(format!("{context}: const functions must be synchronous"));
    }
    if has(attrs, "async") && !matches!(mode, Some(FunctionMode::Async)) {
        return Err(format!(
            "{context}: [[async]] requires an async fn declaration"
        ));
    }
    Ok(())
}
#[derive(Default)]
struct Effects {
    signatures: HashMap<String, Vec<Attribute>>,
    /// Method contracts are indexed by short name until the type checker
    /// supplies the receiver's concrete impl. A name is retained only when it
    /// has one unambiguous contract; overloaded names remain unknown.
    methods: HashMap<String, Vec<Attribute>>,
    ambiguous_methods: HashSet<String>,
    definitions: Vec<(String, String, Function)>,
}
impl Effects {
    fn collect(&mut self, items: &[Item], namespace: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    self.collect(std::slice::from_ref(inner.as_ref()), namespace)?
                }
                Item::Namespace { name, items } => self.collect(items, &qual(namespace, name))?,
                Item::Function(f) => {
                    let id = qual(namespace, &f.name);
                    attrs(&f.attributes, &format!("function '{id}'"), Some(&f.mode))?;
                    // The declaration mode is itself an effect contract. Keep
                    // it in the signature table so callers can see that an
                    // `async fn` requires `async` even when the author did
                    // not repeat it as an attribute.
                    let mut signature = f.attributes.clone();
                    if matches!(f.mode, FunctionMode::Async)
                        && !signature.iter().any(|a| a.name == "async")
                    {
                        signature.push(Attribute { name: "async".into(), args: vec![] });
                    }
                    if self
                        .signatures
                        .insert(id.clone(), signature)
                        .is_some()
                    {
                        return Err(format!("duplicate effect signature for '{id}'"));
                    }
                    self.definitions.push((id, namespace.into(), f.clone()));
                }
                Item::Extern(f) => {
                    let id = qual(namespace, &f.name);
                    attrs(&f.attributes, &format!("extern '{id}'"), None)?;
                    if has(&f.attributes, "pure") {
                        return Err(format!(
                            "extern '{id}' cannot claim [[pure]] without a verified implementation"
                        ));
                    }
                    self.signatures.insert(id, f.attributes.clone());
                }
                Item::Impl(block) => {
                    for f in &block.methods {
                        attrs(
                            &f.attributes,
                            &format!("method '{}'", f.name),
                            Some(&f.mode),
                        )?;
                        if self.ambiguous_methods.contains(&f.name) {
                            continue;
                        }
                        if let Some(previous) = self.methods.get(&f.name) {
                            if !same_contract(previous, &f.attributes) {
                                self.methods.remove(&f.name);
                                self.ambiguous_methods.insert(f.name.clone());
                            }
                        } else {
                            self.methods.insert(f.name.clone(), f.attributes.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn call(&self, name: &str, ns: &str, owner: &str) -> Result<(), String> {
        let attrs = lookup(&self.signatures, ns, name).ok_or_else(|| {
            format!("[[pure]] function '{owner}' calls unresolved or effect-unknown '{name}'")
        })?;
        if !(has(attrs, "pure") || has(attrs, "const") || has(attrs, "comptime")) {
            return Err(format!("[[pure]] function '{owner}' calls impure '{name}'; annotate its verified implementation [[pure]]"));
        }
        if self
            .signatures
            .get(owner)
            .is_some_and(|owner_attrs| has(owner_attrs, "const"))
            && !has(attrs, "const")
        {
            return Err(format!("const fn '{owner}' cannot call non-const '{name}'"));
        }
        Ok(())
    }
    fn expr(&self, expr: &Expr, ns: &str, owner: &str) -> Result<(), String> {
        match expr {
            Expr::If { condition, then_body, else_body } => {
                self.expr(condition,ns,owner)?;
                self.stmts(then_body,ns,owner)?;
                self.stmts(else_body,ns,owner)?;
            }
            Expr::Block(body) => self.stmts(body,ns,owner)?,
            Expr::Call(name,args)|Expr::GenericCall(name,_,args)=>{
                self.call(name,ns,owner)?;
                for e in args {self.expr(e,ns,owner)?;}
            },
            Expr::MethodCall(_,name,_, _)=>return Err(format!("[[pure]] function '{owner}' uses method '{name}' without an effect-checked receiver")),
            Expr::CallIndirect(_,_)=>return Err(format!("[[pure]] function '{owner}' uses an indirect call with unknown effects")),
            Expr::Await(_)|Expr::Yield(_)=>return Err(format!("[[pure]] function '{owner}' cannot suspend")),
            Expr::Lambda{..}=>return Err(format!("[[pure]] function '{owner}' needs effect-typed closures; nested lambda not allowed yet")),
            Expr::Binary(a,_,b)|Expr::Index(a,b)=>{self.expr(a,ns,owner)?;self.expr(b,ns,owner)?;},
            Expr::Unary(_,value)|Expr::Deref(value)|Expr::Spread(value)|Expr::Field(value,_)|
            Expr::Cast(value,_)|Expr::AddressOf{value,..}=>self.expr(value,ns,owner)?,
            Expr::Array(values)|Expr::MacroCall(_,values)=>for value in values{self.expr(value,ns,owner)?;},
            Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>for (_,e) in fields{self.expr(e,ns,owner)?;},
            _=>{},
        }
        Ok(())
    }
    fn stmts(&self, body: &[Stmt], ns: &str, owner: &str) -> Result<(), String> {
        for stmt in body {
            match stmt {
                Stmt::Let { init, defer, .. } => {
                    self.expr(init, ns, owner)?;
                    if let Some(e) = defer {
                        self.expr(e, ns, owner)?;
                    }
                }
                Stmt::Set { value, .. }
                | Stmt::DerefSet { value, .. }
                | Stmt::FieldSet { value, .. }
                | Stmt::CompoundSet { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Defer(value)
                | Stmt::Expr(value) | Stmt::Tail(value) => self.expr(value, ns, owner)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.expr(condition, ns, owner)?;
                    self.stmts(then_body, ns, owner)?;
                    if let Some(body) = else_body {
                        self.stmts(body, ns, owner)?;
                    }
                }
                Stmt::While { condition, body } => {
                    self.expr(condition, ns, owner)?;
                    self.stmts(body, ns, owner)?;
                }
                Stmt::For {
                    start, end, body, ..
                } => {
                    self.expr(start, ns, owner)?;
                    self.expr(end, ns, owner)?;
                    self.stmts(body, ns, owner)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    self.expr(iterable, ns, owner)?;
                    self.stmts(body, ns, owner)?;
                }
                Stmt::Loop(body) => self.stmts(body, ns, owner)?,
                Stmt::Match { value, arms } => {
                    self.expr(value, ns, owner)?;
                    for arm in arms {
                        self.stmts(&arm.body, ns, owner)?;
                    }
                }
                Stmt::Unsafe(_) => {
                    return Err(format!(
                        "[[pure]] function '{owner}' contains an unsafe block"
                    ))
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }
}
/// Visit the expressions that can contain a direct call, including nested
/// control-flow and block-value expressions. Only *declared* effects are used;
/// unknown indirect/method calls are deliberately rejected in strict effects
/// functions until the type checker can supply resolved target signatures.
fn effect_expr(expr:&Expr, ns:&str, owner:&str, caller:&HashSet<String>, signatures:&HashMap<String,Vec<Attribute>>, methods:&HashMap<String,Vec<Attribute>>, ambiguous_methods:&HashSet<String>)->Result<(),String>{
    match expr {
        Expr::Call(name,args)|Expr::GenericCall(name,_,args) => {
            let called=lookup(signatures,ns,name);
            let required = called
                .map(|attributes| declared_call_effects(name, attributes))
                .unwrap_or_else(|| {
                    let mut effects = EffectSet::new();
                    intrinsic_call_effects(name, &mut effects);
                    effects
                });
            if required.is_empty() {
                let Some(attributes) = called else {
                    return Err(format!("error[NOVA-EFF001]: effect-constrained '{owner}' calls unresolved '{name}'"));
                };
                if !attributes.iter().any(|a|a.name=="pure" || a.name=="const" || a.name=="comptime") {
                    return Err(format!("error[NOVA-EFF003]: '{owner}' calls '{name}' with unknown effects; annotate callee #[effects(...)] or verify it #[pure]"));
                }
            }
            for effect in required {
                if !caller.contains(&effect) {
                    return Err(format!("error[NOVA-EFF002]: '{owner}' calls '{name}' requiring effect '{effect}'; add it to #[effects(...)]"));
                }
            }
            for a in args {effect_expr(a,ns,owner,caller,signatures,methods,ambiguous_methods)?;}
        }
        Expr::MethodCall(receiver, method, args, _) => {
            let Some(attributes) = methods.get(method) else {
                return Err(format!("error[NOVA-EFF004]: '{owner}' calls method '{method}' without a resolved effect signature"));
            };
            if ambiguous_methods.contains(method) {
                return Err(format!("error[NOVA-EFF004]: '{owner}' calls overloaded method '{method}' without a receiver-resolved effect signature"));
            }
            let required = declared_call_effects(method, attributes);
            if required.is_empty() && !attributes.iter().any(|a| a.name == "pure" || a.name == "const" || a.name == "comptime") {
                return Err(format!("error[NOVA-EFF003]: '{owner}' calls method '{method}' with unknown effects; annotate it #[effects(...)] or verify it #[pure]"));
            }
            for effect in required {
                if !caller.contains(&effect) {
                    return Err(format!("error[NOVA-EFF002]: '{owner}' calls method '{method}' requiring effect '{effect}'; add it to #[effects(...)]"));
                }
            }
            effect_expr(receiver,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            for arg in args { effect_expr(arg,ns,owner,caller,signatures,methods,ambiguous_methods)?; }
        }
        Expr::CallIndirect(_,_)=>return Err(format!("error[NOVA-EFF004]: '{owner}' calls an untyped function pointer; use an effect-typed callable")),
        Expr::Lambda{..}=>return Err(format!("error[NOVA-EFF004]: '{owner}' captures a closure with unverified effects")),
        Expr::If{condition,then_body,else_body}=>{
            effect_expr(condition,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            effect_stmts(then_body,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            effect_stmts(else_body,ns,owner,caller,signatures,methods,ambiguous_methods)?;
        }
        Expr::Block(body)=>effect_stmts(body,ns,owner,caller,signatures,methods,ambiguous_methods)?,
        Expr::Binary(a,_,b)|Expr::Index(a,b)=>{
            effect_expr(a,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            effect_expr(b,ns,owner,caller,signatures,methods,ambiguous_methods)?;
        }
        Expr::Unary(_,x)|Expr::Cast(x,_)|Expr::AddressOf{value:x,..}|
        Expr::Field(x,_)|Expr::Spread(x)|Expr::Deref(x)|Expr::Await(x)|Expr::Yield(x)=>
            effect_expr(x,ns,owner,caller,signatures,methods,ambiguous_methods)?,
        Expr::Array(values)|Expr::MacroCall(_,values)=>for e in values {effect_expr(e,ns,owner,caller,signatures,methods,ambiguous_methods)?;},
        Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>
            for (_,e) in fields {effect_expr(e,ns,owner,caller,signatures,methods,ambiguous_methods)?;},
        _=>{}
    }
    Ok(())
}
fn effect_stmts(body:&[Stmt],ns:&str,owner:&str,caller:&HashSet<String>,signatures:&HashMap<String,Vec<Attribute>>,methods:&HashMap<String,Vec<Attribute>>,ambiguous_methods:&HashSet<String>)->Result<(),String>{
    for s in body {
        match s {
            Stmt::Let{init,defer,..}=>{
                effect_expr(init,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                if let Some(defer)=defer {effect_expr(defer,ns,owner,caller,signatures,methods,ambiguous_methods)?;}
            }
            Stmt::Set{value,..}|Stmt::DerefSet{value,..}|Stmt::FieldSet{value,..}|
            Stmt::CompoundSet{value,..}|Stmt::Return(Some(value))|Stmt::Defer(value)|
            Stmt::Expr(value)|Stmt::Tail(value)=>effect_expr(value,ns,owner,caller,signatures,methods,ambiguous_methods)?,
            Stmt::If{condition,then_body,else_body}=>{
                effect_expr(condition,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                effect_stmts(then_body,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                if let Some(body)=else_body {effect_stmts(body,ns,owner,caller,signatures,methods,ambiguous_methods)?;}
            }
            Stmt::While{condition,body}=>{
                effect_expr(condition,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                effect_stmts(body,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            }
            Stmt::For{start,end,body,..}=>{
                effect_expr(start,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                effect_expr(end,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                effect_stmts(body,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            }
            Stmt::ForEach{iterable,body,..}=>{
                effect_expr(iterable,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                effect_stmts(body,ns,owner,caller,signatures,methods,ambiguous_methods)?;
            }
            Stmt::Loop(body)|Stmt::Unsafe(body)=>effect_stmts(body,ns,owner,caller,signatures,methods,ambiguous_methods)?,
            Stmt::Match{value,arms}=>{
                effect_expr(value,ns,owner,caller,signatures,methods,ambiguous_methods)?;
                for arm in arms {effect_stmts(&arm.body,ns,owner,caller,signatures,methods,ambiguous_methods)?;}
            }
            Stmt::Return(None)|Stmt::Break|Stmt::Continue=>{}
        }
    }
    Ok(())
}

type EffectSet = HashSet<String>;

fn declared_call_effects(name: &str, attributes: &[Attribute]) -> EffectSet {
    let mut effects = EffectSet::new();
    intrinsic_call_effects(name, &mut effects);
    for attribute in attributes {
        match attribute.name.as_str() {
            "async" => { effects.insert("async".into()); }
            "unsafe" => { effects.insert("unsafe".into()); }
            "io" => { effects.insert("io".into()); }
            "effects" => effects.extend(attribute.args.iter().cloned()),
            _ => {}
        }
    }
    effects
}

fn resolved_name(signatures: &HashMap<String, Vec<Attribute>>, ns: &str, name: &str) -> Option<String> {
    if name.contains("::") && signatures.contains_key(name) {
        return Some(name.into());
    }
    let mut scope = ns;
    loop {
        let candidate = qual(scope, name);
        if signatures.contains_key(&candidate) {
            return Some(candidate);
        }
        if scope.is_empty() {
            return None;
        }
        scope = scope.rsplit_once("::").map(|(parent, _)| parent).unwrap_or("");
    }
}

fn intrinsic_call_effects(name: &str, effects: &mut EffectSet) {
    if name.contains("std::old::io::") || name.contains("std__old__io__") {
        effects.insert("io".into());
    }
    if name.ends_with("::spawn") || name.ends_with("__spawn") {
        effects.insert("spawn".into());
    }
    if name.ends_with("::join") || name.ends_with("__join") {
        effects.insert("blocking".into());
    }
    if name.contains("std::abi::llvm::") || name.contains("std__abi__llvm__") {
        effects.insert("unsafe".into());
    }
}

fn collect_inferred_expr(
    expr: &Expr,
    ns: &str,
    signatures: &HashMap<String, Vec<Attribute>>,
    methods: &HashMap<String, Vec<Attribute>>,
    ambiguous_methods: &HashSet<String>,
    direct: &mut EffectSet,
    calls: &mut Vec<String>,
) {
    match expr {
        Expr::Call(name, args) | Expr::GenericCall(name, _, args) => {
            intrinsic_call_effects(name, direct);
            if let Some(target) = resolved_name(signatures, ns, name) {
                calls.push(target);
            } else {
                direct.insert("unknown".into());
            }
            for arg in args { collect_inferred_expr(arg, ns, signatures, methods, ambiguous_methods, direct, calls); }
        }
        Expr::MethodCall(receiver, method, args, _) => {
            if let Some(attributes) = methods.get(method).filter(|_| !ambiguous_methods.contains(method)) {
                direct.extend(declared_call_effects(method, attributes));
            } else {
                direct.insert("unknown".into());
            }
            collect_inferred_expr(receiver, ns, signatures, methods, ambiguous_methods, direct, calls);
            for arg in args { collect_inferred_expr(arg, ns, signatures, methods, ambiguous_methods, direct, calls); }
        }
        Expr::CallIndirect(callee, args) => {
            direct.insert("unknown".into());
            collect_inferred_expr(callee, ns, signatures, methods, ambiguous_methods, direct, calls);
            for arg in args { collect_inferred_expr(arg, ns, signatures, methods, ambiguous_methods, direct, calls); }
        }
        Expr::Await(inner) | Expr::Yield(inner) => {
            direct.insert("async".into());
            collect_inferred_expr(inner, ns, signatures, methods, ambiguous_methods, direct, calls);
        }
        Expr::Lambda { body, .. } => {
            direct.insert("unknown".into());
            collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls);
        }
        Expr::If { condition, then_body, else_body } => {
            collect_inferred_expr(condition, ns, signatures, methods, ambiguous_methods, direct, calls);
            collect_inferred_stmts(then_body, ns, signatures, methods, ambiguous_methods, direct, calls);
            collect_inferred_stmts(else_body, ns, signatures, methods, ambiguous_methods, direct, calls);
        }
        Expr::Block(body) => collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls),
        Expr::Binary(left, _, right) | Expr::Index(left, right) => {
            collect_inferred_expr(left, ns, signatures, methods, ambiguous_methods, direct, calls);
            collect_inferred_expr(right, ns, signatures, methods, ambiguous_methods, direct, calls);
        }
        Expr::Unary(_, inner) | Expr::Cast(inner, _) | Expr::AddressOf { value: inner, .. }
        | Expr::Field(inner, _) | Expr::Spread(inner) | Expr::Deref(inner) =>
            collect_inferred_expr(inner, ns, signatures, methods, ambiguous_methods, direct, calls),
        Expr::Array(values) | Expr::MacroCall(_, values) =>
            for value in values { collect_inferred_expr(value, ns, signatures, methods, ambiguous_methods, direct, calls); },
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) =>
            for (_, value) in fields { collect_inferred_expr(value, ns, signatures, methods, ambiguous_methods, direct, calls); },
        _ => {}
    }
}

fn collect_inferred_stmts(
    body: &[Stmt],
    ns: &str,
    signatures: &HashMap<String, Vec<Attribute>>,
    methods: &HashMap<String, Vec<Attribute>>,
    ambiguous_methods: &HashSet<String>,
    direct: &mut EffectSet,
    calls: &mut Vec<String>,
) {
    for stmt in body {
        match stmt {
            Stmt::Unsafe(body) => {
                direct.insert("unsafe".into());
                collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls);
            }
            Stmt::Let { init, defer, .. } => {
                collect_inferred_expr(init, ns, signatures, methods, ambiguous_methods, direct, calls);
                if let Some(value) = defer { collect_inferred_expr(value, ns, signatures, methods, ambiguous_methods, direct, calls); }
            }
            Stmt::Set { value, .. } | Stmt::DerefSet { value, .. }
            | Stmt::FieldSet { value, .. } | Stmt::CompoundSet { value, .. }
            | Stmt::Return(Some(value)) | Stmt::Defer(value)
            | Stmt::Expr(value) | Stmt::Tail(value) =>
                collect_inferred_expr(value, ns, signatures, methods, ambiguous_methods, direct, calls),
            Stmt::If { condition, then_body, else_body } => {
                collect_inferred_expr(condition, ns, signatures, methods, ambiguous_methods, direct, calls);
                collect_inferred_stmts(then_body, ns, signatures, methods, ambiguous_methods, direct, calls);
                if let Some(body) = else_body { collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls); }
            }
            Stmt::While { condition, body } => {
                collect_inferred_expr(condition, ns, signatures, methods, ambiguous_methods, direct, calls);
                collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls);
            }
            Stmt::For { start, end, body, .. } => {
                collect_inferred_expr(start, ns, signatures, methods, ambiguous_methods, direct, calls);
                collect_inferred_expr(end, ns, signatures, methods, ambiguous_methods, direct, calls);
                collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls);
            }
            Stmt::ForEach { iterable, body, .. } => {
                collect_inferred_expr(iterable, ns, signatures, methods, ambiguous_methods, direct, calls);
                collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls);
            }
            Stmt::Loop(body) => collect_inferred_stmts(body, ns, signatures, methods, ambiguous_methods, direct, calls),
            Stmt::Match { value, arms } => {
                collect_inferred_expr(value, ns, signatures, methods, ambiguous_methods, direct, calls);
                for arm in arms { collect_inferred_stmts(&arm.body, ns, signatures, methods, ambiguous_methods, direct, calls); }
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn infer_effects(effects: &Effects) -> HashMap<String, EffectSet> {
    let mut inferred = HashMap::new();
    let mut calls = HashMap::<String, Vec<String>>::new();
    for (name, namespace, function) in &effects.definitions {
        let mut direct = EffectSet::new();
        if has(&function.attributes, "async") || !matches!(function.mode, FunctionMode::Normal) {
            direct.insert("async".into());
        }
        // `[[unsafe]]` changes the privilege of the function body, but it does
        // not make calls to that function safe. Keep the two rules separate:
        // HIR seeds the body with an unsafe context, while effect inference
        // propagates the unsafe requirement to callers.
        if has(&function.attributes, "unsafe") {
            direct.insert("unsafe".into());
        }
        if let Some(attribute) = function.attributes.iter().find(|a| a.name == "effects") {
            direct.extend(attribute.args.iter().cloned());
        }
        let mut targets = Vec::new();
        collect_inferred_stmts(
            &function.body,
            namespace,
            &effects.signatures,
            &effects.methods,
            &effects.ambiguous_methods,
            &mut direct,
            &mut targets,
        );
        inferred.insert(name.clone(), direct);
        calls.insert(name.clone(), targets);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (owner, targets) in &calls {
            let additions = targets.iter().filter_map(|target| inferred.get(target))
                .flat_map(|set| set.iter().cloned()).collect::<Vec<_>>();
            let owner_effects = inferred.get_mut(owner).expect("inferred function exists");
            for effect in additions { changed |= owner_effects.insert(effect); }
        }
    }
    inferred
}

fn validate_inferred_effects(effects: &Effects) -> Result<(), String> {
    let inferred = infer_effects(effects);
    for (name, _, function) in &effects.definitions {
        let Some(contract) = function.attributes.iter().find(|attribute| attribute.name == "effects") else {
            continue;
        };
        let allowed = contract.args.iter().cloned().collect::<EffectSet>();
        for effect in inferred.get(name).into_iter().flat_map(|set| set.iter()) {
            // The unsafe function marker is itself the declaration of the
            // function's unsafe effect. Requiring a second #[effects(unsafe)]
            // annotation would duplicate the source-level contract.
            if effect == "unsafe" && has(&function.attributes, "unsafe") {
                continue;
            }
            if !allowed.contains(effect) {
                return Err(format!(
                    "error[NOVA-EFF005]: '{name}' requires inferred effect '{effect}'; add it to #[effects(...)]"
                ));
            }
        }
    }
    Ok(())
}

// Scalar CTFE executes expressions; procedural CTFE executes typed Nova AST
// transformations. Keep their accepted types distinct so arbitrary runtime
// structs cannot be smuggled into the constant evaluator.
fn scalar_comptime_type(ty: &ast::TypeRef) -> bool {
    matches!(ty, ast::TypeRef::Named(name) if matches!(name.as_str(), "i64" | "i32" | "bool" | "str"))
}

fn procedural_meta_type(ty: &ast::TypeRef, _namespace: &str) -> bool {
    let ast::TypeRef::Named(name) = ty else { return false; };
    let short_name = name.rsplit("::").next().unwrap_or(name);
    matches!(short_name,
        "MetaProgram" | "MetaType" | "MetaClass" | "MetaField" | "MetaFields" | "MetaMethods" |
        "MetaMethod" | "MetaTypeRef" | "MetaExpr" | "MetaDecl" |
        "MetaDecls" | "MetaArgs")
}

pub fn validate(program: &ast::Program) -> Result<(), String> {
    let mut effects = Effects::default();
    effects.collect(&program.items, "")?;
    for (name, namespace, function) in &effects.definitions {
        if let Some(contract)=function.attributes.iter().find(|a|a.name=="effects") {
            let mut declared=contract.args.iter().cloned().collect::<HashSet<_>>();
            // An unsafe function is already inside an unsafe capability
            // context. Its signature remains unsafe for callers, but its own
            // body must be able to call unsafe functions without boilerplate.
            if has(&function.attributes, "unsafe") {
                declared.insert("unsafe".into());
            }
            effect_stmts(
                &function.body,
                namespace,
                name,
                &declared,
                &effects.signatures,
                &effects.methods,
                &effects.ambiguous_methods,
            )?;
        }
        if has(&function.attributes, "comptime") || has(&function.attributes, "comptime_assert") {
            if let Some(contract) = function.attributes.iter().find(|a| a.name == "effects") {
                const RUNTIME_EFFECTS: &[&str] = &[
                    "async", "throws", "unsafe", "io", "blocking", "alloc", "spawn", "cancel",
                ];
                if let Some(effect) = contract.args.iter().find(|effect| RUNTIME_EFFECTS.contains(&effect.as_str())) {
                    return Err(format!(
                        "error[NOVA-META009]: compile-time function '{name}' cannot require runtime effect '{effect}'"
                    ));
                }
            }
        }
        if has(&function.attributes, "pure") || has(&function.attributes, "const") || has(&function.attributes,"comptime") || has(&function.attributes,"comptime_assert") {
            effects.stmts(&function.body, namespace, name)?;
            if (has(&function.attributes,"comptime") || has(&function.attributes,"comptime_assert"))
                && !has(&function.attributes,"meta_proc_intrinsic") {
                // The constant evaluator accepts scalar values; the procedural
                // AST evaluator additionally accepts its declared metadata handles.
                // Do not treat arbitrary named structs as compile-time types.
                let procedural_signature = has(&function.attributes, "comptime")
                    && !has(&function.attributes, "comptime_assert")
                    && (function.params.iter().any(|p| procedural_meta_type(&p.ty, namespace))
                        || procedural_meta_type(&function.result, namespace));
                let allowed = |ty: &ast::TypeRef| {
                    scalar_comptime_type(ty)
                        || (procedural_signature && procedural_meta_type(ty, namespace))
                };
                if !function.params.iter().all(|p| allowed(&p.ty)) || !allowed(&function.result) {
                    return Err(format!("error[NOVA-CTFE017]: '{name}' uses a parameter or return type unsupported by the compile-time evaluator"));
                }
                if !function.generics.is_empty() {
                    return Err(format!("error[NOVA-CTFE017]: '{name}' cannot be generic until typed CTFE monomorphization is implemented"));
                }
                if has(&function.attributes,"comptime_assert") && (!function.params.is_empty() || !matches!(&function.result,ast::TypeRef::Named(t) if t=="bool")) {
                    return Err(format!("error[NOVA-CTFE017]: #[comptime_assert] '{name}' must be fn() -> bool"));
                }
            }
            if has(&function.attributes, "const") {
                for p in &function.params {
                    if !matches!(&p.ty,ast::TypeRef::Named(t) if t=="i64" || t=="i32") {
                        return Err(format!("const fn '{name}': stage-0 const evaluator supports i32/i64 parameters"));
                    }
                }
                if !matches!(&function.result,ast::TypeRef::Named(t) if t=="i64" || t=="i32") {
                    return Err(format!(
                        "const fn '{name}': stage-0 const evaluator supports i32/i64 results"
                    ));
                }
            }
        }
    }
    // Check explicit contracts at the call site first. This produces the
    // useful boundary diagnostic (NOVA-EFF002) when a caller declares the
    // wrong capability; the fixed-point inference below then catches missing
    // annotations on otherwise unannotated callers (NOVA-EFF005).
    validate_inferred_effects(&effects)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn check(source: &str) -> Result<(), String> {
        let tree = grammar::ProgramParser::new()
            .parse(source)
            .map_err(|e| format!("{e:?}"))?;
        validate(&tree)
    }
    #[test]
    fn pure_chain_is_accepted() {
        check("[[pure]] fn twice(x:i64)->i64{return x*2;} [[pure]] fn four(x:i64)->i64{return twice(twice(x));}").unwrap();
    }
    #[test]
    fn pure_rejects_unknown_impure_and_unsafe() {
        assert!(
            check("fn io()->i64{return 0;} [[pure]] fn p()->i64{return io();}")
                .unwrap_err()
                .contains("impure")
        );
        assert!(check("[[pure]] fn p()->i32{unsafe {return 1;}}")
            .unwrap_err()
            .contains("unsafe block"));
    }
    #[test]
    fn effect_marker_validation() {
        assert!(check("[[pure]] [[io]] fn p()->i32{return 0;}")
            .unwrap_err()
            .contains("cannot be combined"));
        assert!(check("[[async]] fn p()->i32{return 0;}")
            .unwrap_err()
            .contains("requires an async"));
    }
    #[test]
    fn comptime_ast_helpers_use_procedural_meta_types() {
        check(r#"
            namespace std { namespace meta {
                struct MetaType {}
                struct MetaDecl {}
                #[comptime] fn generate_setters(target: MetaType) -> MetaDecl { return 0; }
            }}
            #[comptime]
            fn make_properties(target: std::meta::MetaType,
                               args: std::meta::MetaArgs) -> std::meta::MetaDecl {
                return 0;
            }
        "#).unwrap();
    }
    #[test]
    fn comptime_meta_signatures_do_not_allow_unrelated_types() {
        let unrelated = check("struct Ordinary {} #[comptime] fn bad(value:Ordinary)->i64{return 0;}")
            .unwrap_err();
        assert!(unrelated.contains("NOVA-CTFE017"), "{unrelated}");

        let mixed = check("#[comptime] fn bad(target:std::meta::MetaType, other:Ordinary)->std::meta::MetaDecl{return 0;}")
            .unwrap_err();
        assert!(mixed.contains("NOVA-CTFE017"), "{mixed}");

        let wrong_return = check("#[comptime] fn bad(target:std::meta::MetaType)->Ordinary{return 0;}")
            .unwrap_err();
        assert!(wrong_return.contains("NOVA-CTFE017"), "{wrong_return}");

        let assertion = check("#[comptime_assert] fn bad(value:std::meta::MetaType)->bool{return true;}")
            .unwrap_err();
        assert!(assertion.contains("NOVA-CTFE017"), "{assertion}");
    }
    #[test]
    fn method_metadata_with_path_does_not_become_an_effect() {
        // The metadata checker owns the arity contract for Route; the
        // effects pass must not misclassify its path as an effect parameter.
        let source = r#"
            #[meta_attribute(function, args_exact_1)] trait Route {}
            class Controller {}
            impl Controller {
                @Route("/hello") static fn hello() -> i32 {return 200;}
            }
        "#;
        let program = grammar::ProgramParser::new().parse(source).unwrap();
        crate::meta::validate(&program).unwrap();
        validate(&program).unwrap();

        // The same rule covers fully qualified names from imported packages.
        let nb_mapping = Attribute {
            name: "NB::GetMapping".into(),
            args: vec!["\"/users/{id}\"".into()],
        };
        attrs(&[nb_mapping], "method 'find'", Some(&FunctionMode::Normal))
            .unwrap();
    }
}

#[cfg(test)]
mod meta_effect_tests {
    use super::*;
    use crate::grammar;
    fn check(src:&str)->Result<(),String>{
        let ast=grammar::ProgramParser::new().parse(src).map_err(|e|format!("{e:?}"))?;
        crate::meta::validate(&ast)?;
        validate(&ast)
    }
    #[test]
    fn source_declared_effects_check_calls() {
        let prefix="#[meta_effect] trait network {} #[effects(network)] fn request()->i32{return 1;} ";
        check(&format!("{prefix} #[effects(network)] fn main()->i32{{return request();}}" )).unwrap();
        assert!(check(&format!("{prefix} #[effects(database)] fn main()->i32{{return request();}}" )).unwrap_err().contains("NOVA-META008"));
        let database_error = check(&format!("{prefix} #[meta_effect] trait database {{}} #[effects(database)] fn main()->i32{{return request();}}" )).unwrap_err();
        assert!(database_error.contains("NOVA-EFF002") || database_error.contains("NOVA-EFF005"));
        assert!(check(&format!("{prefix} fn other()->i32{{return 1;}} #[effects(network)] fn main()->i32{{return other();}}" )).unwrap_err().contains("NOVA-EFF003"));
    }

    #[test]
    fn inferred_effects_propagate_through_unannotated_callees() {
        let missing = check(
            "async fn worker()->i64{return 1;} #[effects(io)] fn main()->i32{worker();return 0;}"
        ).unwrap_err();
        assert!(missing.contains("NOVA-EFF005"), "{missing}");

        check(
            "async fn worker()->i64{return 1;} #[effects(async)] fn main()->i32{worker();return 0;}"
        ).unwrap();
    }

    #[test]
    fn unsafe_functions_propagate_the_unsafe_effect() {
        let missing = check(
            "[[unsafe]] fn raw()->i32{return 1;} #[effects(io)] fn main()->i32{unsafe{return raw();}}"
        ).unwrap_err();
        assert!(missing.contains("NOVA-EFF005"), "{missing}");

        // The marker itself is sufficient for the unsafe function's own
        // contract; callers still need an unsafe context (checked by HIR).
        check(
            "[[unsafe]] fn raw()->i32{return 1;} #[effects(io)] [[unsafe]] fn main()->i32{unsafe{return raw();}}"
        ).unwrap();
    }

    #[test]
    fn builtin_effects_are_checked_at_contract_boundaries() {
        check(
            "async fn worker()->i64{return 1;} #[effects(async)] fn main()->i32{return worker() as i32;}"
        ).unwrap();

        let missing = check(
            "async fn worker()->i64{return 1;} #[effects(io)] fn main()->i32{return worker() as i32;}"
        ).unwrap_err();
        assert!(missing.contains("NOVA-EFF002"), "{missing}");
    }

    #[test]
    fn comptime_functions_cannot_publish_runtime_effects() {
        let error = check(
            "#[comptime] #[effects(io)] fn inspect()->i32{return 1;}"
        ).unwrap_err();
        assert!(error.contains("NOVA-META009"), "{error}");
    }
}
