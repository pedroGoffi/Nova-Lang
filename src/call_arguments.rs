//! AST lowering for named arguments and constant default parameters.
//!
//! This stage runs BEFORE typechecking/monomorphization. It converts all
//! supported calls to positional arguments, so the LLVM ABI is unchanged.
//! No runtime dispatcher, hidden overload or function-wrapper is generated.
use crate::ast::{self, Expr, Item, Stmt};
use std::collections::{HashMap, HashSet};

const NAMED_MARKER: &str = "__nova_internal_named_arg";

#[derive(Clone)]
struct Signature {
    parameters: Vec<ast::Param>,
    variadic: bool,
}

#[derive(Default)]
struct Binder {
    functions: HashMap<String, Signature>,
    methods: HashMap<String, Vec<Signature>>,
}

fn qualified(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}::{name}")
    }
}

fn is_constant(expression: &Expr) -> bool {
    match expression {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Unit
        | Expr::Zero(_) => true,
        Expr::Unary(_, operand) | Expr::Cast(operand, _) => is_constant(operand),
        Expr::Binary(left, _, right) => is_constant(left) && is_constant(right),
        _ => false,
    }
}

fn check_signature(name: &str, parameters: &[ast::Param], variadic: bool) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut found_default = false;
    for (index, parameter) in parameters.iter().enumerate() {
        if !seen.insert(&parameter.name) {
            return Err(format!(
                "function '{name}' declares duplicate parameter '{}'",
                parameter.name
            ));
        }
        if let Some(default) = &parameter.default {
            if !is_constant(default) {
                return Err(format!(
                    "default for '{name}::{}' must be a closed compile-time expression",
                    parameter.name
                ));
            }
            found_default = true;
        } else if found_default {
            return Err(format!(
                "required parameter '{}' follows a default in '{name}'",
                parameter.name
            ));
        }
        if variadic && index == parameters.len().saturating_sub(1) && parameter.default.is_some() {
            return Err(format!(
                "variadic pack '{}' cannot have a default",
                parameter.name
            ));
        }
    }
    Ok(())
}

