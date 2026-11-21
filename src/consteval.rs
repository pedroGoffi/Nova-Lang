//! Deterministic, bounded compile-time evaluation and read-only type reflection.
//! Operates on the source AST after macro expansion and effect checking. Only
//! calls whose arguments are numeric constants are folded; other calls remain
//! ordinary runtime functions. Unsupported operations are not silently executed.
use crate::ast::{self, BinaryOp, Expr, Function, Item, Stmt, UnaryOp, TypeRef};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    Float(String),
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

// The scalar const evaluator only handles i64/bool/str. Nova-authored AST
// transforms run in proc_macros::Engine, whose values include MetaType and
// MetaDecl. Their source bodies are already expanded before consteval::fold;
// asking the scalar evaluator to execute them would misdiagnose MetaType
// parameters as nonconstant runtime locals (NOVA-CTFE016).
fn procedural_meta_type(ty: &TypeRef, _ns: &str) -> bool {
    let TypeRef::Named(name) = ty else { return false; };
    let short = name.rsplit("::").next().unwrap_or(name);
    matches!(short,
        "MetaProgram" | "MetaType" | "MetaField" | "MetaFields" | "MetaMethods" |
        "MetaMethod" | "MetaTypeRef" | "MetaExpr" | "MetaDecl" |
        "MetaDecls" | "MetaArgs")
}

