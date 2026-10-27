//! Lambda lowering into normal Nova functions or compiler-generated callable
//! structs. Captured variables are copied at closure creation time and exposed
//! as immutable fields. The existing HIR forbids copying owned classes into
//! these fields, so capturing an owner is rejected rather than double-freed.
use crate::ast::{self, Expr, Item, Param, Stmt, TypeRef};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Default)]
struct Lifter {
    next_id: usize,
}

type Locals = HashMap<String, TypeRef>;

fn infer_type(value: &Expr) -> Option<TypeRef> {
    let name = match value {
        Expr::Int(raw) => {
            if raw
                .parse::<i64>()
                .ok()
                .is_some_and(|number| number > i32::MAX as i64)
            {
                "i64"
            } else {
                "i32"
            }
        }
        Expr::Float(_) => "f64",
        Expr::Bool(_) => "bool",
        Expr::Str(_) => "str",
        Expr::StructInit(name, _) => return Some(TypeRef::Named(name.clone())),
        Expr::GenericStructInit(name, params, _) => {
            return Some(TypeRef::Applied(name.clone(), params.clone()))
        }
        Expr::Cast(_, target) => return Some(target.clone()),
        Expr::FunctionRef(_) => return None,
        _ => return None,
    };
    Some(TypeRef::Named(name.into()))
}

