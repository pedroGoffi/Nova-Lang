//! Deterministic, bounded compile-time evaluation and read-only type reflection.
//! Operates on the source AST after macro expansion and effect checking. Only
//! calls whose arguments are numeric constants are folded; other calls remain
//! ordinary runtime functions. Unsupported operations are not silently executed.
use crate::ast::{self, BinaryOp, Expr, Function, Item, Stmt, UnaryOp};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
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
    fn str(self) -> Result<String, String> {
        match self {
            Self::Str(x) => Ok(x),
            _ => Err("compile-time reflection field name must be a string".into()),
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
    types: HashMap<String, ast::Struct>,
    enums: HashMap<String, ast::Enum>,
    intrinsics: HashMap<String, String>,
    assertions: Vec<(String, String)>,
    /// Deterministic in-process memoization for a fully typed CTFE call.
    cache: HashMap<(String,Vec<Value>),Value>,
}
impl Engine {
    fn collect(&mut self, items: &[Item], ns: &str) {
        for item in items {
            match item {
                Item::Public(inner) => self.collect(std::slice::from_ref(inner.as_ref()), ns),
                Item::Namespace { name, items } => self.collect(items, &q(ns, name)),
                Item::Struct(decl) => {
                    self.types.insert(q(ns, &decl.name),decl.clone());
                }
                Item::Enum(decl) => {
                    self.enums.insert(q(ns,&decl.name),decl.clone());
                }
                Item::Function(f) => {
                    let key=q(ns,&f.name);
                    for attribute in &f.attributes {
                        if attribute.name=="meta_intrinsic" {
                            if ns!="std::meta" || attribute.args.len()!=1 ||
                                !matches!(attribute.args[0].as_str(),"field_count"|"has_field"|"type_name"|"field_type"|"type_kind"|"variant_count"|"has_variant") {
                                // Invalid declarations are rejected by verify_intrinsics.
                                self.intrinsics.insert(key.clone(), "invalid".into());
                            } else {
                                self.intrinsics.insert(key.clone(),attribute.args[0].clone());
                            }
                        }
                        if attribute.name=="comptime_assert" {
                            self.assertions.push((key.clone(),ns.into()));
                        }
                    }
                    if f.attributes.iter().any(|a| matches!(a.name.as_str(),"const"|"comptime"|"comptime_assert")) {
                        self.functions.insert(key,(f.clone(),ns.into()));
                    }
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
    fn verify_intrinsics(&self) -> Result<(),String> {
        for (name,kind) in &self.intrinsics {
            if kind=="invalid" { return Err(format!("error[NOVA-CTFE011]: invalid #[meta_intrinsic] declaration on '{name}'; only supported reflection primitives in std::meta are permitted")); }
        }
        Ok(())
    }
    fn assert_compile_time(&mut self) -> Result<(),String> {
        for (name,ns) in self.assertions.clone() {
            self.steps=0;
            let answer=self.evaluate(&name,&ns,vec![],0)
                .map_err(|e|format!("error[NOVA-CTFE012]: #[comptime_assert] '{name}': {e}"))?;
            match answer {
                Value::Bool(true)=>{},
                Value::Bool(false)=>return Err(format!("error[NOVA-CTFE013]: compile-time assertion '{name}' failed")),
                _=>return Err(format!("error[NOVA-CTFE012]: compile-time assertion '{name}' must return bool")),
            }
        }
        Ok(())
    }
    fn reflection(&mut self, name:&str, types:&[ast::TypeRef], arguments:Vec<Value>) -> Result<Value,String> {
        let kind=self.intrinsics.get(name).ok_or_else(||format!("unknown compile-time reflection primitive '{name}'"))?.clone();
        if types.len()!=1 { return Err(format!("error[NOVA-CTFE014]: '{name}' needs one concrete type argument")); }
        let type_name=match &types[0] {
            ast::TypeRef::Named(n) | ast::TypeRef::Applied(n,_)=>n.clone(),
            _=>return Err(format!("error[NOVA-CTFE014]: reflection requires a declared struct type, not {:?}",types[0])),
        };
        if matches!(kind.as_str(),"variant_count"|"has_variant"|"type_kind"|"type_name") {
            let enum_entry=self.enums.get(&type_name);
            let struct_entry=self.types.get(&type_name);
            if kind=="type_kind" && arguments.is_empty() {
                return match (struct_entry,enum_entry) {
                    (Some(_),None)=>Ok(Value::Str("struct".into())),
                    (None,Some(_))=>Ok(Value::Str("enum".into())),
                    _=>Err(format!("error[NOVA-CTFE014]: unknown or ambiguous reflected type '{type_name}'")),
                };
            }
            if kind=="type_name" && arguments.is_empty() && (struct_entry.is_some() || enum_entry.is_some()) {
                return Ok(Value::Str(type_name));
            }
            if kind=="variant_count" && arguments.is_empty() {
                return Ok(Value::Int(enum_entry.ok_or_else(||format!("error[NOVA-CTFE014]: '{type_name}' is not an enum"))?.variants.len() as i64));
            }
            if kind=="has_variant" && arguments.len()==1 {
                let target=arguments.into_iter().next().unwrap().str()?;
                return Ok(Value::Bool(enum_entry.ok_or_else(||format!("error[NOVA-CTFE014]: '{type_name}' is not an enum"))?.variants.iter().any(|v|v.name==target)));
            }
        }
        let entry=if let Some(exact)=self.types.get(&type_name) {
            exact
        } else {
            let matches=self.types.iter().filter(|(name,_)|name.rsplit("::").next()==Some(type_name.as_str())).collect::<Vec<_>>();
            match matches.as_slice() {
                [(_,single)]=>*single,
                []=>return Err(format!("error[NOVA-CTFE014]: no reflected struct '{type_name}'")),
                _=>return Err(format!("error[NOVA-CTFE014]: ambiguous reflected type '{type_name}'; use its qualified name")),
            }
        };
        if !entry.generics.is_empty() {
            if let ast::TypeRef::Applied(_,args)=&types[0] {
                if args.len()!=entry.generics.len(){return Err(format!("error[NOVA-CTFE014]: '{type_name}' expects {} type arguments",entry.generics.len()));}
            } else {return Err(format!("error[NOVA-CTFE014]: '{type_name}' requires concrete generic arguments"));}
        }
        match kind.as_str() {
            "field_count" if arguments.is_empty()=>Ok(Value::Int(entry.fields.len() as i64)),
            "type_name" if arguments.is_empty()=>Ok(Value::Str(type_name)),
            "has_field" if arguments.len()==1=>{
                let field=arguments.into_iter().next().unwrap().str()?;
                Ok(Value::Bool(entry.fields.iter().any(|f|f.name==field)))
            }
            "field_type" if arguments.len()==1=>{
                let field=arguments.into_iter().next().unwrap().str()?;
                let ty=entry.fields.iter().find(|f|f.name==field)
                    .ok_or_else(||format!("error[NOVA-CTFE014]: struct '{type_name}' has no field '{field}'"))?;
                let concrete=match (&ty.ty,&types[0]) {
                    (ast::TypeRef::Named(variable),ast::TypeRef::Applied(_,args))=>{
                        entry.generics.iter().position(|p|p==variable).and_then(|i|args.get(i)).unwrap_or(&ty.ty)
                    }
                    _=>&ty.ty,
                };
                Ok(Value::Str(match concrete {ast::TypeRef::Named(name)=>name.clone(),other=>format!("{other:?}")}))
            }
            _=>Err(format!("error[NOVA-CTFE014]: wrong argument count for reflection primitive '{name}'"))
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
        let key=(q(&fn_ns,&function.name),args.clone());
        if let Some(hit)=self.cache.get(&key){return Ok(hit.clone());}
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
        let output=self.block(&function.body, &fn_ns, &mut locals, depth)?
            .ok_or_else(|| format!("const fn '{fn_name}' did not return a value"))?;
        self.cache.insert(key,output.clone());
        Ok(output)
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
            Expr::Str(s)=>Ok(Value::Str(s.trim_matches('"').to_owned())),
            Expr::Var(name)=>vars.get(name).cloned().ok_or_else(||format!("non-constant variable '{name}'")),
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
                    BinaryOp::And if !a.clone().bool()?=>return Ok(Value::Bool(false)),
                    BinaryOp::Or if a.clone().bool()?=>return Ok(Value::Bool(true)),
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
                    BinaryOp::Eq=>Ok(Value::Bool(a==b)),
                    BinaryOp::Ne=>Ok(Value::Bool(a!=b)),
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
            Expr::If { condition, then_body, else_body } => {
                let test=self.expr(condition,ns,vars,depth)?.bool()?;
                let mut local=vars.clone();
                let body=if test {then_body} else {else_body};
                self.block(body,ns,&mut local,depth)?
                    .ok_or_else(||"const if expression requires a final value".into())
            },
            Expr::Block(body) => {
                let mut local=vars.clone();
                self.block(body,ns,&mut local,depth)?
                    .ok_or_else(||"const block expression requires a final value".into())
            },
            Expr::Call(name,args)=>{
                let mut values=Vec::new();
                for arg in args {values.push(self.expr(arg,ns,vars,depth)?);}
                self.evaluate(name,ns,values,depth+1)
            },
            Expr::GenericCall(name,types,args) if self.intrinsics.contains_key(name)=>{
                let mut values=Vec::new();
                for arg in args {values.push(self.expr(arg,ns,vars,depth)?);}
                self.reflection(name,types,values)
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
                Stmt::CompoundSet{name,op,value}=>{
                    if !vars.contains_key(name){return Err(format!("undefined const variable '{name}'"));}
                    let expression=Expr::Binary(Box::new(Expr::Var(name.clone())),*op,Box::new(value.clone()));
                    let result=self.expr(&expression,ns,vars,depth)?;
                    vars.insert(name.clone(),result);
                },
                Stmt::Return(Some(e))=>return self.expr(e,ns,vars,depth).map(Some),
                Stmt::If{condition,then_body,else_body}=>{
                    let cond=self.expr(condition,ns,vars,depth)?.bool()?;
                    let block=if cond {Some(then_body)}else{else_body.as_ref()};
                    if let Some(body)=block {
                        let mut inner=vars.clone();
                        let result=self.block(body,ns,&mut inner,depth)?;
                        for (key,value) in inner {if vars.contains_key(&key){vars.insert(key,value);}}
                        if result.is_some(){return Ok(result);}
                    }
                },
                Stmt::While{condition,body}=>{
                    while self.expr(condition,ns,vars,depth)?.bool()? {
                        self.consume()?;
                        let mut inner=vars.clone();
                        let result=self.block(body,ns,&mut inner,depth)?;
                        for (key,value) in inner {if vars.contains_key(&key){vars.insert(key,value);}}
                        if result.is_some(){return Ok(result);}
                    }
                },
                Stmt::For{name,start,end,body}=>{
                    let mut index=self.expr(start,ns,vars,depth)?.int()?;
                    let stop=self.expr(end,ns,vars,depth)?.int()?;
                    while index<stop {
                        self.consume()?;
                        let mut inner=vars.clone();
                        inner.insert(name.clone(),Value::Int(index));
                        let result=self.block(body,ns,&mut inner,depth)?;
                        for (key,value) in inner {if key.as_str()!=name.as_str() && vars.contains_key(&key){vars.insert(key,value);}}
                        if result.is_some(){return Ok(result);}
                        index=index.checked_add(1).ok_or("compile-time range overflow")?;
                    }
                },
                Stmt::Tail(expr)=>return self.expr(expr,ns,vars,depth).map(Some),
                Stmt::Expr(expr)=>{self.expr(expr,ns,vars,depth)?;},
                _=>return Err("const fn evaluator: unsupported statement (unsafe, break/continue, pattern matching or I/O)".into()),
            }
        }
        Ok(None)
    }
    fn fold_expr(&mut self, expr: &mut Expr, ns: &str) -> Result<(), String> {
        match expr {
            Expr::If { condition, then_body, else_body } => {
                self.fold_expr(condition,ns)?;
                self.fold_stmts(then_body,ns)?;
                self.fold_stmts(else_body,ns)?;
            }
            Expr::Block(body) => self.fold_stmts(body,ns)?,
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
        if let Expr::GenericCall(name, types, args)=expr {
            if self.intrinsics.contains_key(name) {
                let local=HashMap::new();
                let mut values=Vec::new();
                for arg in args.iter() {values.push(self.expr(arg,ns,&local,0)?);}
                self.steps=0;
                let result=self.reflection(name,types,values)?;
                *expr=match result {
                    Value::Bool(v)=>Expr::Bool(v),
                    Value::Int(v) if v>=0=>Expr::Int(v.to_string()),
                    Value::Int(v)=>Expr::Unary(UnaryOp::Neg,Box::new(Expr::Int(v.unsigned_abs().to_string()))),
                    Value::Str(s)=>Expr::Str(format!("{s:?}")),
                };
            }
        }
        if let Expr::Call(name, args) = expr {
            if let Some((function,_))=self.function(name,ns) {
                let mandatory=function.attributes.iter().any(|a|a.name=="comptime");
                let locals=HashMap::new();
                // A normal const fn may also execute at runtime when its
                // arguments are nonconstant. `comptime fn` requires CTFE.
                let values=args.iter().map(|arg|self.expr(arg,ns,&locals,0))
                    .collect::<Result<Vec<_>,_>>();
                if let Ok(values)=values {
                    self.steps=0;
                    let result=self.evaluate(name,ns,values,0)
                        .map_err(|e|format!("error[NOVA-CTFE015]: compile-time call '{name}': {e}"))?;
                    *expr=match result {
                        Value::Bool(v)=>Expr::Bool(v),
                        Value::Str(v)=>Expr::Str(format!("{v:?}")),
                        Value::Int(v) if v>=0=>Expr::Int(v.to_string()),
                        Value::Int(v)=>Expr::Unary(UnaryOp::Neg,Box::new(Expr::Int(v.unsigned_abs().to_string()))),
                    };
                } else if mandatory {
                    return Err(format!("error[NOVA-CTFE016]: comptime function '{name}' requires compile-time constant arguments"));
                }
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
                | Stmt::Expr(e) | Stmt::Tail(e) => self.fold_expr(e, ns)?,
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
    engine.verify_intrinsics()?;
    engine.assert_compile_time()?;
    let mut output = program.clone();
    engine.items(&mut output.items, "")?;
    // Contract checks and compile-time-only functions are never lowered to
    // runtime IR. In particular, CTFE-only comparisons over metadata strings
    // must not become pointer comparisons in the ordinary HIR.
    fn remove_compile_time_only(items:&mut Vec<Item>) {
        items.retain(|item| {
            let inner=match item {Item::Public(value)=>value.as_ref(),other=>other};
            !matches!(inner,Item::Function(f) if f.attributes.iter().any(|a|matches!(a.name.as_str(),"comptime"|"comptime_assert")))
        });
        for item in items {
            let inner=match item {Item::Public(value)=>value.as_mut(),other=>other};
            if let Item::Namespace{items,..}=inner {remove_compile_time_only(items);}
        }
    }
    remove_compile_time_only(&mut output.items);
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

#[cfg(test)]
mod meta_025_tests {
    use super::*;
    use crate::grammar;
    fn run(src:&str)->Result<ast::Program,String>{
        let ast=grammar::ProgramParser::new().parse(src).map_err(|e|format!("{e:?}"))?;
        fold(&ast)
    }
    #[test]
    fn comptime_loop_and_tail_value_are_folded(){
        let result=run("comptime fn f(n:i64)->i64 {let mut acc:i64=0; for i in 0..n {acc+=i;} acc} fn main()->i32 { return f(5) as i32 - 10; }").unwrap();
        let main=result.items.iter().find_map(|item|match item {Item::Function(f) if f.name=="main"=>Some(f),_=>None}).unwrap();
        assert!(!format!("{:?}",main.body).contains("Call(\"f\""));
    }
    #[test]
    fn comptime_contract_reports_failure(){
        let err=run("#[comptime_assert] fn validation()->bool {return false;} fn main()->i32 {return 0;}").unwrap_err();
        assert!(err.contains("NOVA-CTFE013"),"{err}");
    }
    #[test]
    fn comptime_contract_rejects_unsafe_calls(){
        let err=run("fn io()->bool{return true;} #[comptime_assert] fn validation()->bool {return io();}").unwrap_err();
        assert!(err.contains("unknown const function"),"{err}");
    }
}

#[cfg(test)]
mod meta_025_reflection_tests {
    use super::*;
    use crate::grammar;
    fn program(src:&str)->Result<ast::Program,String>{
        let parsed=grammar::ProgramParser::new().parse(src).map_err(|e|format!("{e:?}"))?;
        fold(&parsed)
    }
    #[test]
    fn reflection_contract_is_evaluated_using_ast_not_runtime(){
        let input=r#"
        namespace std { namespace meta {
            #[meta_intrinsic(field_count)] fn field_count<T>() -> i64 {return 0;}
            #[meta_intrinsic(has_field)] fn has_field<T>(name:str) -> bool {return false;}
        }}
        struct User { id:i64, active:bool }
        #[comptime_assert] fn fields_are_valid()->bool {
            std::meta::field_count::<User>() == 2 && std::meta::has_field::<User>("id")
        }
        fn main()->i32 {return 0;}
        "#;
        program(input).unwrap();
    }
    #[test]
    fn reflection_intrinsics_are_not_user_defined_host_hooks(){
        let error=program("#[meta_intrinsic(field_count)] fn sneak<T>()->i64{return 0;} fn main()->i32{return 0;}").unwrap_err();
        assert!(error.contains("NOVA-CTFE011"),"{error}");
    }
    #[test]
    fn nonterminating_comptime_loop_respects_instruction_budget(){
        let error=program("#[comptime_assert] fn endless()->bool {while true {} return true;} fn main()->i32{return 0;}").unwrap_err();
        assert!(error.contains("evaluation budget"),"{error}");
    }
}
