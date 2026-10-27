//! Declarative AST macros expanded before type checking, not textual replacement.
//!
//! Rust-inspired typed fragment bindings, ordered `macro_rules!` arms and
//! trailing `$( $fragment:expr ),*`/`+`/`?` AST captures.
//! Template `$( $value ),*` splices expression AST nodes into arrays/calls.
//! No string substitution, compiler plugin or arbitrary compile-time I/O.
//! Full Rust token-tree matchers and procedural macros remain separate work.
use crate::ast::{
    self, Expr, Item, MacroArm, MacroBody, MacroDef, MacroFragment, MacroRepetition, Stmt,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug)]
enum Binding {
    Single(Expr),
    Repeated(Vec<Expr>),
}
type Bindings = HashMap<String, Binding>;

const REPEAT_STAR: &str = "__nova_macro_repeat_star";
const REPEAT_PLUS: &str = "__nova_macro_repeat_plus";
const REPEAT_OPTIONAL: &str = "__nova_macro_repeat_optional";
const STMT_REPEAT_STAR: &str = "__nova_macro_stmt_repeat_star";
const STMT_REPEAT_PLUS: &str = "__nova_macro_stmt_repeat_plus";
const STMT_REPEAT_OPTIONAL: &str = "__nova_macro_stmt_repeat_optional";