impl Binder {
    fn gather(&mut self, items: &[Item], namespace: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    self.gather(std::slice::from_ref(inner.as_ref()), namespace)?
                }
                Item::Namespace { name, items } => {
                    self.gather(items, &qualified(namespace, name))?
                }
                Item::Function(function) => {
                    let name = qualified(namespace, &function.name);
                    check_signature(&name, &function.params, function.variadic)?;
                    if self
                        .functions
                        .insert(
                            name.clone(),
                            Signature {
                                parameters: function.params.clone(),
                                variadic: function.variadic,
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("function '{name}' declared more than once; overload resolution is not implemented"));
                    }
                }
                Item::Impl(block) => {
                    for method in &block.methods {
                        let name = qualified(namespace, &method.name);
                        check_signature(&name, &method.params, method.variadic)?;
                        self.methods.entry(name).or_default().push(Signature {
                            parameters: method.params.clone(),
                            variadic: method.variadic,
                        });
                    }
                }
                Item::Extern(external) => {
                    let name = qualified(namespace, &external.name);
                    if external
                        .params
                        .iter()
                        .any(|parameter| parameter.default.is_some())
                    {
                        return Err(format!(
                            "external function '{name}' cannot declare default arguments"
                        ));
                    }
                    self.functions.insert(
                        name,
                        Signature {
                            parameters: external.params.clone(),
                            variadic: external.c_variadic,
                        },
                    );
                }
                Item::Trait(definition) => {
                    for method in &definition.methods {
                        if method
                            .params
                            .iter()
                            .any(|parameter| parameter.default.is_some())
                        {
                            return Err(format!(
                                "trait declaration '{}::{}' cannot declare defaults",
                                definition.name, method.name
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn function(&self, name: &str, namespace: &str) -> Option<&Signature> {
        if name.contains("::") {
            return self.functions.get(name);
        }
        let mut current = namespace;
        loop {
            if let Some(signature) = self.functions.get(&qualified(current, name)) {
                return Some(signature);
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

    fn bind(
        &self,
        name: &str,
        signature: &Signature,
        arguments: Vec<Expr>,
        implicit_receiver: bool,
    ) -> Result<Vec<Expr>, String> {
        let receiver_count = usize::from(implicit_receiver);
        if signature.parameters.len() < receiver_count {
            return Err(format!("method '{name}' has no receiver parameter"));
        }
        let parameters = &signature.parameters[receiver_count..];
        let mut positional = Vec::new();
        let mut named = HashMap::new();
        let mut named_order = Vec::new();
        let mut has_named = false;
        for argument in arguments {
            if let Expr::Call(marker, mut values) = argument {
                if marker == NAMED_MARKER {
                    if values.len() != 2 {
                        return Err("invalid internal named argument".into());
                    }
                    let value = values.pop().unwrap();
                    let Expr::Str(parameter_name) = values.pop().unwrap() else {
                        return Err("invalid named argument identifier".into());
                    };
                    if named.insert(parameter_name.clone(), value).is_some() {
                        return Err(format!(
                            "duplicate named argument '{parameter_name}' in '{name}'"
                        ));
                    }
                    named_order.push(parameter_name);
                    has_named = true;
                    continue;
                }
                if has_named {
                    return Err(format!(
                        "positional argument follows named arguments in '{name}'"
                    ));
                }
                positional.push(Expr::Call(marker, values));
            } else {
                if has_named {
                    return Err(format!(
                        "positional argument follows named arguments in '{name}'"
                    ));
                }
                positional.push(argument);
            }
        }
        if signature.variadic {
            if has_named {
                return Err(format!(
                    "named arguments are unsupported for variadic '{name}'"
                ));
            }
            return Ok(positional);
        }
        // Spread packs expand in the surface monomorphizer, *after* this pass.
        // Do not count `...pair` as a single positional argument here.
        if positional
            .iter()
            .any(|argument| matches!(argument, Expr::Spread(_)))
        {
            if has_named {
                return Err(format!(
                    "named arguments cannot be mixed with spread in '{name}'"
                ));
            }
            return Ok(positional);
        }
        if positional.len() > parameters.len() {
            return Err(format!("'{name}' received too many arguments"));
        }
        // The core LLVM lowering evaluates positional arguments left-to-right.
        // Reordering side-effecting named arguments would silently change the
        // program's behavior. Restrict reordered named values to constants
        // until AST temporary-binding lowering is implemented.
        let supplied_in_parameter_order = parameters
            .iter()
            .filter(|parameter| named.contains_key(&parameter.name))
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        if supplied_in_parameter_order != named_order
            && named.values().any(|value| !is_constant(value))
        {
            return Err(format!("reordered named arguments in '{name}' must be constant expressions to preserve evaluation order"));
        }
        let mut ordered = positional;
        for parameter in parameters.iter().skip(ordered.len()) {
            if let Some(value) = named.remove(&parameter.name) {
                ordered.push(value);
            } else if let Some(default) = &parameter.default {
                ordered.push(default.clone());
            } else {
                return Err(format!(
                    "'{name}' is missing required argument '{}'",
                    parameter.name
                ));
            }
        }
        // Named args targeting a position already filled positionally are errors.
        for parameter in parameters.iter().take(ordered.len()) {
            if named.contains_key(&parameter.name) {
                return Err(format!(
                    "'{name}' provides '{}' both positionally and by name",
                    parameter.name
                ));
            }
        }
        if let Some(unrecognized) = named.keys().next() {
            return Err(format!(
                "'{name}' has no available argument named '{unrecognized}'"
            ));
        }
        Ok(ordered)
    }

    fn expression(&self, expression: &mut Expr, namespace: &str) -> Result<(), String> {
        match expression {
            Expr::If { condition, then_body, else_body } => {
                self.expression(condition,namespace)?;
                self.statements(then_body,namespace)?;
                self.statements(else_body,namespace)?;
            }
            Expr::Block(body) => self.statements(body,namespace)?,
            Expr::Call(name, values) => {
                if name == NAMED_MARKER {
                    return Err("named argument can only appear in a function call".into());
                }
                let has_named = values
                    .iter()
                    .any(|value| matches!(value,Expr::Call(marker,_) if marker==NAMED_MARKER));
                if let Some(signature) = self.function(name, namespace) {
                    *values = self.bind(name, signature, std::mem::take(values), false)?;
                } else if has_named {
                    return Err(format!(
                        "named arguments require a known Nova function signature for '{name}'"
                    ));
                }
                for argument in values {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::GenericCall(name, _, values) => {
                let has_named = values
                    .iter()
                    .any(|value| matches!(value,Expr::Call(marker,_) if marker==NAMED_MARKER));
                if let Some(signature) = self.function(name, namespace) {
                    *values = self.bind(name, signature, std::mem::take(values), false)?;
                } else if has_named {
                    return Err(format!(
                        "unknown generic function '{name}' for named arguments"
                    ));
                }
                for argument in values {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::MethodCall(receiver, name, values) => {
                self.expression(receiver, namespace)?;
                let has_named = values
                    .iter()
                    .any(|value| matches!(value,Expr::Call(marker,_) if marker==NAMED_MARKER));
                // This pass has not resolved the receiver type yet. Methods from
                // unrelated impls can share a name but have different arities
                // (e.g. Map.insert(key,value) versus Set.insert(value)).
                // Never validate ordinary positional calls against a guessed
                // method signature; surface::Engine resolves the receiver and
                // checks the actual arity and types later.
                //
                // The only positional calls we need to rewrite here are calls
                // relying on method defaults. Do so only when the method name
                // resolves to ONE signature across all receiver types.
                let globally_matching = self
                    .methods
                    .iter()
                    .filter(|(qualified_name, _)| qualified_name.rsplit("::").next() == Some(name))
                    .flat_map(|(_, signatures)| signatures.iter())
                    .collect::<Vec<_>>();
                if has_named {
                    // Preserve the existing namespace preference for named
                    // methods. Only positional calls need the receiver-aware
                    // arity fix below.
                    let method_name = qualified(namespace, name);
                    let signatures = self
                        .methods
                        .get(&method_name)
                        .or_else(|| self.methods.get(name));
                    let candidates = if let Some(signatures) = signatures {
                        signatures.iter().collect::<Vec<_>>()
                    } else {
                        globally_matching.clone()
                    };
                    if candidates.len() == 1 {
                        *values = self.bind(name, candidates[0], std::mem::take(values), true)?;
                    } else {
                        return Err(if candidates.is_empty() {
                            format!("unknown method '{name}' for named arguments")
                        } else {
                            format!(
                                "named arguments for '{name}' are ambiguous across receiver types"
                            )
                        });
                    }
                } else if globally_matching.len() == 1 {
                    let signature = globally_matching[0];
                    let explicit = &signature.parameters[1..];
                    if values.len() < explicit.len()
                        && explicit.iter().any(|param| param.default.is_some())
                    {
                        *values = self.bind(name, signature, std::mem::take(values), true)?;
                    }
                }
                for argument in values {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
                for (_, value) in fields {
                    self.expression(value, namespace)?;
                }
            }
            Expr::Lambda { params, body, .. } => {
                if params.iter().any(|param| param.default.is_some()) {
                    return Err(
                        "lambda parameters cannot have defaults; use a named function".into(),
                    );
                }
                self.statements(body, namespace)?;
            }
            Expr::CallIndirect(callee, values) => {
                self.expression(callee, namespace)?;
                for argument in values {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::MacroCall(_, values) | Expr::Array(values) => {
                for argument in values {
                    self.expression(argument, namespace)?;
                }
            }
            Expr::Unary(_, value)
            | Expr::Field(value, _)
            | Expr::Deref(value)
            | Expr::Yield(value)
            | Expr::Await(value)
            | Expr::Spread(value)
            | Expr::AddressOf { value, .. }
            | Expr::Cast(value, _) => self.expression(value, namespace)?,
            Expr::Binary(left, _, right) | Expr::Index(left, right) => {
                self.expression(left, namespace)?;
                self.expression(right, namespace)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn statements(&self, statements: &mut [Stmt], namespace: &str) -> Result<(), String> {
        for statement in statements {
            match statement {
                Stmt::Let { init, defer, .. } => {
                    self.expression(init, namespace)?;
                    if let Some(cleanup) = defer {
                        self.expression(cleanup, namespace)?;
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
                Stmt::While { condition, body } => {
                    self.expression(condition, namespace)?;
                    self.statements(body, namespace)?;
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
    fn items(&self, items: &mut [Item], namespace: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    self.items(std::slice::from_mut(inner.as_mut()), namespace)?
                }
                Item::Namespace { name, items } => {
                    self.items(items, &qualified(namespace, name))?
                }
                Item::Function(function) => {
                    for parameter in &mut function.params {
                        parameter.default = None;
                    }
                    self.statements(&mut function.body, namespace)?;
                }
                Item::Impl(block) => {
                    for method in &mut block.methods {
                        for parameter in &mut method.params {
                            parameter.default = None;
                        }
                        self.statements(&mut method.body, namespace)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub fn lower(program: &ast::Program) -> Result<ast::Program, String> {
    let mut binder = Binder::default();
    binder.gather(&program.items, "")?;
    let mut lowered = program.clone();
    binder.items(&mut lowered.items, "")?;
    Ok(lowered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn lowered(source: &str) -> Result<ast::Program, String> {
        let parsed = grammar::ProgramParser::new()
            .parse(source)
            .map_err(|error| format!("{error:?}"))?;
        lower(&parsed)
    }
    #[test]
    fn named_and_default_arguments_lower_to_abi_positional_calls() {
        let program = lowered(
            r#"
          fn score(first: i32, second: i32 = 10, third: i32 = 20) -> i32 {
              return first + second + third;
          }
          fn main()->i32 { return score(2, third: 7); }
        "#,
        )
        .unwrap();
        let Item::Function(main) = &program.items[1] else {
            panic!("missing main")
        };
        let Stmt::Return(Some(Expr::Call(_, values))) = &main.body[0] else {
            panic!("expected call")
        };
        assert_eq!(values.len(), 3);
        assert!(matches!(&values[0],Expr::Int(value) if value=="2"));
        assert!(matches!(&values[1],Expr::Int(value) if value=="10"));
        assert!(matches!(&values[2],Expr::Int(value) if value=="7"));
    }
    #[test]
    fn rejects_duplicate_or_unknown_named_arguments() {
        let base = "fn score(first:i32, second:i32=10)->i32{return first+second;}";
        assert!(lowered(&format!(
            "{base} fn main()->i32{{return score(first:1,first:2);}}"
        ))
        .unwrap_err()
        .contains("duplicate named argument"));
        assert!(lowered(&format!(
            "{base} fn main()->i32{{return score(other:2,first:1);}}"
        ))
        .unwrap_err()
        .contains("no available argument"));
        assert!(lowered(&format!(
            "{base} fn main()->i32{{return score(first:1,2);}}"
        ))
        .unwrap_err()
        .contains("positional argument follows named"));
    }
    #[test]
    fn positional_method_calls_wait_for_receiver_type_resolution() {
        // A binder that looks up only `insert` may incorrectly select a map
        // signature for a set call. The typed surface pass must pick the impl.
        let program = lowered(
            r#"
            struct Map { value: i32 }
            impl Map {
                fn insert(self: &Map, key: i32, value: i32) -> bool { return true; }
            }
            struct Set { value: i32 }
            impl Set {
                fn insert(self: &Set, value: i32) -> bool { return true; }
            }
            fn main() -> i32 {
                let map = new Map { value: 0 };
                map.insert(10, 20);
                let set = new Set { value: 0 };
                set.insert(42);
                return 0;
            }
        "#,
        )
        .unwrap();
        let Item::Function(main) = program.items.last().unwrap() else {
            panic!("expected main")
        };
        let Stmt::Expr(Expr::MethodCall(_, _, map_args)) = &main.body[1] else {
            panic!("expected map.insert")
        };
        let Stmt::Expr(Expr::MethodCall(_, _, set_args)) = &main.body[3] else {
            panic!("expected set.insert")
        };
        assert_eq!(map_args.len(), 2);
        assert_eq!(set_args.len(), 1);
    }

    #[test]
    fn positional_methods_are_not_bound_to_unrelated_single_signature() {
        // Even if only Map.insert was collected, the AST binder must not
        // reject set.insert(42); it does not yet know the receiver type.
        lowered(
            r#"
            struct Map { value: i32 }
            impl Map {
                fn insert(self: &Map, key: i32, value: i32) -> bool { return true; }
            }
            fn main() -> i32 {
                let set = 1;
                set.insert(42);
                return 0;
            }
        "#,
        )
        .unwrap();
    }

    #[test]
    fn unique_method_defaults_still_expand() {
        let program = lowered(
            r#"
            struct Counter { value: i32 }
            impl Counter {
                fn increase(self: &Counter, amount: i32 = 3) -> i32 {
                    return self.value + amount;
                }
            }
            fn main() -> i32 {
                let counter = new Counter { value: 1 };
                return counter.increase();
            }
        "#,
        )
        .unwrap();
        let Item::Function(main) = program.items.last().unwrap() else {
            panic!("expected main")
        };
        let Stmt::Return(Some(Expr::MethodCall(_, _, arguments))) = &main.body[1] else {
            panic!("expected method call")
        };
        assert!(matches!(&arguments[..], [Expr::Int(value)] if value=="3"));
    }

    #[test]
    fn rejects_nonconstant_defaults_to_avoid_caller_scope_capture() {
        assert!(lowered("fn f(x:i32=external)->i32{return x;}")
            .unwrap_err()
            .contains("closed compile-time expression"));
    }
}