fn capture_expression(
    expression: &Expr,
    outer: &Locals,
    bound: &HashSet<String>,
    captures: &mut BTreeMap<String, TypeRef>,
) {
    match expression {
        Expr::Var(name) if !bound.contains(name) => {
            if let Some(ty) = outer.get(name) {
                captures.insert(name.clone(), ty.clone());
            }
        }
        Expr::Call(name, args) => {
            if !bound.contains(name) {
                if let Some(ty) = outer.get(name) {
                    captures.insert(name.clone(), ty.clone());
                }
            }
            for value in args {
                capture_expression(value, outer, bound, captures);
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut nested = bound.clone();
            for param in params {
                nested.insert(param.name.clone());
            }
            capture_statements(body, outer, &mut nested, captures);
        }
        Expr::Binary(left, _, right) | Expr::Index(left, right) => {
            capture_expression(left, outer, bound, captures);
            capture_expression(right, outer, bound, captures);
        }
        Expr::CallIndirect(callee, args) => {
            capture_expression(callee, outer, bound, captures);
            for value in args {
                capture_expression(value, outer, bound, captures);
            }
        }
        Expr::GenericCall(_, _, args) | Expr::MacroCall(_, args) | Expr::Array(args) => {
            for value in args {
                capture_expression(value, outer, bound, captures);
            }
        }
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
            for (_, value) in fields {
                capture_expression(value, outer, bound, captures);
            }
        }
        Expr::MethodCall(receiver, _, args) => {
            capture_expression(receiver, outer, bound, captures);
            for value in args {
                capture_expression(value, outer, bound, captures);
            }
        }
        Expr::Unary(_, value)
        | Expr::Deref(value)
        | Expr::Spread(value)
        | Expr::Yield(value)
        | Expr::Await(value)
        | Expr::Cast(value, _)
        | Expr::Field(value, _)
        | Expr::AddressOf { value, .. } => capture_expression(value, outer, bound, captures),
        _ => {}
    }
}
fn capture_statements(
    statements: &[Stmt],
    outer: &Locals,
    bound: &mut HashSet<String>,
    captures: &mut BTreeMap<String, TypeRef>,
) {
    for statement in statements {
        match statement {
            Stmt::Let {
                name, init, defer, ..
            } => {
                capture_expression(init, outer, bound, captures);
                if let Some(cleanup) = defer {
                    capture_expression(cleanup, outer, bound, captures);
                }
                bound.insert(name.clone());
            }
            Stmt::Set { name, value } | Stmt::CompoundSet { name, value, .. } => {
                if !bound.contains(name) {
                    if let Some(ty) = outer.get(name) {
                        captures.insert(name.clone(), ty.clone());
                    }
                }
                capture_expression(value, outer, bound, captures);
            }
            Stmt::DerefSet { name, value } => {
                if !bound.contains(name) {
                    if let Some(ty) = outer.get(name) {
                        captures.insert(name.clone(), ty.clone());
                    }
                }
                capture_expression(value, outer, bound, captures);
            }
            Stmt::FieldSet { root, value, .. } => {
                if !bound.contains(root) {
                    if let Some(ty) = outer.get(root) {
                        captures.insert(root.clone(), ty.clone());
                    }
                }
                capture_expression(value, outer, bound, captures);
            }
            Stmt::Defer(value) | Stmt::Expr(value) | Stmt::Return(Some(value)) => {
                capture_expression(value, outer, bound, captures)
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                capture_expression(condition, outer, bound, captures);
                capture_statements(then_body, outer, &mut bound.clone(), captures);
                if let Some(body) = else_body {
                    capture_statements(body, outer, &mut bound.clone(), captures);
                }
            }
            Stmt::While { condition, body } => {
                capture_expression(condition, outer, bound, captures);
                capture_statements(body, outer, &mut bound.clone(), captures);
            }
            Stmt::For {
                name,
                start,
                end,
                body,
            } => {
                capture_expression(start, outer, bound, captures);
                capture_expression(end, outer, bound, captures);
                let mut nested = bound.clone();
                nested.insert(name.clone());
                capture_statements(body, outer, &mut nested, captures);
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
            } => {
                capture_expression(iterable, outer, bound, captures);
                let mut nested = bound.clone();
                nested.insert(name.clone());
                capture_statements(body, outer, &mut nested, captures);
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => {
                capture_statements(body, outer, &mut bound.clone(), captures)
            }
            Stmt::Match { value, arms } => {
                capture_expression(value, outer, bound, captures);
                for arm in arms {
                    capture_statements(&arm.body, outer, &mut bound.clone(), captures);
                }
            }
            _ => {}
        }
    }
}

fn rewrite_expression(
    expression: &mut Expr,
    captures: &BTreeMap<String, TypeRef>,
    bound: &HashSet<String>,
) {
    match expression {
        Expr::Var(name) if captures.contains_key(name) && !bound.contains(name) => {
            *expression = Expr::Field(Box::new(Expr::Var("self".into())), name.clone());
        }
        Expr::Call(name, arguments) => {
            for argument in arguments.iter_mut() {
                rewrite_expression(argument, captures, bound);
            }
            if captures.contains_key(name) && !bound.contains(name) {
                let field = Expr::Field(Box::new(Expr::Var("self".into())), name.clone());
                let captured_arguments = std::mem::take(arguments);
                *expression = if matches!(captures.get(name), Some(TypeRef::Function { .. })) {
                    Expr::CallIndirect(Box::new(field), captured_arguments)
                } else {
                    Expr::MethodCall(Box::new(field), "call".into(), captured_arguments)
                };
            }
        }
        Expr::Lambda { .. } => {} // Inner lambdas are lifted before the enclosing lambda.
        Expr::Binary(left, _, right) | Expr::Index(left, right) => {
            rewrite_expression(left, captures, bound);
            rewrite_expression(right, captures, bound);
        }
        Expr::CallIndirect(callee, values) => {
            rewrite_expression(callee, captures, bound);
            for value in values {
                rewrite_expression(value, captures, bound);
            }
        }
        Expr::MacroCall(_, values) | Expr::GenericCall(_, _, values) | Expr::Array(values) => {
            for value in values {
                rewrite_expression(value, captures, bound);
            }
        }
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
            for (_, value) in fields {
                rewrite_expression(value, captures, bound);
            }
        }
        Expr::MethodCall(receiver, _, arguments) => {
            rewrite_expression(receiver, captures, bound);
            for value in arguments {
                rewrite_expression(value, captures, bound);
            }
        }
        Expr::Unary(_, value)
        | Expr::Deref(value)
        | Expr::Spread(value)
        | Expr::Yield(value)
        | Expr::Await(value)
        | Expr::Cast(value, _)
        | Expr::Field(value, _)
        | Expr::AddressOf { value, .. } => rewrite_expression(value, captures, bound),
        _ => {}
    }
}
fn rewrite_statements(
    statements: &mut [Stmt],
    captures: &BTreeMap<String, TypeRef>,
    bound: &mut HashSet<String>,
) -> Result<(), String> {
    for statement in statements {
        match statement {
            Stmt::Let {
                name, init, defer, ..
            } => {
                rewrite_expression(init, captures, bound);
                if let Some(cleanup) = defer {
                    rewrite_expression(cleanup, captures, bound);
                }
                bound.insert(name.clone());
            }
            Stmt::Set { name, value } | Stmt::CompoundSet { name, value, .. } => {
                if captures.contains_key(name) && !bound.contains(name) {
                    return Err(format!("cannot assign to captured variable '{name}'; closures capture immutable values"));
                }
                rewrite_expression(value, captures, bound);
            }
            Stmt::DerefSet { name, value } => {
                if captures.contains_key(name) && !bound.contains(name) {
                    return Err(format!("cannot mutate through captured reference '{name}'"));
                }
                rewrite_expression(value, captures, bound);
            }
            Stmt::FieldSet { root, value, .. } => {
                if captures.contains_key(root) && !bound.contains(root) {
                    return Err(format!(
                        "cannot mutate captured variable '{root}'; capture is immutable"
                    ));
                }
                rewrite_expression(value, captures, bound);
            }
            Stmt::Defer(value) | Stmt::Expr(value) | Stmt::Return(Some(value)) => {
                rewrite_expression(value, captures, bound)
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                rewrite_expression(condition, captures, bound);
                rewrite_statements(then_body, captures, &mut bound.clone())?;
                if let Some(body) = else_body {
                    rewrite_statements(body, captures, &mut bound.clone())?;
                }
            }
            Stmt::While { condition, body } => {
                rewrite_expression(condition, captures, bound);
                rewrite_statements(body, captures, &mut bound.clone())?;
            }
            Stmt::For {
                name,
                start,
                end,
                body,
            } => {
                rewrite_expression(start, captures, bound);
                rewrite_expression(end, captures, bound);
                let mut nested = bound.clone();
                nested.insert(name.clone());
                rewrite_statements(body, captures, &mut nested)?;
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
            } => {
                rewrite_expression(iterable, captures, bound);
                let mut nested = bound.clone();
                nested.insert(name.clone());
                rewrite_statements(body, captures, &mut nested)?;
            }
            Stmt::Loop(body) | Stmt::Unsafe(body) => {
                rewrite_statements(body, captures, &mut bound.clone())?
            }
            Stmt::Match { value, arms } => {
                rewrite_expression(value, captures, bound);
                for arm in arms {
                    rewrite_statements(&mut arm.body, captures, &mut bound.clone())?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
impl Lifter {
    fn expr(
        &mut self,
        expression: &mut Expr,
        generated: &mut Vec<Item>,
        locals: &Locals,
    ) -> Result<(), String> {
        match expression {
            Expr::Lambda {
                params,
                result,
                body,
            } => {
                let mut nested_locals = locals.clone();
                for param in params.iter() {
                    nested_locals.insert(param.name.clone(), param.ty.clone());
                }
                self.statements(body, generated, &mut nested_locals)?;

                let mut bound = params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect::<HashSet<_>>();
                let mut captures = BTreeMap::new();
                capture_statements(body, locals, &mut bound, &mut captures);
                let id = self.next_id;
                self.next_id += 1;
                if captures.is_empty() {
                    let name = format!("__nova_lambda_{id}");
                    generated.push(Item::Function(ast::Function {
                        mode: ast::FunctionMode::Normal,
                        name: name.clone(),
                        generics: vec![],
                        concept_bounds: vec![],
                        params: params.clone(),
                        variadic: false,
                        result: result.clone(),
                        body: body.clone(),
                        is_virtual: false,
                        is_override: false,
                        attributes: vec![],
                    }));
                    *expression = Expr::FunctionRef(name);
                } else {
                    let closure_name = format!("__nova_closure_{id}");
                    if params.iter().any(|param| param.name == "self") {
                        return Err(
                            "capturing closures reserve 'self' for their captured environment"
                                .into(),
                        );
                    }
                    let captured_fields = captures
                        .iter()
                        .map(|(name, ty)| ast::Field {
                            name: name.clone(),
                            ty: ty.clone(),
                        })
                        .collect();
                    generated.push(Item::Struct(ast::Struct {
                        name: closure_name.clone(),
                        generics: vec![],
                        fields: captured_fields,
                        attributes: vec![],
                        is_class: false,
                        parent: None,
                    }));
                    let mut method_body = body.clone();
                    let mut initial = params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<HashSet<_>>();
                    rewrite_statements(&mut method_body, &captures, &mut initial)?;
                    let self_param = Param {
                        name: "self".into(),
                        ty: TypeRef::Named(closure_name.clone()),
                        default: None,
                    };
                    let method_params = std::iter::once(self_param)
                        .chain(params.iter().cloned())
                        .collect();
                    generated.push(Item::Impl(ast::Impl {
                        generics: vec![],
                        trait_name: None,
                        target: TypeRef::Named(closure_name.clone()),
                        associated_types: vec![],
                        methods: vec![ast::Function {
                            mode: ast::FunctionMode::Normal,
                            name: "call".into(),
                            generics: vec![],
                            concept_bounds: vec![],
                            params: method_params,
                            variadic: false,
                            result: result.clone(),
                            body: method_body,
                            is_virtual: false,
                            is_override: false,
                            attributes: vec![],
                        }],
                    }));
                    *expression = Expr::StructInit(
                        closure_name,
                        captures
                            .keys()
                            .map(|name| (name.clone(), Expr::Var(name.clone())))
                            .collect(),
                    );
                }
            }
            Expr::CallIndirect(callee, values) => {
                self.expr(callee, generated, locals)?;
                for value in values {
                    self.expr(value, generated, locals)?;
                }
            }
            Expr::Call(_, values)
            | Expr::MacroCall(_, values)
            | Expr::GenericCall(_, _, values)
            | Expr::Array(values) => {
                for value in values {
                    self.expr(value, generated, locals)?;
                }
            }
            Expr::MethodCall(receiver, _, values) => {
                self.expr(receiver, generated, locals)?;
                for value in values {
                    self.expr(value, generated, locals)?;
                }
            }
            Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
                for (_, value) in fields {
                    self.expr(value, generated, locals)?;
                }
            }
            Expr::Binary(left, _, right) | Expr::Index(left, right) => {
                self.expr(left, generated, locals)?;
                self.expr(right, generated, locals)?;
            }
            Expr::Unary(_, value)
            | Expr::Deref(value)
            | Expr::Spread(value)
            | Expr::Yield(value)
            | Expr::Await(value)
            | Expr::Cast(value, _)
            | Expr::Field(value, _)
            | Expr::AddressOf { value, .. } => self.expr(value, generated, locals)?,
            _ => {}
        }
        Ok(())
    }
    fn statements(
        &mut self,
        statements: &mut [Stmt],
        generated: &mut Vec<Item>,
        locals: &mut Locals,
    ) -> Result<(), String> {
        for statement in statements {
            match statement {
                Stmt::Let {
                    name,
                    ty,
                    init,
                    defer,
                    ..
                } => {
                    self.expr(init, generated, locals)?;
                    let variable_type = ty.clone().or_else(|| infer_type(init));
                    if let Some(value) = variable_type {
                        locals.insert(name.clone(), value);
                    }
                    if let Some(value) = defer {
                        self.expr(value, generated, locals)?;
                    }
                }
                Stmt::Defer(value)
                | Stmt::Set { value, .. }
                | Stmt::CompoundSet { value, .. }
                | Stmt::FieldSet { value, .. }
                | Stmt::DerefSet { value, .. }
                | Stmt::Expr(value)
                | Stmt::Return(Some(value)) => self.expr(value, generated, locals)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.expr(condition, generated, locals)?;
                    self.statements(then_body, generated, &mut locals.clone())?;
                    if let Some(body) = else_body {
                        self.statements(body, generated, &mut locals.clone())?;
                    }
                }
                Stmt::While { condition, body } => {
                    self.expr(condition, generated, locals)?;
                    self.statements(body, generated, &mut locals.clone())?;
                }
                Stmt::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    self.expr(start, generated, locals)?;
                    self.expr(end, generated, locals)?;
                    let mut nested = locals.clone();
                    if let Some(value) = infer_type(start) {
                        nested.insert(name.clone(), value);
                    }
                    self.statements(body, generated, &mut nested)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    self.expr(iterable, generated, locals)?;
                    self.statements(body, generated, &mut locals.clone())?;
                }
                Stmt::Loop(body) | Stmt::Unsafe(body) => {
                    self.statements(body, generated, &mut locals.clone())?
                }
                Stmt::Match { value, arms } => {
                    self.expr(value, generated, locals)?;
                    for arm in arms {
                        self.statements(&mut arm.body, generated, &mut locals.clone())?;
                    }
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }
    fn items(&mut self, items: Vec<Item>) -> Result<Vec<Item>, String> {
        let mut output = Vec::new();
        let mut generated = Vec::new();
        for mut item in items {
            match &mut item {
                Item::Namespace { items, .. } => {
                    *items = self.items(std::mem::take(items))?;
                }
                Item::Function(function) => {
                    let mut locals = function
                        .params
                        .iter()
                        .map(|p| (p.name.clone(), p.ty.clone()))
                        .collect();
                    self.statements(&mut function.body, &mut generated, &mut locals)?;
                }
                Item::Impl(block) => {
                    for method in &mut block.methods {
                        let mut locals = method
                            .params
                            .iter()
                            .map(|p| (p.name.clone(), p.ty.clone()))
                            .collect();
                        self.statements(&mut method.body, &mut generated, &mut locals)?;
                    }
                }
                Item::Public(inner) => {
                    let mut expanded = self.items(vec![*inner.clone()])?;
                    if !expanded.is_empty() {
                        *inner = Box::new(expanded.remove(0));
                        output.extend(expanded);
                    }
                }
                _ => {}
            }
            output.push(item);
        }
        output.extend(generated);
        Ok(output)
    }
}
pub fn lift(program: ast::Program) -> Result<ast::Program, String> {
    let mut lifter = Lifter::default();
    Ok(ast::Program {
        items: lifter.items(program.items)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    #[test]
    fn capture_is_encoded_as_struct_with_callable_method() {
        let parsed=grammar::ProgramParser::new().parse("fn run()->i32{let increment:i32=2;let add=fn(value:i32)->i32{return value+increment;};return add(40);}").unwrap();
        let transformed = lift(parsed).unwrap();
        assert!(transformed
            .items
            .iter()
            .any(|item| matches!(item, Item::Struct(_))));
        assert!(transformed
            .items
            .iter()
            .any(|item| matches!(item, Item::Impl(_))));
    }
    #[test]
    fn noncapturing_lambda_remains_a_function_pointer() {
        let parsed = grammar::ProgramParser::new()
            .parse("fn run()->i32{let f=fn(value:i32)->i32{return value+1;};return f(41);}")
            .unwrap();
        let transformed = lift(parsed).unwrap();
        assert!(transformed
            .items
            .iter()
            .any(|item| matches!(item,Item::Function(f) if f.name.starts_with("__nova_lambda_"))));
    }
}