// Every declaration introduced by a macro template receives a fresh local
// name. Caller-supplied AST expressions are substituted only AFTER this pass:
// an introduced local cannot capture a free name inside `$argument`.
static HYGIENE_COUNTER: AtomicU64 = AtomicU64::new(0);
fn fresh_local(name: &str) -> String {
    let id = HYGIENE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("__nova_macro_local_{id}_{name}")
}
fn rename_name(name: &mut String, env: &HashMap<String, String>) {
    if let Some(replacement) = env.get(name) {
        *name = replacement.clone();
    }
}
fn rename_expr(expr: &mut Expr, env: &HashMap<String, String>) {
    match expr {
        Expr::Var(name) => rename_name(name, env),
        Expr::Unary(_, value)
        | Expr::Deref(value)
        | Expr::Yield(value)
        | Expr::Await(value)
        | Expr::Spread(value)
        | Expr::Cast(value, _) => {
            rename_expr(value, env);
        }
        Expr::AddressOf { value, .. } => rename_expr(value, env),
        Expr::Binary(left, _, right) | Expr::Index(left, right) => {
            rename_expr(left, env);
            rename_expr(right, env);
        }
        // A call such as `cb(21)` is parsed as a named AST call, even if `cb`
        // is a local holding a function pointer.  Hygiene must rename the
        // callee together with its `let cb = ...` declaration.  Otherwise the
        // surface pass cannot find the original local after alpha-renaming.
        Expr::Call(name, args) => {
            rename_name(name, env);
            for arg in args {
                rename_expr(arg, env);
            }
        }
        // Macro names and generic function paths are declarations, not local
        // value bindings.  Do not alpha-rename those names.
        Expr::MacroCall(_, args) | Expr::Array(args) | Expr::GenericCall(_, _, args) => {
            for arg in args {
                rename_expr(arg, env);
            }
        }
        Expr::CallIndirect(receiver, args) | Expr::MethodCall(receiver, _, args) => {
            rename_expr(receiver, env);
            for arg in args {
                rename_expr(arg, env);
            }
        }
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
            for (_, arg) in fields {
                rename_expr(arg, env);
            }
        }
        Expr::Field(receiver, _) => rename_expr(receiver, env),
        Expr::Lambda { params, body, .. } => {
            let mut env = env.clone();
            for param in params {
                env.remove(&param.name);
                if !param.name.starts_with('$') {
                    let old = param.name.clone();
                    param.name = fresh_local(&old);
                    env.insert(old, param.name.clone());
                }
            }
            rename_stmts(body, &env);
        }
        _ => {}
    }
}
fn rename_stmts(body: &mut [Stmt], inherited: &HashMap<String, String>) {
    let mut env = inherited.clone();
    for stmt in body {
        match stmt {
            Stmt::Let {
                name, init, defer, ..
            } => {
                rename_expr(init, &env);
                if !name.starts_with('$') {
                    let original = name.clone();
                    *name = fresh_local(&original);
                    env.insert(original, name.clone());
                }
                if let Some(expr) = defer {
                    rename_expr(expr, &env);
                }
            }
            Stmt::Set { name, value }
            | Stmt::DerefSet { name, value }
            | Stmt::CompoundSet { name, value, .. } => {
                rename_name(name, &env);
                rename_expr(value, &env);
            }
            Stmt::FieldSet { root, value, .. } => {
                rename_name(root, &env);
                rename_expr(value, &env);
            }
            Stmt::Defer(expr) | Stmt::Expr(expr) => rename_expr(expr, &env),
            Stmt::Return(Some(expr)) => rename_expr(expr, &env),
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                rename_expr(condition, &env);
                rename_stmts(then_body, &env);
                if let Some(body) = else_body {
                    rename_stmts(body, &env);
                }
            }
            Stmt::While { condition, body } => {
                rename_expr(condition, &env);
                rename_stmts(body, &env);
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => rename_stmts(body, &env),
            Stmt::For {
                name,
                start,
                end,
                body,
            } => {
                rename_expr(start, &env);
                rename_expr(end, &env);
                let mut child = env.clone();
                if !name.starts_with('$') {
                    let original = name.clone();
                    *name = fresh_local(&original);
                    child.insert(original, name.clone());
                }
                rename_stmts(body, &child);
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
            } => {
                rename_expr(iterable, &env);
                let mut child = env.clone();
                if !name.starts_with('$') {
                    let original = name.clone();
                    *name = fresh_local(&original);
                    child.insert(original, name.clone());
                }
                rename_stmts(body, &child);
            }
            Stmt::Match { value, arms } => {
                rename_expr(value, &env);
                for arm in arms {
                    let mut child = env.clone();
                    if let ast::Pattern::Variant { bindings, .. } = &arm.pattern {
                        for name in bindings {
                            child.remove(name);
                        }
                    }
                    rename_stmts(&mut arm.body, &child);
                }
            }
            _ => {}
        }
    }
}
fn qualified(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}::{name}")
    }
}
fn gather(
    items: &[Item],
    namespace: &str,
    definitions: &mut HashMap<String, MacroDef>,
) -> Result<(), String> {
    for item in items {
        match item {
            Item::Public(inner) => {
                gather(std::slice::from_ref(inner.as_ref()), namespace, definitions)?
            }
            Item::Namespace { name, items } => {
                gather(items, &qualified(namespace, name), definitions)?
            }
            Item::Macro(definition) => {
                if matches!(
                    definition.name.as_str(),
                    "print" | "println" | "eprint" | "eprintln" | "format"
                ) {
                    return Err(format!(
                        "error[NOVA-FMT009]: `{}` is a reserved built-in output macro",
                        definition.name
                    ));
                }
                let qualified_name = qualified(namespace, &definition.name);
                let mut parameter_names = HashSet::new();
                if definition.arms.is_empty() {
                    return Err(format!(
                        "error[NOVA-MAC001]: macro '{qualified_name}!' needs at least one arm"
                    ));
                }
                for (arm_index, arm) in definition.arms.iter().enumerate() {
                    parameter_names.clear();
                    for (index, parameter) in arm.params.iter().enumerate() {
                        if !parameter_names.insert(&parameter.name) {
                            return Err(format!(
                                "error[NOVA-MAC002]: macro '{qualified_name}!' arm {} binds '{}' more than once",
                                arm_index + 1, parameter.name
                            ));
                        }
                        if parameter.concept.is_some()
                            && !matches!(
                                parameter.fragment,
                                MacroFragment::Expr | MacroFragment::Literal
                            )
                        {
                            return Err(format!(
                                "error[NOVA-MAC017]: macro '{qualified_name}!' arm {}: concept constraints require an `expr` or `literal` capture; '{}' is an identifier/path used for AST substitution",
                                arm_index + 1, parameter.name
                            ));
                        }
                        if parameter.repetition.is_some() && index + 1 != arm.params.len() {
                            return Err(format!(
                                "error[NOVA-MAC003]: macro '{qualified_name}!' arm {}: a repeating AST capture must be the final parameter",
                                arm_index + 1
                            ));
                        }
                    }
                }
                if definitions
                    .insert(qualified_name.clone(), definition.clone())
                    .is_some()
                {
                    return Err(format!("duplicate macro '{qualified_name}'"));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// These helpers exist only at the surface AST stage. An ordinary Nova
// generic specialization checks the inferred argument type against the macro
// concept, including `requires`, trait and multi-type concept predicates.
// Reusing the language's own checker also means no runtime concept checks.
const CONCEPT_HELPER_PREFIX: &str = "__nova_macro_concept_check__";
const CONCEPT_TYPE_PARAM: &str = "__NovaMacroConceptValue";
fn concept_helper_name(macro_name: &str, arm: usize, param: usize) -> String {
    format!("{CONCEPT_HELPER_PREFIX}{macro_name}__arm{arm}__param{param}")
}
fn concept_check_expr(
    qualified_macro: &str,
    short_name: &str,
    arm: usize,
    param: usize,
    argument: Expr,
) -> Expr {
    let helper = concept_helper_name(short_name, arm, param);
    let scope = qualified_macro
        .rsplit_once("::")
        .map(|(ns, _)| ns)
        .unwrap_or("");
    Expr::Call(qualified(scope, &helper), vec![argument])
}
fn concept_helpers(definition: &MacroDef) -> Vec<Item> {
    let mut result = Vec::new();
    for (arm_idx, arm) in definition.arms.iter().enumerate() {
        for (param_idx, param) in arm.params.iter().enumerate() {
            let Some(concept) = &param.concept else {
                continue;
            };
            let type_parameter = ast::TypeRef::Named(CONCEPT_TYPE_PARAM.into());
            let mut args = vec![type_parameter.clone()];
            args.extend(concept.additional_args.iter().cloned());
            result.push(Item::Function(ast::Function {
                mode: ast::FunctionMode::Normal,
                name: concept_helper_name(&definition.name, arm_idx, param_idx),
                generics: vec![CONCEPT_TYPE_PARAM.into()],
                concept_bounds: vec![ast::ConceptBound {
                    name: concept.name.clone(),
                    args,
                }],
                params: vec![ast::Param {
                    name: "value".into(),
                    ty: type_parameter.clone(),
                    default: None,
                }],
                variadic: false,
                result: type_parameter,
                body: vec![Stmt::Return(Some(Expr::Var("value".into())))],
                is_virtual: false,
                is_override: false,
                attributes: vec![],
            }));
        }
    }
    result
}
struct Expander {
    definitions: HashMap<String, MacroDef>,
}
impl Expander {
    fn resolve(&self, namespace: &str, name: &str) -> Option<String> {
        if name.contains("::") {
            if self.definitions.contains_key(name) {
                return Some(name.into());
            }
            if let Some(relative) = name.strip_prefix("crate::") {
                return self
                    .definitions
                    .contains_key(relative)
                    .then(|| relative.into());
            }
            return None;
        }
        let mut namespace = namespace.to_owned();
        loop {
            let candidate = qualified(&namespace, name);
            if self.definitions.contains_key(&candidate) {
                return Some(candidate);
            }
            if namespace.is_empty() {
                return None;
            }
            namespace = namespace
                .rsplit_once("::")
                .map(|(parent, _)| parent.to_owned())
                .unwrap_or_default();
        }
    }

    fn validate_argument(
        fragment: &MacroFragment,
        argument: &Expr,
        macro_name: &str,
        parameter_name: &str,
    ) -> Result<(), String> {
        let valid = match fragment {
            MacroFragment::Expr => true,
            MacroFragment::Ident => {
                matches!(argument,Expr::Var(identifier) if !identifier.contains("::"))
            }
            MacroFragment::Path => matches!(argument, Expr::Var(_)),
            MacroFragment::Literal => matches!(
                argument,
                Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Unit
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(format!("macro '{macro_name}': parameter '{parameter_name}' expects AST fragment {fragment:?}, received {argument:?}"))
        }
    }

    /// Select the first arm whose arity and typed AST fragments match.
    fn match_arm(
        &self,
        qualified_name: &str,
        arm: &MacroArm,
        arm_index: usize,
        arguments: &[Expr],
    ) -> Result<Bindings, String> {
        let fixed = arm.params.iter().filter(|p| p.repetition.is_none()).count();
        let repeated = arm.params.last().filter(|p| p.repetition.is_some());
        let accepted = match repeated.map(|p| p.repetition.unwrap()) {
            None => arguments.len() == fixed,
            Some(MacroRepetition::ZeroOrMore) => arguments.len() >= fixed,
            Some(MacroRepetition::OneOrMore) => arguments.len() > fixed,
            Some(MacroRepetition::Optional) => (fixed..=fixed + 1).contains(&arguments.len()),
        };
        if !accepted {
            return Err(format!(
                "macro '{qualified_name}!' expects {}{} argument(s), received {}",
                fixed,
                match repeated.map(|p| p.repetition.unwrap()) {
                    None => "".to_string(),
                    Some(MacroRepetition::ZeroOrMore) => " or more".to_string(),
                    Some(MacroRepetition::OneOrMore) => " or more (one required)".to_string(),
                    Some(MacroRepetition::Optional) => " or one optional".to_string(),
                },
                arguments.len()
            ));
        }
        let mut bindings = Bindings::new();
        let short_name = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
        for (index, (param, arg)) in arm
            .params
            .iter()
            .take(fixed)
            .zip(arguments.iter())
            .enumerate()
        {
            Self::validate_argument(&param.fragment, arg, qualified_name, &param.name)?;
            let value = if param.concept.is_some() {
                concept_check_expr(qualified_name, short_name, arm_index, index, arg.clone())
            } else {
                arg.clone()
            };
            bindings.insert(param.name.clone(), Binding::Single(value));
        }
        if let Some(param) = repeated {
            let mut rest = Vec::new();
            for arg in &arguments[fixed..] {
                Self::validate_argument(&param.fragment, arg, qualified_name, &param.name)?;
                let value = if param.concept.is_some() {
                    concept_check_expr(qualified_name, short_name, arm_index, fixed, arg.clone())
                } else {
                    arg.clone()
                };
                rest.push(value);
            }
            bindings.insert(param.name.clone(), Binding::Repeated(rest));
        }
        Ok(bindings)
    }

    fn invoke<'a>(
        &'a self,
        namespace: &str,
        name: &str,
        arguments: &[Expr],
        caller_bindings: &Bindings,
        stack: &mut Vec<String>,
    ) -> Result<(&'a MacroBody, Bindings, String), String> {
        let qualified_name = self.resolve(namespace, name).ok_or_else(|| {
            format!("error[NOVA-MAC004]: unknown macro '{name}!'; declare it with `macro` or `macro_rules!`")
        })?;
        if stack.contains(&qualified_name) {
            return Err(format!(
                "error[NOVA-MAC005]: recursive macro expansion: {} -> {qualified_name}",
                stack.join(" -> ")
            ));
        }
        if stack.len() >= 64 {
            return Err(format!(
                "error[NOVA-MAC006]: macro expansion exceeded 64 invocations: {}",
                stack.join(" -> ")
            ));
        }
        // Expand caller-side AST first, including any repeated metavariable splices.
        let evaluated = self.expressions(arguments, namespace, caller_bindings, stack)?;
        let definition = &self.definitions[&qualified_name];
        let mut failures = Vec::new();
        for (index, arm) in definition.arms.iter().enumerate() {
            match self.match_arm(&qualified_name, arm, index, &evaluated) {
                Ok(bindings) => return Ok((&arm.body, bindings, qualified_name)),
                Err(reason) => failures.push(format!("  arm {}: {reason}", index + 1)),
            }
        }
        Err(format!(
            "error[NOVA-MAC007]: no matching arm for '{qualified_name}!' with {} argument(s)\n{}",
            evaluated.len(),
            failures.join("\n")
        ))
    }

    fn identifier(&self, identifier: &str, bindings: &Bindings) -> Result<String, String> {
        if !identifier.starts_with('$') {
            return Ok(identifier.to_owned());
        }
        let argument = bindings.get(identifier).ok_or_else(|| {
            format!("error[NOVA-MAC008]: macro AST placeholder '{identifier}' was not bound")
        })?;
        match argument {
            Binding::Single(Expr::Var(name)) => Ok(name.clone()),
            Binding::Repeated(_) => Err(format!(
                "error[NOVA-MAC009]: repeated placeholder '{identifier}' must occur inside `$(...),*`, `$(...),+` or `$(...)?`"
            )),
            _ => Err(format!(
                "error[NOVA-MAC010]: macro placeholder '{identifier}' must contain an ident/path AST node"
            )),
        }
    }

    fn expression(
        &self,
        expression: &Expr,
        namespace: &str,
        bindings: &Bindings,
        stack: &mut Vec<String>,
    ) -> Result<Expr, String> {
        use Expr::*;
        Ok(match expression {
            Var(name) if bindings.contains_key(name) => match &bindings[name] {
                Binding::Single(expr) => expr.clone(),
                Binding::Repeated(_) => return Err(format!(
                    "error[NOVA-MAC009]: repeated placeholder '{name}' must occur inside a `$(...),*` splice"
                )),
            },
            MacroCall(name, arguments) => {
                if name == "format" {
                    let values = self.expressions(arguments, namespace, bindings, stack)?;
                    return crate::format_macros::expand_format(&values);
                }
                if matches!(name.as_str(), "print" | "println" | "eprint" | "eprintln") {
                    return Err(format!("error[NOVA-FMT008]: `{name}!` is a statement macro; use it as `{name}!(...);`"));
                }
                let (macro_body, parameters, qualified_name) =
                    self.invoke(namespace, name, arguments, bindings, stack)?;
                let MacroBody::Expression(template) = macro_body else {
                    return Err(format!(
                        "statement macro '{qualified_name}!' cannot be used as an expression"
                    ));
                };
                stack.push(qualified_name.clone());
                let definition_namespace = qualified_name
                    .rsplit_once("::")
                    .map(|(parent, _)| parent)
                    .unwrap_or("");
                let mut hygienic_template = template.clone();
                if parameters.keys().all(|name|name.starts_with('$')) {
                    rename_expr(&mut hygienic_template, &HashMap::new());
                }
                let expanded = self.expression(&hygienic_template, definition_namespace, &parameters, stack);
                stack.pop();
                expanded?
            }
            Unary(operator, inner) => Unary(
                *operator,
                Box::new(self.expression(inner, namespace, bindings, stack)?),
            ),
            Binary(left, operator, right) => Binary(
                Box::new(self.expression(left, namespace, bindings, stack)?),
                *operator,
                Box::new(self.expression(right, namespace, bindings, stack)?),
            ),
            Call(name, _arguments) if [REPEAT_STAR,REPEAT_PLUS,REPEAT_OPTIONAL].contains(&name.as_str()) => {
                return Err(format!("error[NOVA-MAC011]: `$(...),*` expansion must appear inside an array or call argument list, not as a scalar expression (got {name})"));
            }
            Call(name, arguments) => Call(
                self.identifier(name, bindings)?,
                self.expressions(arguments, namespace, bindings, stack)?,
            ),
            CallIndirect(callee, arguments) => CallIndirect(
                Box::new(self.expression(callee, namespace, bindings, stack)?),
                self.expressions(arguments, namespace, bindings, stack)?,
            ),
            Lambda {
                params,
                result,
                body,
            } => Lambda {
                params: params.clone(),
                result: result.clone(),
                body: self.statements(body, namespace, bindings, stack)?,
            },
            GenericCall(name, types, arguments) => GenericCall(
                self.identifier(name, bindings)?,
                types.clone(),
                self.expressions(arguments, namespace, bindings, stack)?,
            ),
            MethodCall(receiver, name, arguments) => MethodCall(
                Box::new(self.expression(receiver, namespace, bindings, stack)?),
                self.identifier(name, bindings)?,
                self.expressions(arguments, namespace, bindings, stack)?,
            ),
            StructInit(name, fields) => StructInit(
                self.identifier(name, bindings)?,
                self.fields(fields, namespace, bindings, stack)?,
            ),
            GenericStructInit(name, types, fields) => GenericStructInit(
                self.identifier(name, bindings)?,
                types.clone(),
                self.fields(fields, namespace, bindings, stack)?,
            ),
            Field(receiver, name) => Field(
                Box::new(self.expression(receiver, namespace, bindings, stack)?),
                self.identifier(name, bindings)?,
            ),
            Array(values) => Array(self.expressions(values, namespace, bindings, stack)?),
            Index(receiver, index) => Index(
                Box::new(self.expression(receiver, namespace, bindings, stack)?),
                Box::new(self.expression(index, namespace, bindings, stack)?),
            ),
            AddressOf { mutable, value } => AddressOf {
                mutable: *mutable,
                value: Box::new(self.expression(value, namespace, bindings, stack)?),
            },
            Deref(value) => Deref(Box::new(
                self.expression(value, namespace, bindings, stack)?,
            )),
            Cast(value, target) => Cast(
                Box::new(self.expression(value, namespace, bindings, stack)?),
                target.clone(),
            ),
            Yield(value) => Yield(Box::new(
                self.expression(value, namespace, bindings, stack)?,
            )),
            Await(value) => Await(Box::new(
                self.expression(value, namespace, bindings, stack)?,
            )),
            Spread(value) => Spread(Box::new(
                self.expression(value, namespace, bindings, stack)?,
            )),
            Var(name) => Var(self.identifier(name, bindings)?),
            _ => expression.clone(),
        })
    }
    fn expressions(
        &self,
        expressions: &[Expr],
        namespace: &str,
        bindings: &Bindings,
        stack: &mut Vec<String>,
    ) -> Result<Vec<Expr>, String> {
        let mut output = Vec::new();
        for expression in expressions {
            if let Expr::Call(operation, parts) = expression {
                if [REPEAT_STAR, REPEAT_PLUS, REPEAT_OPTIONAL].contains(&operation.as_str()) {
                    if parts.len() != 1 {
                        return Err("error[NOVA-MAC012]: malformed AST repetition".into());
                    }
                    let mut names = HashSet::new();
                    self.repeated_names(&parts[0], &mut names);
                    let mut count = None;
                    for name in names {
                        if let Some(Binding::Repeated(values)) = bindings.get(&name) {
                            if let Some(prior) = count {
                                if prior != values.len() {
                                    return Err(format!("error[NOVA-MAC013]: repetition uses captures with different lengths ({} versus {})",prior,values.len()));
                                }
                            }
                            count = Some(values.len());
                        }
                    }
                    let length = count.ok_or_else(|| "error[NOVA-MAC014]: repetition template must reference a repeated AST metavariable".to_owned())?;
                    if operation == REPEAT_PLUS && length == 0 {
                        return Err(
                            "error[NOVA-MAC015]: `$(...),+` requires at least one AST element"
                                .into(),
                        );
                    }
                    if operation == REPEAT_OPTIONAL && length > 1 {
                        return Err(
                            "error[NOVA-MAC016]: `$(...)?` permits at most one AST element".into(),
                        );
                    }
                    for index in 0..length {
                        let mut iteration = bindings.clone();
                        for (name, binding) in bindings {
                            if let Binding::Repeated(values) = binding {
                                if values.len() == length {
                                    iteration.insert(
                                        name.clone(),
                                        Binding::Single(values[index].clone()),
                                    );
                                }
                            }
                        }
                        output.push(self.expression(&parts[0], namespace, &iteration, stack)?);
                    }
                    continue;
                }
            }
            output.push(self.expression(expression, namespace, bindings, stack)?);
        }
        Ok(output)
    }
    fn repeated_names(&self, expression: &Expr, found: &mut HashSet<String>) {
        // The expression AST has no iterator visitor; these patterns cover
        // all containers able to host AST metavariables at the splice site.
        match expression {
            Expr::Var(name) if name.starts_with('$') => {
                found.insert(name.clone());
            }
            Expr::Unary(_, inner)
            | Expr::Deref(inner)
            | Expr::Yield(inner)
            | Expr::Await(inner)
            | Expr::Spread(inner)
            | Expr::Cast(inner, _) => self.repeated_names(inner, found),
            Expr::AddressOf { value, .. } => self.repeated_names(value, found),
            Expr::Binary(left, _, right) | Expr::Index(left, right) => {
                self.repeated_names(left, found);
                self.repeated_names(right, found);
            }
            Expr::Call(_, args)
            | Expr::MacroCall(_, args)
            | Expr::Array(args)
            | Expr::GenericCall(_, _, args) => {
                for arg in args {
                    self.repeated_names(arg, found);
                }
            }
            Expr::MethodCall(receiver, _, args) => {
                self.repeated_names(receiver, found);
                for arg in args {
                    self.repeated_names(arg, found);
                }
            }
            Expr::CallIndirect(receiver, args) => {
                self.repeated_names(receiver, found);
                for arg in args {
                    self.repeated_names(arg, found);
                }
            }
            Expr::Field(receiver, _) => self.repeated_names(receiver, found),
            Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
                for (_, expr) in fields {
                    self.repeated_names(expr, found);
                }
            }
            _ => {}
        }
    }
    fn repeated_names_stmt(&self, stmt: &Stmt, found: &mut HashSet<String>) {
        match stmt {
            Stmt::Let { init, defer, .. } => {
                self.repeated_names(init, found);
                if let Some(value) = defer {
                    self.repeated_names(value, found);
                }
            }
            Stmt::Set { value, .. }
            | Stmt::DerefSet { value, .. }
            | Stmt::CompoundSet { value, .. }
            | Stmt::FieldSet { value, .. }
            | Stmt::Defer(value)
            | Stmt::Expr(value) => self.repeated_names(value, found),
            Stmt::Return(Some(value)) => self.repeated_names(value, found),
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                self.repeated_names(condition, found);
                for stmt in then_body {
                    self.repeated_names_stmt(stmt, found);
                }
                if let Some(body) = else_body {
                    for stmt in body {
                        self.repeated_names_stmt(stmt, found);
                    }
                }
            }
            Stmt::While { condition, body } => {
                self.repeated_names(condition, found);
                for stmt in body {
                    self.repeated_names_stmt(stmt, found);
                }
            }
            Stmt::For {
                start, end, body, ..
            } => {
                self.repeated_names(start, found);
                self.repeated_names(end, found);
                for stmt in body {
                    self.repeated_names_stmt(stmt, found);
                }
            }
            Stmt::ForEach { iterable, body, .. } => {
                self.repeated_names(iterable, found);
                for stmt in body {
                    self.repeated_names_stmt(stmt, found);
                }
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => {
                for stmt in body {
                    self.repeated_names_stmt(stmt, found);
                }
            }
            Stmt::Match { value, arms } => {
                self.repeated_names(value, found);
                for arm in arms {
                    for stmt in &arm.body {
                        self.repeated_names_stmt(stmt, found);
                    }
                }
            }
            _ => {}
        }
    }

    fn fields(
        &self,
        fields: &[(String, Expr)],
        namespace: &str,
        bindings: &Bindings,
        stack: &mut Vec<String>,
    ) -> Result<Vec<(String, Expr)>, String> {
        fields
            .iter()
            .map(|(name, value)| {
                Ok((
                    self.identifier(name, bindings)?,
                    self.expression(value, namespace, bindings, stack)?,
                ))
            })
            .collect()
    }
    fn statements(
        &self,
        body: &[Stmt],
        namespace: &str,
        bindings: &Bindings,
        stack: &mut Vec<String>,
    ) -> Result<Vec<Stmt>, String> {
        let mut expanded = Vec::new();
        for statement in body {
            expanded.extend(self.statement(statement, namespace, bindings, stack)?);
        }
        Ok(expanded)
    }
    fn statement(
        &self,
        statement: &Stmt,
        namespace: &str,
        bindings: &Bindings,
        stack: &mut Vec<String>,
    ) -> Result<Vec<Stmt>, String> {
        let expand = |value: &Expr, stack: &mut Vec<String>| {
            self.expression(value, namespace, bindings, stack)
        };
        let transformed = match statement {
            Stmt::If {
                condition: Expr::Var(marker),
                then_body,
                else_body: None,
            } if [STMT_REPEAT_STAR, STMT_REPEAT_PLUS, STMT_REPEAT_OPTIONAL]
                .contains(&marker.as_str()) =>
            {
                let mut names = HashSet::new();
                for stmt in then_body {
                    self.repeated_names_stmt(stmt, &mut names);
                }
                let mut count = None;
                for name in names {
                    if let Some(Binding::Repeated(values)) = bindings.get(&name) {
                        if let Some(previous) = count {
                            if previous != values.len() {
                                return Err(format!("error[NOVA-MAC013]: mismatched AST statement repetition lengths ({previous} and {})",values.len()));
                            }
                        }
                        count = Some(values.len());
                    }
                }
                let length=count.ok_or_else(|| "error[NOVA-MAC014]: repeated statements require a repeated AST metavariable".to_string())?;
                if marker == STMT_REPEAT_PLUS && length == 0 {
                    return Err(
                        "error[NOVA-MAC015]: `$(...)+` requires at least one statement".into(),
                    );
                }
                if marker == STMT_REPEAT_OPTIONAL && length > 1 {
                    return Err(
                        "error[NOVA-MAC016]: `$(...)?` permits at most one statement".into(),
                    );
                }
                let mut expanded = Vec::new();
                for index in 0..length {
                    let mut iteration = bindings.clone();
                    for (name, binding) in bindings {
                        if let Binding::Repeated(values) = binding {
                            if values.len() == length {
                                iteration
                                    .insert(name.clone(), Binding::Single(values[index].clone()));
                            }
                        }
                    }
                    let mut template = then_body.clone();
                    rename_stmts(&mut template, &HashMap::new());
                    expanded.extend(self.statements(&template, namespace, &iteration, stack)?);
                }
                return Ok(expanded);
            }
            Stmt::Expr(Expr::MacroCall(name, arguments)) => {
                // `format!` is an expression even when its value is discarded.
                // Route it through the expression expander, not macro_rules!.
                if name == "format" {
                    return Ok(vec![Stmt::Expr(self.expression(
                        &Expr::MacroCall(name.clone(), arguments.clone()),
                        namespace,
                        bindings,
                        stack,
                    )?)]);
                }
                // Built-in output macros are expanded directly into typed AST
                // statements. Resolve caller metavariables first, so user
                // macro_rules! can forward `println!($value)` safely.
                if matches!(name.as_str(), "print" | "println" | "eprint" | "eprintln") {
                    let values = self.expressions(arguments, namespace, bindings, stack)?;
                    let generated = crate::format_macros::expand(name, &values)
                        .expect("confirmed builtin output macro")?;
                    return Ok(generated);
                }
                let (macro_body, macro_bindings, qualified_name) =
                    self.invoke(namespace, name, arguments, bindings, stack)?;
                let MacroBody::Statements(body) = macro_body else {
                    return Ok(vec![Stmt::Expr(expand(
                        &Expr::MacroCall(name.clone(), arguments.clone()),
                        stack,
                    )?)]);
                };
                stack.push(qualified_name.clone());
                let definition_namespace = qualified_name
                    .rsplit_once("::")
                    .map(|(parent, _)| parent)
                    .unwrap_or("");
                // Rename every template-owned local before caller AST substitution.
                let mut hygienic_body = body.clone();
                if macro_bindings.keys().all(|name| name.starts_with('$')) {
                    rename_stmts(&mut hygienic_body, &HashMap::new());
                }
                let expanded =
                    self.statements(&hygienic_body, definition_namespace, &macro_bindings, stack);
                stack.pop();
                // Block scope is deliberate: declarations inside the macro do
                // not leak into the caller or collide on repeated invocation.
                return Ok(vec![Stmt::If {
                    condition: Expr::Bool(true),
                    then_body: expanded?,
                    else_body: None,
                }]);
            }
            Stmt::Let {
                name,
                mutable,
                ty,
                init,
                defer,
            } => Stmt::Let {
                name: self.identifier(name, bindings)?,
                mutable: *mutable,
                ty: ty.clone(),
                init: expand(init, stack)?,
                defer: defer
                    .as_ref()
                    .map(|value| expand(value, stack))
                    .transpose()?,
            },
            Stmt::Defer(value) => Stmt::Defer(expand(value, stack)?),
            Stmt::Set { name, value } => Stmt::Set {
                name: self.identifier(name, bindings)?,
                value: expand(value, stack)?,
            },
            Stmt::DerefSet { name, value } => Stmt::DerefSet {
                name: self.identifier(name, bindings)?,
                value: expand(value, stack)?,
            },
            Stmt::FieldSet {
                root,
                field,
                op,
                value,
            } => Stmt::FieldSet {
                root: self.identifier(root, bindings)?,
                field: self.identifier(field, bindings)?,
                op: *op,
                value: expand(value, stack)?,
            },
            Stmt::CompoundSet { name, op, value } => Stmt::CompoundSet {
                name: self.identifier(name, bindings)?,
                op: *op,
                value: expand(value, stack)?,
            },
            Stmt::Return(value) => Stmt::Return(
                value
                    .as_ref()
                    .map(|value| expand(value, stack))
                    .transpose()?,
            ),
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => Stmt::If {
                condition: expand(condition, stack)?,
                then_body: self.statements(then_body, namespace, bindings, stack)?,
                else_body: else_body
                    .as_ref()
                    .map(|body| self.statements(body, namespace, bindings, stack))
                    .transpose()?,
            },
            Stmt::While { condition, body } => Stmt::While {
                condition: expand(condition, stack)?,
                body: self.statements(body, namespace, bindings, stack)?,
            },
            Stmt::Loop(body) => Stmt::Loop(self.statements(body, namespace, bindings, stack)?),
            Stmt::For {
                name,
                start,
                end,
                body,
            } => Stmt::For {
                name: self.identifier(name, bindings)?,
                start: expand(start, stack)?,
                end: expand(end, stack)?,
                body: self.statements(body, namespace, bindings, stack)?,
            },
            Stmt::ForEach {
                name,
                iterable,
                body,
            } => Stmt::ForEach {
                name: self.identifier(name, bindings)?,
                iterable: expand(iterable, stack)?,
                body: self.statements(body, namespace, bindings, stack)?,
            },
            Stmt::Unsafe(body) => Stmt::Unsafe(self.statements(body, namespace, bindings, stack)?),
            Stmt::Match { value, arms } => Stmt::Match {
                value: expand(value, stack)?,
                arms: arms
                    .iter()
                    .map(|arm| {
                        Ok(ast::MatchArm {
                            pattern: arm.pattern.clone(),
                            body: self.statements(&arm.body, namespace, bindings, stack)?,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            },
            Stmt::Expr(value) => Stmt::Expr(expand(value, stack)?),
            _ => statement.clone(),
        };
        Ok(vec![transformed])
    }
    fn items(&self, items: &[Item], namespace: &str) -> Result<Vec<Item>, String> {
        let mut result = Vec::new();
        for item in items {
            let transformed = match item {
                Item::Macro(definition) => {
                    result.extend(concept_helpers(definition));
                    continue;
                }
                Item::Public(inner) => {
                    for expanded in self.items(std::slice::from_ref(inner.as_ref()), namespace)? {
                        result.push(Item::Public(Box::new(expanded)));
                    }
                    continue;
                }
                Item::Namespace { name, items } => Item::Namespace {
                    name: name.clone(),
                    items: self.items(items, &qualified(namespace, name))?,
                },
                Item::Function(function) => {
                    let mut function = function.clone();
                    function.body = self.statements(
                        &function.body,
                        namespace,
                        &Bindings::new(),
                        &mut Vec::new(),
                    )?;
                    Item::Function(function)
                }
                Item::Impl(block) => {
                    let mut block = block.clone();
                    for method in &mut block.methods {
                        method.body = self.statements(
                            &method.body,
                            namespace,
                            &Bindings::new(),
                            &mut Vec::new(),
                        )?;
                    }
                    Item::Impl(block)
                }
                _ => item.clone(),
            };
            result.push(transformed);
        }
        Ok(result)
    }
}
pub fn expand(program: &ast::Program) -> Result<ast::Program, String> {
    let mut definitions = HashMap::new();
    gather(&program.items, "", &mut definitions)?;
    Ok(ast::Program {
        items: Expander { definitions }.items(&program.items, "")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn compile(source: &str) -> Result<ast::Program, String> {
        let parsed = grammar::ProgramParser::new()
            .parse(source)
            .map_err(|error| error.to_string())?;
        expand(&parsed)
    }
    #[test]
    fn concept_bound_macro_emits_typed_specialization_checks() {
        let program = compile(
            r#"
            concept Whole<T> = integer<T>;
            macro_rules! checked {
                ($value:expr where Whole) => $value + 1;
            }
            fn main() -> i32 { return checked!(41) - 42; }
        "#,
        )
        .unwrap();
        let helper = program
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name.starts_with(CONCEPT_HELPER_PREFIX) => {
                    Some(function)
                }
                _ => None,
            })
            .expect("macro concept specialization helper");
        assert_eq!(helper.concept_bounds[0].name, "Whole");
        assert_eq!(helper.concept_bounds[0].args.len(), 1);
        let Item::Function(main) = program.items.last().unwrap() else {
            panic!("main not expanded")
        };
        let Stmt::Return(Some(Expr::Binary(value, _, _))) = &main.body[0] else {
            panic!("unexpected main body")
        };
        // The expanded AST must contain the generated typed check.
        assert!(format!("{value:?}").contains(CONCEPT_HELPER_PREFIX));
    }
    #[test]
    fn macro_concept_rejects_identifier_only_capture() {
        let error = compile(
            r#"
            macro_rules! invalid { ($name:ident where signed) => $name; }
            fn main()->i32{return 0;}
        "#,
        )
        .unwrap_err();
        assert!(error.contains("NOVA-MAC017"), "{error}");
    }
    #[test]
    fn repeated_concept_capture_checks_each_element() {
        let program = compile(
            r#"
            macro_rules! checked { ($($value:expr where integer),+) => [$($value),*]; }
            fn main()->i32 { let xs = checked!(1, 2, 3); return 0; }
        "#,
        )
        .unwrap();
        let Item::Function(main) = program.items.last().unwrap() else {
            panic!("missing main")
        };
        let Stmt::Let {
            init: Expr::Array(values),
            ..
        } = &main.body[0]
        else {
            panic!("missing array")
        };
        assert_eq!(values.len(), 3);
        for value in values {
            assert!(matches!(value,Expr::Call(name,_) if name.contains(CONCEPT_HELPER_PREFIX)));
        }
    }
    #[test]
    fn legacy_expression_macro_still_expands() {
        let expanded=compile("namespace ui { macro component(label, width) => label + width; fn build() -> i32 { auto total = component!(2, 3); return total; } }").unwrap();
        let Item::Namespace { items, .. } = &expanded.items[0] else {
            panic!("missing namespace")
        };
        let Item::Function(function) = &items[0] else {
            panic!("missing function")
        };
        assert!(matches!(
            &function.body[0],
            Stmt::Let {
                init: Expr::Binary(..),
                ..
            }
        ));
    }
    #[test]
    fn typed_macro_substitutes_ast_nodes_not_strings() {
        let expanded = compile(
            "macro double($value:expr) => $value + $value; fn main()->i32{return double!(20)+2;}",
        )
        .unwrap();
        let Item::Function(function) = &expanded.items[0] else {
            panic!("missing function")
        };
        assert!(matches!(
            &function.body[0],
            Stmt::Return(Some(Expr::Binary(..)))
        ));
    }
    #[test]
    fn ident_macro_creates_scoped_declaration() {
        let expanded=compile("macro bind($name:ident,$value:expr) => {let $name=$value;} fn main()->i32 {bind!(total,42);return 0;}").unwrap();
        let Item::Function(function) = &expanded.items[0] else {
            panic!("missing function")
        };
        let Stmt::If { then_body, .. } = &function.body[0] else {
            panic!("macro output must be scoped")
        };
        assert!(matches!(&then_body[0],Stmt::Let{name,..} if name=="total"));
    }
    #[test]
    fn fragments_are_validated() {
        let error =
            compile("macro only_name($name:ident) => $name; fn main()->i32{return only_name!(3);}")
                .unwrap_err();
        assert!(error.contains("expects AST fragment Ident"), "{error}");
    }
    #[test]
    fn rust_style_multiple_arms_match_ast_fragments_in_order() {
        let program = compile(
            r#"
            macro_rules! choose {
                ($name:ident) => $name;
                ($value:literal) => $value + 1;
            }
            fn main()->i32 {
                let variable=10;
                let a=choose!(variable);
                let b=choose!(41);
                return a+b;
            }
        "#,
        )
        .unwrap();
        let Item::Function(main) = &program.items[0] else {
            panic!("missing main")
        };
        assert!(matches!(&main.body[1],Stmt::Let{init:Expr::Var(name),..} if name=="variable"));
        assert!(matches!(
            &main.body[2],
            Stmt::Let {
                init: Expr::Binary(..),
                ..
            }
        ));
    }
    #[test]
    fn zero_or_more_splices_array_ast_not_source_text() {
        let program = compile(
            r#"
            macro_rules! make_array {
                ($($value:expr),*) => [$($value),*];
            }
            fn main()->i32 {
                let empty=make_array!();
                let values=make_array!(1, 2, 3);
                return values[1];
            }
        "#,
        )
        .unwrap();
        let Item::Function(main) = &program.items[0] else {
            panic!("missing main")
        };
        assert!(
            matches!(&main.body[0],Stmt::Let{init:Expr::Array(values),..} if values.is_empty())
        );
        assert!(matches!(&main.body[1],Stmt::Let{init:Expr::Array(values),..} if values.len()==3));
    }
    #[test]
    fn one_or_more_and_optional_capture_enforce_arity() {
        let error = compile(
            r#"
            macro_rules! required { ($($x:expr),+) => [$($x),+]; }
            fn main()->i32 { let xs=required!(); return 0; }
        "#,
        )
        .unwrap_err();
        assert!(error.contains("NOVA-MAC007"), "{error}");
        let error = compile(
            r#"
            macro_rules! maybe { ($($x:expr)?) => [$($x)?]; }
            fn main()->i32 { let xs=maybe!(1,2); return 0; }
        "#,
        )
        .unwrap_err();
        assert!(error.contains("NOVA-MAC007"), "{error}");
    }
    #[test]
    fn nested_macro_forwarding_splices_ast_arguments() {
        let program = compile(
            r#"
            macro_rules! pair { ($a:expr,$b:expr) => $a + $b; }
            macro_rules! forward { ($($x:expr),+) => pair!($($x),*); }
            fn main()->i32 { return forward!(40,2); }
        "#,
        )
        .unwrap();
        let Item::Function(main) = &program.items[0] else {
            panic!("missing main")
        };
        assert!(matches!(
            &main.body[0],
            Stmt::Return(Some(Expr::Binary(..)))
        ));
    }
    #[test]
    fn statement_repetition_splices_multiple_ast_statements() {
        let program = compile(
            r#"
            macro_rules! repeat_output {
                ($($text:expr),*) => { $(std::io::println($text);)* };
            }
            fn main()->i32 { repeat_output!("one","two","three"); return 0; }
        "#,
        )
        .unwrap();
        let Item::Function(main) = &program.items[0] else {
            panic!("missing main")
        };
        let Stmt::If { then_body, .. } = &main.body[0] else {
            panic!("macro must expand to scope")
        };
        assert_eq!(then_body.len(), 3);
        assert!(then_body
            .iter()
            .all(|stmt| matches!(stmt,Stmt::Expr(Expr::Call(_,args)) if args.len()==1)));
    }
    #[test]
    fn hygienic_locals_do_not_capture_caller_ast() {
        let program = compile(
            r#"
            macro_rules! scoped {
                ($value:expr) => {
                    let temp=100;
                    let result=$value;
                };
            }
            fn main()->i32 {
                let temp=42;
                scoped!(temp);
                return temp;
            }
        "#,
        )
        .unwrap();
        let Item::Function(main) = &program.items[0] else {
            panic!("missing main")
        };
        let Stmt::If { then_body, .. } = &main.body[1] else {
            panic!("expected macro scope")
        };
        let Stmt::Let { name, .. } = &then_body[0] else {
            panic!("expected local")
        };
        assert!(name.starts_with("__nova_macro_local_"), "{name}");
        let Stmt::Let {
            init: Expr::Var(source),
            ..
        } = &then_body[1]
        else {
            panic!("expected caller binding")
        };
        assert_eq!(source, "temp");
        assert!(matches!(&main.body[2],Stmt::Return(Some(Expr::Var(name))) if name=="temp"));
    }
    #[test]
    fn hygienic_macro_callback_call_tracks_renamed_local() {
        // A macro-local function-pointer binding must be alpha-renamed both at
        // its declaration and at a direct `cb(...)` call.  Source references
        // from the caller remain unchanged by hygienic macro expansion.
        let program = compile(
            r#"
            macro_rules! invoke {
                ($callback:expr where same_as<fn(i64)->i64>) => {
                    let cb = $callback;
                    let result = cb(21);
                };
            }
            fn twice(value:i64)->i64 { return value * 2; }
            fn main()->i32 {
                let cb = 99;
                invoke!(twice);
                return cb;
            }
        "#,
        )
        .unwrap();
        let main = program
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main function is preserved");
        let Stmt::If { then_body, .. } = &main.body[1] else {
            panic!("macro expansion should introduce a lexical scope");
        };
        let Stmt::Let { name: binding, .. } = &then_body[0] else {
            panic!("expected hygienic callback declaration");
        };
        let Stmt::Let {
            init: Expr::Call(callee, args),
            ..
        } = &then_body[1]
        else {
            panic!("expected direct invocation of macro-local function pointer");
        };
        assert!(binding.starts_with("__nova_macro_local_"), "{binding}");
        assert_eq!(
            callee, binding,
            "callee must refer to alpha-renamed binding"
        );
        assert_eq!(args.len(), 1);
        assert!(matches!(&main.body[2], Stmt::Return(Some(Expr::Var(name))) if name == "cb"));
    }

    #[test]
    fn repeated_metavariable_must_be_spliced() {
        let error = compile(
            r#"
            macro_rules! invalid { ($($x:expr),*) => $x; }
            fn main()->i32 { return invalid!(1,2); }
        "#,
        )
        .unwrap_err();
        assert!(error.contains("NOVA-MAC009"), "{error}");
    }
    #[test]
    fn duplicate_macro_capture_name_is_rejected() {
        let error = compile(
            r#"
            macro_rules! invalid { ($x:expr,$x:literal) => $x; }
            fn main()->i32 { return invalid!(1,2); }
        "#,
        )
        .unwrap_err();
        assert!(error.contains("NOVA-MAC002"), "{error}");
    }
    #[test]
    fn recursion_and_arity_are_rejected() {
        assert!(
            compile("macro a(x) => a!(x); fn main()->i32{return a!(1);}")
                .unwrap_err()
                .contains("recursive")
        );
        assert!(compile("macro a(x) => x; fn main()->i32{return a!(1,2);}")
            .unwrap_err()
            .contains("expects"));
    }
}