fn procedural_comptime(f: &Function, ns: &str) -> bool {
    f.attributes.iter().any(|a| a.name == "comptime")
        && (f.attributes.iter().any(|a| a.name == "meta_proc_intrinsic")
            || ns.starts_with("std::introspection::meta")
            || f.params.iter().any(|p| procedural_meta_type(&p.ty, ns))
            || procedural_meta_type(&f.result, ns))
}
#[derive(Default)]
struct Engine {
    functions: HashMap<String, (Function, String)>,
    constants: HashMap<String, (ast::CompileTimeConst, String)>,
    aliases: HashMap<String, (TypeRef, String)>,
    constant_values: HashMap<String, Value>,
    evaluating_constants: HashSet<String>,
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
                Item::CompileTimeConst(decl) => {
                    self.constants.insert(q(ns,&decl.name),(decl.clone(),ns.into()));
                }
                Item::TypeAlias(alias) => {
                    self.aliases.insert(q(ns,&alias.name),(alias.target.clone(),ns.into()));
                }
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
                            if !matches!(ns, "std::meta" | "std::introspection::meta") || attribute.args.len()!=1 ||
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
    fn constant_key(&self, name: &str, ns: &str) -> Option<String> {
        if name.contains("::") {
            return self.constants.contains_key(name).then(||name.into());
        }
        let mut current=ns;
        loop {
            let candidate=q(current,name);
            if self.constants.contains_key(&candidate) {return Some(candidate);}
            if current.is_empty(){return None;}
            current=current.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
        }
    }
    fn checked_type(&self, name: &str, ns: &str) -> Option<TypeRef> {
        let mut ty=TypeRef::Named(name.into());
        let mut current_ns=ns.to_owned();
        for _ in 0..32 {
            let TypeRef::Named(ref symbol)=ty else {return Some(ty);};
            let target = if symbol.contains("::") {self.aliases.get(symbol).cloned()} else {
                let mut found=None;
                let mut here=current_ns.as_str();
                loop {
                    if let Some(alias)=self.aliases.get(&q(here,symbol)) {found=Some(alias.clone());break;}
                    if here.is_empty(){break;}
                    here=here.rsplit_once("::").map(|(p,_)|p).unwrap_or("");
                }
                found
            };
            let Some((new_ty,new_ns))=target else {return Some(ty);};
            ty=new_ty; current_ns=new_ns;
        }
        None // recursive typedef
    }
    fn validate_constant(&self, key:&str, decl:&ast::CompileTimeConst, ns:&str, value:&Value) -> Result<(),String> {
        let ty=match &decl.ty {
            TypeRef::Named(name)=>self.checked_type(name,ns).ok_or_else(||format!("error[NOVA-CTFE021]: typedef cycle for '{key}'"))?,
            _=>return Err(format!("error[NOVA-CTFE021]: compile-time constant '{key}' must be scalar")),
        };
        let TypeRef::Named(name)=ty else {return Err(format!("error[NOVA-CTFE021]: compile-time constant '{key}' requires scalar type"));};
        let allowed = match (name.as_str(),value) {
            ("bool",Value::Bool(_)) | ("str",Value::Str(_))=>true,
            ("f32"|"f64",Value::Float(_))=>true,
            ("i8",Value::Int(v))=> i8::try_from(*v).is_ok(),
            ("i16",Value::Int(v))=> i16::try_from(*v).is_ok(),
            ("i32",Value::Int(v))=> i32::try_from(*v).is_ok(),
            ("i64"|"isize",Value::Int(_))=>true,
            ("u8",Value::Int(v))=> u8::try_from(*v).is_ok(),
            ("u16",Value::Int(v))=> u16::try_from(*v).is_ok(),
            ("u32",Value::Int(v))=> u32::try_from(*v).is_ok(),
            ("u64"|"usize",Value::Int(v))=>*v>=0,
            _=>false,
        };
        if !allowed {
            return Err(format!("error[NOVA-CTFE021]: '{key}' value {value:?} does not fit declared constant type {name}; only scalar bool, str and integers up to signed i64 are supported"));
        }
        Ok(())
    }
    fn constant_value(&mut self,key:&str) -> Result<Value,String> {
        if let Some(value)=self.constant_values.get(key) {return Ok(value.clone());}
        if !self.evaluating_constants.insert(key.into()) {
            return Err(format!("error[NOVA-CTFE020]: cyclic compile-time constant '{key}'"));
        }
        let result=(|| {
            let (decl,ns)=self.constants.get(key).ok_or_else(||format!("unknown constant '{key}'"))?.clone();
            let value=self.expr(&decl.value,&ns,&HashMap::new(),0)?;
            self.validate_constant(key,&decl,&ns,&value)?;
            Ok::<Value,String>(value)
        })();
        self.evaluating_constants.remove(key);
        let value=result?;
        self.constant_values.insert(key.into(),value.clone());
        Ok(value)
    }
    fn constant_literal(value: Value) -> Expr {
        match value {
            Value::Bool(v)=>Expr::Bool(v),
            Value::Str(v)=>Expr::Str(format!("{v:?}")),
            Value::Int(v) if v>=0=>Expr::Int(v.to_string()),
            Value::Int(v)=>Expr::Unary(UnaryOp::Neg,Box::new(Expr::Int(v.unsigned_abs().to_string()))),
            Value::Float(v)=>Expr::Float(v),
        }
    }
    // Substitute immutable module-level constants while respecting each
    // lexical scope. Never rewrite a local or a lambda argument with the same
    // identifier. No global storage is produced by this operation.
    fn inline_expr(&mut self, e:&mut Expr, ns:&str, locals:&HashSet<String>) -> Result<(),String> {
        if let Expr::Var(name)=e {
            if !locals.contains(name) || name.contains("::") {
                if let Some(key)=self.constant_key(name,ns) {
                    *e=Self::constant_literal(self.constant_value(&key)?);
                }
            }
            return Ok(());
        }
        match e {
            Expr::Call(_,args) | Expr::GenericCall(_,_,args)
            | Expr::MacroCall(_,args) | Expr::Array(args) =>
                for a in args {self.inline_expr(a,ns,locals)?;},
            Expr::MethodCall(receiver,_,args,_) => {
                self.inline_expr(receiver,ns,locals)?;
                for a in args {self.inline_expr(a,ns,locals)?;}
            }
            Expr::CallIndirect(target,args) => {
                self.inline_expr(target,ns,locals)?;
                for a in args {self.inline_expr(a,ns,locals)?;}
            }
            Expr::Binary(a,_,b) | Expr::Index(a,b) => {
                self.inline_expr(a,ns,locals)?;self.inline_expr(b,ns,locals)?;
            }
            Expr::Unary(_,v) | Expr::Deref(v) | Expr::Spread(v)
            | Expr::Yield(v) | Expr::Await(v) | Expr::Field(v,_)
            | Expr::AddressOf{value:v,..} | Expr::Cast(v,_) => self.inline_expr(v,ns,locals)?,
            Expr::StructInit(_,fields) | Expr::GenericStructInit(_,_,fields) =>
                for (_,v) in fields {self.inline_expr(v,ns,locals)?;},
            Expr::If{condition,then_body,else_body} => {
                self.inline_expr(condition,ns,locals)?;
                self.inline_block(then_body,ns,locals)?;
                self.inline_block(else_body,ns,locals)?;
            }
            Expr::Block(body)=>self.inline_block(body,ns,locals)?,
            Expr::Lambda{params,body,..}=>{
                let mut inner=locals.clone();
                for p in params {inner.insert(p.name.clone());}
                self.inline_block(body,ns,&inner)?;
            }
            _=>{}
        }
        Ok(())
    }
    fn inline_block(&mut self,body:&mut[Stmt],ns:&str,outer:&HashSet<String>) -> Result<(),String> {
        let mut locals=outer.clone();
        for stmt in body {
            match stmt {
                Stmt::Let{name,init,defer,..}=>{
                    self.inline_expr(init,ns,&locals)?;
                    locals.insert(name.clone());
                    if let Some(defer)=defer {self.inline_expr(defer,ns,&locals)?;}
                }
                Stmt::Set{name,value} | Stmt::CompoundSet{name,value,..} =>{
                    // A constant is never assignable, including a qualified
                    // module constant. Reject before HIR for a clear message.
                    if !locals.contains(name) {
                        if let Some(key)=self.constant_key(name,ns) {
                            return Err(format!("error[NOVA-CTFE022]: cannot assign to compile-time constant '{key}'"));
                        }
                    }
                    self.inline_expr(value,ns,&locals)?;
                }
                Stmt::DerefSet{value,..} | Stmt::FieldSet{value,..} | Stmt::Defer(value)
                | Stmt::Return(Some(value)) | Stmt::Expr(value) | Stmt::Tail(value) =>
                    self.inline_expr(value,ns,&locals)?,
                Stmt::If{condition,then_body,else_body}=>{
                    self.inline_expr(condition,ns,&locals)?;
                    self.inline_block(then_body,ns,&locals)?;
                    if let Some(other)=else_body {self.inline_block(other,ns,&locals)?;}
                }
                Stmt::While{condition,body}=>{
                    self.inline_expr(condition,ns,&locals)?;
                    self.inline_block(body,ns,&locals)?;
                }
                Stmt::Loop(body) | Stmt::Unsafe(body)=>self.inline_block(body,ns,&locals)?,
                Stmt::For{name,start,end,body}=>{
                    self.inline_expr(start,ns,&locals)?;
                    self.inline_expr(end,ns,&locals)?;
                    let mut inner=locals.clone();inner.insert(name.clone());
                    self.inline_block(body,ns,&inner)?;
                }
                Stmt::ForEach{name,iterable,body}=>{
                    self.inline_expr(iterable,ns,&locals)?;
                    let mut inner=locals.clone();inner.insert(name.clone());
                    self.inline_block(body,ns,&inner)?;
                }
                Stmt::Match{value,arms}=>{
                    self.inline_expr(value,ns,&locals)?;
                    for arm in arms {
                        let mut bindings=locals.clone();
                        if let ast::Pattern::Variant{bindings: names,..}=&arm.pattern {
                            for name in names {bindings.insert(name.clone());}
                        }
                        self.inline_block(&mut arm.body,ns,&bindings)?;
                    }
                }
                _=>{}
            }
        }
        Ok(())
    }
    fn inline_items(&mut self,items:&mut[Item],ns:&str) -> Result<(),String> {
        for item in items {
            match item {
                Item::Public(inner)=>self.inline_items(std::slice::from_mut(inner.as_mut()),ns)?,
                Item::Namespace{name,items}=>self.inline_items(items,&q(ns,name))?,
                Item::Struct(decl) => {
                    for field in &mut decl.fields {
                        if let Some(default)=&mut field.default {
                            self.inline_expr(default,ns,&HashSet::new())?;
                        }
                    }
                }
                Item::Function(f)=>{
                    let args=f.params.iter().map(|p|p.name.clone()).collect();
                    self.inline_block(&mut f.body,ns,&args)?;
                }
                Item::Impl(block)=>for f in &mut block.methods {
                    let args=f.params.iter().map(|p|p.name.clone()).collect();
                    self.inline_block(&mut f.body,ns,&args)?;
                },
                _=>{}
            }
        }
        Ok(())
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
            Expr::Float(s)=>Ok(Value::Float(s.clone())),
            Expr::Var(name)=> {
                if let Some(value)=vars.get(name) {return Ok(value.clone());}
                if let Some(key)=self.constant_key(name,ns) {return self.constant_value(&key);}
                Err(format!("non-constant variable '{name}'"))
            },
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
            _=>Err(format!("const fn evaluator supports local integers, booleans, arithmetic, if/return and calls to const fn; expression={expr:?}")),
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
    fn has_runtime_variable(expr:&Expr) -> bool {
        match expr {
            Expr::Var(_) => true,
            Expr::Unary(_,value) | Expr::Deref(value) | Expr::Spread(value)
            | Expr::Yield(value) | Expr::Await(value) | Expr::Cast(value,_)
            | Expr::Field(value,_) | Expr::AddressOf{value,..} => Self::has_runtime_variable(value),
            Expr::Binary(left,_,right) | Expr::Index(left,right) =>
                Self::has_runtime_variable(left) || Self::has_runtime_variable(right),
            Expr::Call(_,args) | Expr::GenericCall(_,_,args) | Expr::MacroCall(_,args)
            | Expr::Array(args) => args.iter().any(Self::has_runtime_variable),
            Expr::MethodCall(receiver,_,args,_) =>
                Self::has_runtime_variable(receiver) || args.iter().any(Self::has_runtime_variable),
            Expr::CallIndirect(receiver,args) =>
                Self::has_runtime_variable(receiver) || args.iter().any(Self::has_runtime_variable),
            Expr::StructInit(_,fields) | Expr::GenericStructInit(_,_,fields) =>
                fields.iter().any(|(_,value)|Self::has_runtime_variable(value)),
            _=>false,
        }
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
            Expr::MethodCall(receiver, _, args, _) => {
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
                    Value::Float(s)=>Expr::Float(s),
                };
            }
        }
        if let Expr::Call(name, args) = expr {
            if let Some((function, definition_ns)) = self.function(name, ns) {
                if procedural_comptime(&function, &definition_ns) {
                    return Ok(());
                }
            }
            if let Some((function,definition_ns))=self.function(name,ns) {
                // Already owned by the typed procedural AST evaluator, not
                // the scalar const evaluator. Never execute MetaType/MetaDecl
                // functions with scalar Value or require literal arguments.
                if procedural_comptime(&function, &definition_ns) {
                    return Ok(());
                }
                let mandatory=function.attributes.iter().any(|a|a.name=="comptime");
                let locals=HashMap::new();
                // A normal const fn may also execute at runtime when its
                // arguments are nonconstant. `comptime fn` requires CTFE.
                // A local Var must not accidentally resolve to a module
                // constant of the same name. Globally constant references
                // were already substituted by inline_items, so any remaining
                // variable makes this a runtime call (or a CTFE error).
                let values=if args.iter().any(Self::has_runtime_variable) {
                    Err("runtime local variable in compile-time call".into())
                } else {
                    args.iter().map(|arg|self.expr(arg,ns,&locals,0))
                        .collect::<Result<Vec<_>,_>>()
                };
                if let Ok(values)=values {
                    self.steps=0;
                    let result=self.evaluate(name,ns,values,0)
                        .map_err(|e|format!("error[NOVA-CTFE015]: compile-time call '{name}': {e}"))?;
                    *expr=match result {
                        Value::Bool(v)=>Expr::Bool(v),
                        Value::Str(v)=>Expr::Str(format!("{v:?}")),
                        Value::Int(v) if v>=0=>Expr::Int(v.to_string()),
                        Value::Int(v)=>Expr::Unary(UnaryOp::Neg,Box::new(Expr::Int(v.unsigned_abs().to_string()))),
                        Value::Float(v)=>Expr::Float(v),
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
                Item::Struct(decl) => {
                    for field in &mut decl.fields {
                        if let Some(default)=&mut field.default {
                            self.fold_expr(default,ns)?;
                        }
                    }
                }
                Item::Function(f) => {
                    // Procedural comptime functions are interpreted (and their
                    // arguments supplied) by proc_macros::expand. Their bodies
                    // are removed below, so scalar folding must not inspect
                    // calls inside them at all.
                    if !ns.starts_with("std::introspection") && !procedural_comptime(f, ns) {
                        self.fold_stmts(&mut f.body, ns)?;
                    }
                }
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
    // Resolve all constants (including unused ones) to diagnose cycles and
    // illegal initializers deterministically at compile time.
    let mut keys=engine.constants.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    for key in keys {engine.steps=0;engine.constant_value(&key)?;}
    engine.assert_compile_time()?;
    let mut output = program.clone();
    engine.inline_items(&mut output.items, "")?;
    engine.items(&mut output.items, "")?;
    // Contract checks and compile-time-only functions are never lowered to
    // runtime IR. In particular, CTFE-only comparisons over metadata strings
    // must not become pointer comparisons in the ordinary HIR.
    fn remove_compile_time_only(items:&mut Vec<Item>) {
        items.retain(|item| {
            let inner=match item {Item::Public(value)=>value.as_ref(),other=>other};
            !matches!(inner, Item::CompileTimeConst(_)) &&
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
    fn global_constants_fold_into_literals_and_leave_no_runtime_symbol() {
        let result=source(r#"
            comptime const BASE: i32 = -100;
            const MASK: i32 = 1 | 64 | 512;
            namespace nested { pub comptime const DERIVED: i32 = BASE + MASK; }
            fn main()->i32 {return nested::DERIVED - 477;}
        "#).unwrap();
        assert!(!result.items.iter().any(|i|matches!(i,Item::CompileTimeConst(_))));
        let Item::Function(main)=result.items.last().unwrap() else {panic!("missing main")};
        assert!(!format!("{:?}",main.body).contains("DERIVED"));
    }
    #[test]
    fn constants_respect_shadowing_and_type_ranges() {
        let result=source("const VALUE: i32 = 42; fn main()->i32 { let VALUE:i32=8; return VALUE; }").unwrap();
        let Item::Function(main)=result.items.last().unwrap() else {panic!()};
        assert!(matches!(&main.body[1],Stmt::Return(Some(Expr::Var(v))) if v=="VALUE"));
        let err=source("const BAD: u8 = 300; fn main()->i32{return 0;}").unwrap_err();
        assert!(err.contains("NOVA-CTFE021"),"{err}");
    }
    #[test]
    fn local_shadows_global_even_in_const_function_arguments() {
        let result=source("const X:i32=10; const fn add(x:i32)->i32{return x+1;} fn main()->i32{let X:i32=3;return add(X);}").unwrap();
        let Item::Function(main)=result.items.last().unwrap() else {panic!()};
        assert!(matches!(&main.body[1],Stmt::Return(Some(Expr::Call(_,args))) if matches!(&args[0],Expr::Var(x) if x=="X")));
    }
    #[test]
    fn constants_reject_cycles_and_mutation() {
        let err=source("const A:i32=B; const B:i32=A; fn main()->i32{return 0;}").unwrap_err();
        assert!(err.contains("NOVA-CTFE020"),"{err}");
        let err=source("const A:i32=42; fn main()->i32{A=3;return 0;}").unwrap_err();
        assert!(err.contains("NOVA-CTFE022"),"{err}");
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
    fn procedural_ast_helpers_are_not_misread_as_scalar_ctfe() {
        let result = source(r#"
            namespace std { namespace meta {
                struct MetaType {}
                struct MetaDecl {}
                #[comptime]
                #[meta_proc_intrinsic(impl_for)]
                pub fn impl_for(target: MetaType) -> MetaDecl { return 0; }
                #[comptime]
                pub fn generate_setters(target: MetaType) -> MetaDecl {
                    return impl_for(target);
                }
            }}
            #[comptime]
            fn make_properties(target: std::meta::MetaType) -> std::meta::MetaDecl {
                return std::meta::impl_for(target);
            }
            fn main() -> i64 { return 42; }
        "#).unwrap();
        assert!(result.items.iter().any(|item|matches!(item,
            Item::Function(f) if f.name == "main")));
        assert!(!result.items.iter().any(|item|matches!(item,
            Item::Function(f) if f.name == "make_properties")));
    }

    #[test]
    fn scalar_comptime_still_requires_constant_arguments() {
        let err = source(r#"
            #[comptime] fn square(value: i64) -> i64 { return value * value; }
            fn main() -> i64 { let local: i64 = 3; return square(local); }
        "#).unwrap_err();
        assert!(err.contains("NOVA-CTFE016"), "{err}");

        let ok = source(r#"
            #[comptime] fn square(value: i64) -> i64 { return value * value; }
            fn main() -> i64 { return square(3); }
        "#).unwrap();
        let Item::Function(main) = ok.items.last().unwrap() else { panic!("missing main") };
        assert!(matches!(&main.body[0], Stmt::Return(Some(Expr::Int(value))) if value == "9"));
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
