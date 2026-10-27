//! Bounded compile-time evaluation of integer `const fn` calls.
//! Operates on the source AST after macro expansion and effect checking. Only
//! calls whose arguments are numeric constants are folded; other calls remain
//! ordinary runtime functions. Unsupported operations are not silently executed.
use crate::ast::{self, BinaryOp, Expr, Function, Item, Stmt, UnaryOp};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
enum Value {
    Int(i64),
    Bool(bool),
}
impl Value {
    fn int(self) -> Result<i64, String> {
        match self {
            Self::Int(x) => Ok(x),
            _ => Err("const expression requires integer".into()),
        }
    }
    fn bool(self) -> Result<bool, String> {
        match self {
            Self::Bool(x) => Ok(x),
            _ => Err("const expression requires boolean".into()),
        }
    }
}
fn q(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.into()
    } else {
        format!("{ns}::{name}")
    }
}
#[derive(Default)]
struct Engine {
    functions: HashMap<String, (Function, String)>,
    steps: usize,
}
impl Engine {
    fn collect(&mut self, items: &[Item], ns: &str) {
        for item in items {
            match item {
                Item::Public(inner) => self.collect(std::slice::from_ref(inner.as_ref()), ns),
                Item::Namespace { name, items } => self.collect(items, &q(ns, name)),
                Item::Function(f) if f.attributes.iter().any(|a| a.name == "const") => {
                    self.functions
                        .insert(q(ns, &f.name), (f.clone(), ns.into()));
                }
                _ => {}
            }
        }
    }
    fn function(&self, name: &str, ns: &str) -> Option<(Function, String)> {
        if name.contains("::") {
            return self.functions.get(name).cloned();
        }
        let mut here = ns;
        loop {
            if let Some(f) = self.functions.get(&q(here, name)) {
                return Some(f.clone());
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
    fn consume(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > 100000 {
            return Err("const fn exceeded evaluation budget (100000 steps)".into());
        }
        Ok(())
    }
    fn evaluate(
        &mut self,
        fn_name: &str,
        ns: &str,
        args: Vec<Value>,
        depth: usize,
    ) -> Result<Value, String> {
        if depth > 64 {
            return Err("const fn recursion depth exceeds 64".into());
        }
        let (function, fn_ns) = self
            .function(fn_name, ns)
            .ok_or_else(|| format!("unknown const function '{fn_name}'"))?;
        if !function.generics.is_empty() {
            return Err(
                "const fn generics require const monomorphization (not yet supported)".into(),
            );
        }
        if args.len() != function.params.len() {
            return Err(format!("const fn '{fn_name}' argument count mismatch"));
        }
        let mut locals = function
            .params
            .iter()
            .map(|p| p.name.clone())
            .zip(args)
            .collect::<HashMap<_, _>>();
        self.block(&function.body, &fn_ns, &mut locals, depth)?
            .ok_or_else(|| format!("const fn '{fn_name}' did not return a value"))
    }
    fn expr(
        &mut self,
        expr: &Expr,
        ns: &str,
        vars: &HashMap<String, Value>,
        depth: usize,
    ) -> Result<Value, String> {
        self.consume()?;
        match expr {
            Expr::Int(n)=>n.parse::<i64>().map(Value::Int).map_err(|_|format!("invalid const integer {n}")),
            Expr::Bool(b)=>Ok(Value::Bool(*b)),
            Expr::Var(name)=>vars.get(name).copied().ok_or_else(||format!("non-constant variable '{name}'")),
            Expr::Unary(op,inner)=>{
                let val=self.expr(inner,ns,vars,depth)?;
                match op {
                    UnaryOp::Neg=>val.int()?.checked_neg().map(Value::Int).ok_or("const integer overflow".into()),
                    UnaryOp::Not=>Ok(Value::Bool(!val.bool()?)),
                    UnaryOp::BitNot=>Ok(Value::Int(!val.int()?)),
                }
            },
            Expr::Binary(left,op,right)=>{
                let a=self.expr(left,ns,vars,depth)?;
                match op {
                    BinaryOp::And if !a.bool()?=>return Ok(Value::Bool(false)),
                    BinaryOp::Or if a.bool()?=>return Ok(Value::Bool(true)),
                    _=>{},
                }
                let b=self.expr(right,ns,vars,depth)?;
                let arithmetic=|value:Option<i64>|value.map(Value::Int).ok_or_else(||"const integer overflow/division by zero".into());
                match op {
                    BinaryOp::Add=>arithmetic(a.int()?.checked_add(b.int()?)),
                    BinaryOp::Sub=>arithmetic(a.int()?.checked_sub(b.int()?)),
                    BinaryOp::Mul=>arithmetic(a.int()?.checked_mul(b.int()?)),
                    BinaryOp::Div=>arithmetic(a.int()?.checked_div(b.int()?)),
                    BinaryOp::Rem=>arithmetic(a.int()?.checked_rem(b.int()?)),
                    BinaryOp::Eq=>Ok(Value::Bool(match (a,b){(Value::Int(a),Value::Int(b))=>a==b,(Value::Bool(a),Value::Bool(b))=>a==b,_=>false})),
                    BinaryOp::Ne=>Ok(Value::Bool(match (a,b){(Value::Int(a),Value::Int(b))=>a!=b,(Value::Bool(a),Value::Bool(b))=>a!=b,_=>true})),
                    BinaryOp::Lt=>Ok(Value::Bool(a.int()?<b.int()?)),
                    BinaryOp::Le=>Ok(Value::Bool(a.int()?<=b.int()?)),
                    BinaryOp::Gt=>Ok(Value::Bool(a.int()?>b.int()?)),
                    BinaryOp::Ge=>Ok(Value::Bool(a.int()?>=b.int()?)),
                    BinaryOp::And=>Ok(Value::Bool(a.bool()?&&b.bool()?)),
                    BinaryOp::Or=>Ok(Value::Bool(a.bool()?||b.bool()?)),
                    BinaryOp::BitAnd=>Ok(Value::Int(a.int()? & b.int()?)),
                    BinaryOp::BitOr=>Ok(Value::Int(a.int()? | b.int()?)),
                    BinaryOp::BitXor=>Ok(Value::Int(a.int()? ^ b.int()?)),
                    BinaryOp::Shl=>arithmetic(a.int()?.checked_shl(u32::try_from(b.int()?).unwrap_or(u32::MAX))),
                    BinaryOp::Shr=>arithmetic(a.int()?.checked_shr(u32::try_from(b.int()?).unwrap_or(u32::MAX))),
                }
            },
            Expr::Call(name,args)=>{
                let mut values=Vec::new();
                for arg in args {values.push(self.expr(arg,ns,vars,depth)?);}
                self.evaluate(name,ns,values,depth+1)
            },
            _=>Err("const fn evaluator supports local integers, booleans, arithmetic, if/return and calls to const fn".into()),
        }
    }
    fn block(
        &mut self,
        body: &[Stmt],
        ns: &str,
        vars: &mut HashMap<String, Value>,
        depth: usize,
    ) -> Result<Option<Value>, String> {
        for stmt in body {
            self.consume()?;
            match stmt {
                Stmt::Let{name,init,..}=>{let v=self.expr(init,ns,vars,depth)?;vars.insert(name.clone(),v);},
                Stmt::Set{name,value}=>{
                    if !vars.contains_key(name){return Err(format!("undefined const variable '{name}'"));}
                    let v=self.expr(value,ns,vars,depth)?;vars.insert(name.clone(),v);
                },
                Stmt::Return(Some(e))=>return self.expr(e,ns,vars,depth).map(Some),
                Stmt::If{condition,then_body,else_body}=>{
                    let cond=self.expr(condition,ns,vars,depth)?.bool()?;
                    let block=if cond {Some(then_body)}else{else_body.as_ref()};
                    if let Some(body)=block {
                        let mut inner=vars.clone();
                        if let Some(result)=self.block(body,ns,&mut inner,depth)?{return Ok(Some(result));}
                    }
                },
                Stmt::Expr(expr)=>{self.expr(expr,ns,vars,depth)?;},
                _=>return Err("const fn evaluator: unsupported statement (loops and unsafe operations are not const)".into()),
            }
        }
        Ok(None)
    }
    fn fold_expr(&mut self, expr: &mut Expr, ns: &str) -> Result<(), String> {
        match expr {
            Expr::Call(_, args)
            | Expr::GenericCall(_, _, args)
            | Expr::MacroCall(_, args)
            | Expr::Array(args) => {
                for value in args {
                    self.fold_expr(value, ns)?;
                }
            }
            Expr::MethodCall(receiver, _, args) => {
                self.fold_expr(receiver, ns)?;
                for arg in args {
                    self.fold_expr(arg, ns)?;
                }
            }
            Expr::CallIndirect(callee, args) => {
                self.fold_expr(callee, ns)?;
                for arg in args {
                    self.fold_expr(arg, ns)?;
                }
            }
            Expr::Lambda { body, .. } => self.fold_stmts(body, ns)?,
            Expr::Binary(a, _, b) | Expr::Index(a, b) => {
                self.fold_expr(a, ns)?;
                self.fold_expr(b, ns)?;
            }
            Expr::Unary(_, v)
            | Expr::Deref(v)
            | Expr::Spread(v)
            | Expr::Yield(v)
            | Expr::Await(v)
            | Expr::Field(v, _)
            | Expr::AddressOf { value: v, .. }
            | Expr::Cast(v, _) => self.fold_expr(v, ns)?,
            Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
                for (_, value) in fields {
                    self.fold_expr(value, ns)?;
                }
            }
            _ => {}
        }
        if let Expr::Call(name, args) = expr {
            if self.function(name, ns).is_some()
                && args
                    .iter()
                    .all(|arg| matches!(arg, Expr::Int(_) | Expr::Bool(_) | Expr::Unary(..)))
            {
                let locals = HashMap::new();
                let values = args
                    .iter()
                    .map(|arg| self.expr(arg, ns, &locals, 0))
                    .collect::<Result<Vec<_>, _>>()?;
                self.steps = 0;
                let result = self
                    .evaluate(name, ns, values, 0)
                    .map_err(|e| format!("const fn '{name}' evaluation failed: {e}"))?;
                *expr = match result {
                    Value::Bool(b) => Expr::Bool(b),
                    Value::Int(x) if x >= 0 => Expr::Int(x.to_string()),
                    Value::Int(x) => Expr::Unary(
                        UnaryOp::Neg,
                        Box::new(Expr::Int(x.unsigned_abs().to_string())),
                    ),
                };
            }
        }
        Ok(())
    }
    fn fold_stmts(&mut self, stmts: &mut [Stmt], ns: &str) -> Result<(), String> {
        for stmt in stmts {
            match stmt {
                Stmt::Let { init, defer, .. } => {
                    self.fold_expr(init, ns)?;
                    if let Some(e) = defer {
                        self.fold_expr(e, ns)?;
                    }
                }
                Stmt::Defer(e)
                | Stmt::Set { value: e, .. }
                | Stmt::DerefSet { value: e, .. }
                | Stmt::CompoundSet { value: e, .. }
                | Stmt::FieldSet { value: e, .. }
                | Stmt::Return(Some(e))
                | Stmt::Expr(e) => self.fold_expr(e, ns)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.fold_expr(condition, ns)?;
                    self.fold_stmts(then_body, ns)?;
                    if let Some(body) = else_body {
                        self.fold_stmts(body, ns)?;
                    }
                }
                Stmt::While { condition, body } => {
                    self.fold_expr(condition, ns)?;
                    self.fold_stmts(body, ns)?;
                }
                Stmt::For {
                    start, end, body, ..
                } => {
                    self.fold_expr(start, ns)?;
                    self.fold_expr(end, ns)?;
                    self.fold_stmts(body, ns)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    self.fold_expr(iterable, ns)?;
                    self.fold_stmts(body, ns)?;
                }
                Stmt::Loop(body) | Stmt::Unsafe(body) => self.fold_stmts(body, ns)?,
                Stmt::Match { value, arms } => {
                    self.fold_expr(value, ns)?;
                    for arm in arms {
                        self.fold_stmts(&mut arm.body, ns)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn items(&mut self, items: &mut [Item], ns: &str) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => self.items(std::slice::from_mut(inner.as_mut()), ns)?,
                Item::Namespace { name, items } => self.items(items, &q(ns, name))?,
                Item::Function(f) => self.fold_stmts(&mut f.body, ns)?,
                Item::Impl(block) => {
                    for f in &mut block.methods {
                        self.fold_stmts(&mut f.body, ns)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
pub fn fold(program: &ast::Program) -> Result<ast::Program, String> {
    let mut engine = Engine::default();
    engine.collect(&program.items, "");
    let mut output = program.clone();
    engine.items(&mut output.items, "")?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn source(src: &str) -> Result<ast::Program, String> {
        let program = grammar::ProgramParser::new()
            .parse(src)
            .map_err(|e| format!("{e:?}"))?;
        fold(&program)
    }
    #[test]
    fn factorial_is_folded_at_compile_time() {
        let result=source("const fn fact(n:i64)->i64 {if n<=1{return 1;}return n*fact(n-1);} fn main()->i32{return fact(5) as i32;}").unwrap();
        let function = result
            .items
            .iter()
            .find_map(|i| match i {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .unwrap();
        assert!(format!("{:?}", function.body).contains("120"));
    }
    #[test]
    fn recursion_is_bounded() {
        let error=source("const fn infinite(n:i64)->i64{return infinite(n);} fn main()->i32{return infinite(1) as i32;}").unwrap_err();
        assert!(error.contains("recursion depth"), "{error}");
    }
}
