//! Nova-authored procedural derives and attribute macros.
//!
//! The trusted boundary is deliberately small: the compiler exposes typed AST
//! constructors, not source-string evaluation or host I/O. Macro bodies are
//! interpreted using a bounded, deterministic subset of normal Nova AST.
//! Every generated declaration goes through ordinary surface/HIR/ownership.
use crate::ast::{self, Expr, Function, Item, Stmt, TypeRef};
use std::collections::{HashMap, HashSet};

const MAX_STEPS: usize = 100_000;
const MAX_DEPTH: usize = 32;
const MAX_GENERATED: usize = 1024;

fn qualified(ns: &str, name: &str) -> String {
    if ns.is_empty() { name.to_string() } else { format!("{ns}::{name}") }
}

#[derive(Clone, Debug)]
enum Value {
    Unit,
    Int(i64),
    Bool(bool),
    Text(String),
    Target(ast::Struct),
    Field(ast::Field),
    Fields(Vec<ast::Field>),
    Arguments(Vec<String>),
    Type(TypeRef),
    Expression(Expr),
    Declaration(Item),
    Declarations(Vec<Item>),
}
impl Value {
    fn text(self) -> Result<String, String> {
        match self { Self::Text(s) => Ok(s), _ => Err("expected compile-time str".into()) }
    }
    fn target(self) -> Result<ast::Struct, String> {
        match self { Self::Target(s) => Ok(s), _ => Err("expected MetaType".into()) }
    }
    fn field(self) -> Result<ast::Field, String> {
        match self { Self::Field(f) => Ok(f), _ => Err("expected MetaField".into()) }
    }
    fn arguments(self) -> Result<Vec<String>, String> {
        match self {Self::Arguments(a)=>Ok(a),_=>Err("expected MetaArgs".into())}
    }
    fn ty(self) -> Result<TypeRef, String> {
        match self { Self::Type(t) => Ok(t), _ => Err("expected MetaTypeRef".into()) }
    }
    fn expression(self) -> Result<Expr, String> {
        match self { Self::Expression(e) => Ok(e), _ => Err("expected MetaExpr".into()) }
    }
    fn declaration(self) -> Result<Item, String> {
        match self { Self::Declaration(d) => Ok(d), _ => Err("expected MetaDecl".into()) }
    }
    fn boolean(self) -> Result<bool, String> {
        match self { Self::Bool(b) => Ok(b), _ => Err("expected compile-time bool".into()) }
    }
    fn into_items(self) -> Result<Vec<Item>, String> {
        match self {
            Self::Declarations(items) => Ok(items),
            Self::Declaration(item) => Ok(vec![item]),
            _ => Err("procedural macro must return MetaDecl or MetaDecls".into()),
        }
    }
}
#[derive(Clone)]
struct MacroDef {
    function: Function,
    namespace: String,
    public: bool,
    mode: Mode,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Derive, Attribute, Expression }

struct Engine {
    derives: HashMap<String, MacroDef>,
    attributes: HashMap<String, MacroDef>,
    expressions: HashMap<String, MacroDef>,
    functions: HashMap<String, (Function, String)>,
    intrinsics: HashMap<String, String>,
    steps: usize,
    emitted: usize,
}
impl Engine {
    fn new(program: &ast::Program) -> Result<Self, String> {
        let mut result = Self {
            derives: HashMap::new(), attributes: HashMap::new(), expressions: HashMap::new(),
            functions: HashMap::new(), intrinsics: HashMap::new(),
            steps: 0, emitted: 0,
        };
        result.collect(&program.items, "", false)?;
        Ok(result)
    }
    fn collect(&mut self, items: &[Item], ns: &str, public: bool) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => self.collect(std::slice::from_ref(inner.as_ref()), ns, true)?,
                Item::Namespace { name, items } => self.collect(items, &qualified(ns,name), public)?,
                Item::Function(f) => {
                    let fullname = qualified(ns, &f.name);
                    for attribute in &f.attributes {
                        if attribute.name == "meta_proc_intrinsic" {
                            if ns != "std::meta" || attribute.args.len()!=1 ||
                                !matches!(attribute.args[0].as_str(),
                                    "fields" | "field_name" | "field_type_ref" | "type_ref" |
                                    "reference" | "mutable_reference" | "unit_type" | "assign" | "impl_for" | "function" | "parameter" |
                                    "method" | "declarations" | "append" | "variable" |
                                    "access" | "struct_decl" | "struct_field" | "type_named" |
                                    "call" | "int_expr" | "binary" | "field_count" | "target_name" |
                                    "has_field" | "require" | "argument_count" | "argument") {
                                return Err(format!("error[NOVA-PROC001]: illegal procedural intrinsic on '{fullname}'"));
                            }
                            if self.intrinsics.insert(fullname.clone(), attribute.args[0].clone()).is_some() {
                                return Err(format!("error[NOVA-PROC001]: duplicate intrinsic '{fullname}'"));
                            }
                        }
                    }
                    let markers: Vec<_> = f.attributes.iter().filter(|a|
                        matches!(a.name.as_str(), "proc_derive" | "proc_attribute" | "proc_macro")).collect();
                    if markers.len()>1 {
                        return Err(format!("error[NOVA-PROC002]: '{fullname}' has multiple procedural macro roles"));
                    }
                    if let Some(marker) = markers.first() {
                        let is_expression=marker.name=="proc_macro";
                        let valid_result=if is_expression {
                            matches!(&f.result,TypeRef::Named(n) if n=="MetaExpr" || n=="std::meta::MetaExpr")
                        }else{
                            matches!(&f.result,TypeRef::Named(n) if matches!(n.as_str(),"MetaDecl"|"MetaDecls"|"std::meta::MetaDecl"|"std::meta::MetaDecls"))
                        };
                        let correct_params=if is_expression {
                            !f.params.is_empty() && f.params.iter().all(|p|matches!(&p.ty,TypeRef::Named(n) if n=="MetaExpr"||n=="std::meta::MetaExpr"))
                        }else{
                            (f.params.len()==1 || (marker.name=="proc_attribute" && f.params.len()==2)) &&
                            matches!(&f.params[0].ty,TypeRef::Named(n) if n=="MetaType" || n=="std::meta::MetaType") &&
                            (f.params.len()==1 || matches!(&f.params[1].ty,TypeRef::Named(n) if n=="MetaArgs" || n=="std::meta::MetaArgs"))
                        };
                        if !f.attributes.iter().any(|a|a.name=="comptime") || marker.args.len()!=1 ||
                            !f.generics.is_empty() || !f.concept_bounds.is_empty() || f.variadic ||
                            !correct_params || !valid_result {
                            return Err(format!("error[NOVA-PROC002]: invalid #[{}] signature on '{fullname}' (expected compile-time AST arguments and {} return)",marker.name,if is_expression{"MetaExpr"}else{"MetaDecl or MetaDecls"}));
                        }
                        let name=&marker.args[0];
                        valid_ident(name)?;
                        let mode=match marker.name.as_str() {"proc_derive"=>Mode::Derive,"proc_attribute"=>Mode::Attribute,_=>Mode::Expression};
                        if mode==Mode::Derive && matches!(name.as_str(),"Clone"|"Debug"|"Default"|"Json"|"Serialize"|"Deserialize") {
                            return Err(format!("error[NOVA-PROC003]: procedural derive '{}' cannot replace a builtin derive",name));
                        }
                        if mode==Mode::Attribute && matches!(name.as_str(),"derive"|"repr"|"effects"|"comptime"|"operator"|"requires"|"capabilities") {
                            return Err(format!("error[NOVA-PROC003]: '{}' is a reserved compiler attribute",name));
                        }
                        let target=match mode {Mode::Derive=>&mut self.derives,Mode::Attribute=>&mut self.attributes,Mode::Expression=>&mut self.expressions};
                        let key=qualified(ns,name);
                        if target.insert(key.clone(),MacroDef{function:f.clone(),namespace:ns.into(),public,mode}).is_some() {
                            return Err(format!("error[NOVA-PROC003]: duplicate procedural macro '{key}'"));
                        }
                    }
                    if f.attributes.iter().any(|a|a.name=="comptime") {
                        self.functions.insert(fullname,(f.clone(),ns.to_string()));
                    }
                }
                _=>{}
            }
        }
        Ok(())
    }
    fn tick(&mut self) -> Result<(),String> {
        self.steps += 1;
        if self.steps > MAX_STEPS {Err(format!("error[NOVA-PROC004]: macro evaluation exceeded {MAX_STEPS} operations"))}else{Ok(())}
    }
    fn lookup<'a>(map: &'a HashMap<String, MacroDef>, ns: &str, name: &str) -> Option<(&'a String, &'a MacroDef)> {
        if name.contains("::") {return map.get_key_value(name);}
        let mut scope=ns;
        loop {
            if let Some(found)=map.get_key_value(&qualified(scope,name)) {return Some(found);}
            if scope.is_empty() {break;}
            scope=scope.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
        }
        None
    }
    fn intrinsic(&mut self, name:&str, values:Vec<Value>)->Result<Value,String> {
        let kind=self.intrinsics.get(name).ok_or_else(||format!("error[NOVA-PROC005]: '{name}' is not a declared Nova Meta intrinsic"))?.clone();
        macro_rules! arity { ($n:expr) => { if values.len()!=$n{return Err(format!("error[NOVA-PROC005]: {name} expects {} arguments",$n));} }; }
        match kind.as_str() {
            "fields" => {arity!(1);Ok(Value::Fields(values[0].clone().target()?.fields))}
            "field_name"=>{arity!(1);Ok(Value::Text(values[0].clone().field()?.name))}
            "field_type_ref"=>{arity!(1);Ok(Value::Type(values[0].clone().field()?.ty))}
            "field_count"=>{arity!(1);Ok(Value::Int(values[0].clone().target()?.fields.len() as i64))}
            "argument_count"=>{arity!(1);Ok(Value::Int(values[0].clone().arguments()?.len() as i64))}
            "argument"=>{arity!(2);let index=match values[1].clone(){Value::Int(i) if i>=0 => i as usize,
                _=>return Err("error[NOVA-PROC005]: argument index must be nonnegative integer".into())};
                let args=values[0].clone().arguments()?;
                args.get(index).cloned().map(Value::Text).ok_or_else(||"error[NOVA-PROC005]: metadata argument index is out of range".into())}
            "has_field"=>{arity!(2);let name=values[1].clone().text()?;
                Ok(Value::Bool(values[0].clone().target()?.fields.iter().any(|f|f.name==name)))}
            "require"=>{arity!(2);if !values[0].clone().boolean()?{
                return Err(format!("error[NOVA-PROC009]: {}",values[1].clone().text()?));
                }Ok(Value::Bool(true))}
            "target_name"=>{arity!(1);Ok(Value::Text(values[0].clone().target()?.name))}
            "type_ref"=>{arity!(1);let s=values[0].clone().target()?;
                Ok(Value::Type(if s.generics.is_empty(){TypeRef::Named(s.name)}else{
                    TypeRef::Applied(s.name,s.generics.into_iter().map(TypeRef::Named).collect())}))}
            "type_named"=>{arity!(1);let name=values[0].clone().text()?;valid_type_path(&name)?;Ok(Value::Type(TypeRef::Named(name)))}
            "reference"=>{arity!(1);Ok(Value::Type(TypeRef::Reference{mutable:false,inner:Box::new(values[0].clone().ty()?)}))}
            "mutable_reference"=>{arity!(1);Ok(Value::Type(TypeRef::Reference{mutable:true,inner:Box::new(values[0].clone().ty()?)}))}
            "unit_type"=>{arity!(0);Ok(Value::Type(TypeRef::Named("()".into())))}
            "assign"=>{arity!(2);
                let destination=values[0].clone().expression()?;
                let value=values[1].clone().expression()?;
                match destination {
                    Expr::Field(owner,field)=>match *owner {
                        Expr::Var(root)=>Ok(Value::Expression(Expr::Block(vec![
                            Stmt::FieldSet{root,field,op:None,value},
                            Stmt::Tail(Expr::Unit),
                        ]))),
                        _=>Err("error[NOVA-PROC005]: assign currently requires a field on a named variable".into()),
                    },
                    _=>Err("error[NOVA-PROC005]: assign currently requires a field target".into()),
                }
            }
            "variable"=>{arity!(1);let name=values[0].clone().text()?;valid_ident(&name)?;Ok(Value::Expression(Expr::Var(name)))}
            "int_expr"=>{arity!(1);if let Value::Int(n)=values[0].clone() {Ok(Value::Expression(Expr::Int(n.to_string())))}else{Err("int_expr expects integer".into())}}
            "access"=>{arity!(2);let field=values[1].clone().text()?;valid_ident(&field)?;
                Ok(Value::Expression(Expr::Field(Box::new(values[0].clone().expression()?),field)))}
            "binary"=>{arity!(3);let op=match values[0].clone().text()?.as_str(){
                "+"=>ast::BinaryOp::Add,"-"=>ast::BinaryOp::Sub,"*"=>ast::BinaryOp::Mul,
                "=="=>ast::BinaryOp::Eq,"!="=>ast::BinaryOp::Ne,
                "&"=>ast::BinaryOp::BitAnd,"|"=>ast::BinaryOp::BitOr,
                _=>return Err("error[NOVA-PROC005]: unsupported AST binary operator".into()),};
                Ok(Value::Expression(Expr::Binary(Box::new(values[1].clone().expression()?),op,Box::new(values[2].clone().expression()?))))}
            "call"=>{arity!(2);let path=values[0].clone().text()?;valid_type_path(&path)?;
                Ok(Value::Expression(Expr::Call(path,vec![values[1].clone().expression()?])))}
            "impl_for"=>{arity!(1);let s=values[0].clone().target()?;
                let target=if s.generics.is_empty(){TypeRef::Named(s.name)}else{
                    TypeRef::Applied(s.name,s.generics.iter().map(|x|TypeRef::Named(x.clone())).collect())};
                Ok(Value::Declaration(Item::Impl(ast::Impl{generics:s.generics,trait_name:None,target,associated_types:vec![],methods:vec![]})))}
            "function"=>{arity!(3);let name=values[0].clone().text()?;valid_ident(&name)?;
                let ty=values[1].clone().ty()?;let expr=values[2].clone().expression()?;
                Ok(Value::Declaration(Item::Function(Function{
                    mode:ast::FunctionMode::Normal,name,generics:vec![],concept_bounds:vec![],
                    params:vec![],variadic:false,result:ty.clone(),body:if ty==TypeRef::Named("()".into()) {
                        vec![Stmt::Expr(expr),Stmt::Return(None)]
                    } else {vec![Stmt::Return(Some(expr))]},
                    is_virtual:false,is_override:false,attributes:vec![]})))}
            "parameter"=>{arity!(3);let name=values[1].clone().text()?;valid_ident(&name)?;
                let mut f=match values[0].clone().declaration()?{Item::Function(f)=>f,_=>return Err("parameter requires a function declaration".into())};
                if f.params.iter().any(|p|p.name==name){return Err("duplicate parameter in generated function".into());}
                f.params.push(ast::Param{name,ty:values[2].clone().ty()?,default:None});
                Ok(Value::Declaration(Item::Function(f)))}
            "method"=>{arity!(2);let mut block=match values[0].clone().declaration()?{
                Item::Impl(i)=>i,_=>return Err("method requires an impl declaration".into())};
                let method=match values[1].clone().declaration()?{
                Item::Function(f)=>f,_=>return Err("method requires a function declaration".into())};
                if block.methods.iter().any(|f|f.name==method.name){return Err(format!("duplicate generated method '{}'",method.name));}
                block.methods.push(method);Ok(Value::Declaration(Item::Impl(block)))}
            "struct_decl"=>{arity!(1);let name=values[0].clone().text()?;valid_ident(&name)?;
                Ok(Value::Declaration(Item::Struct(ast::Struct{name,generics:vec![],fields:vec![],attributes:vec![],is_class:false,parent:None})))}
            "struct_field"=>{arity!(3);let mut s=match values[0].clone().declaration()?{
                Item::Struct(s)=>s,_=>return Err("struct_field requires a struct declaration".into())};
                let name=values[1].clone().text()?;valid_ident(&name)?;
                if s.fields.iter().any(|f|f.name==name){return Err("duplicate generated struct field".into());}
                s.fields.push(ast::Field{name,ty:values[2].clone().ty()?});
                Ok(Value::Declaration(Item::Struct(s)))}
            "declarations"=>{arity!(0);Ok(Value::Declarations(vec![]))}
            "append"=>{arity!(2);let mut items=values[0].clone().into_items()?;
                items.extend(values[1].clone().into_items()?);
                if items.len()>MAX_GENERATED{return Err("error[NOVA-PROC004]: too many generated declarations".into());}
                Ok(Value::Declarations(items))}
            _=>Err(format!("error[NOVA-PROC005]: unknown procedural intrinsic '{kind}'")),
        }
    }
    fn evaluate(&mut self, name:&str, ns:&str, args:Vec<Value>, depth:usize)->Result<Value,String> {
        if depth>MAX_DEPTH{return Err("error[NOVA-PROC004]: macro recursion limit exceeded".into());}
        self.tick()?;
        let resolved=if name.contains("::") {name.to_string()}else{
            let mut scope=ns;
            loop {
                let candidate=qualified(scope,name);
                if self.functions.contains_key(&candidate)||self.intrinsics.contains_key(&candidate){break candidate;}
                if scope.is_empty(){return Err(format!("error[NOVA-PROC005]: compile-time call to non-comptime function '{name}'"));}
                scope=scope.rsplit_once("::").map(|(head,_)|head).unwrap_or("");
            }
        };
        if self.intrinsics.contains_key(&resolved){return self.intrinsic(&resolved,args);}
        let (f,fn_ns)=self.functions.get(&resolved).cloned().ok_or_else(||format!("error[NOVA-PROC005]: '{resolved}' cannot run at compile-time"))?;
        if f.params.len()!=args.len(){return Err(format!("error[NOVA-PROC005]: '{resolved}' argument count mismatch"));}
        if !f.generics.is_empty(){return Err(format!("error[NOVA-PROC005]: generic compile-time function '{resolved}' not yet supported"));}
        let mut vars=f.params.iter().map(|p|p.name.clone()).zip(args).collect();
        self.block(&f.body,&fn_ns,&mut vars,depth)?.ok_or_else(||format!("error[NOVA-PROC005]: '{resolved}' did not return a value"))
    }
    fn expr(&mut self, e:&Expr, ns:&str, vars:&HashMap<String,Value>, depth:usize)->Result<Value,String> {
        self.tick()?;
        match e {
            Expr::Int(n)=>n.parse::<i64>().map(Value::Int).map_err(|_|"invalid macro integer".into()),
            Expr::Bool(v)=>Ok(Value::Bool(*v)),
            Expr::Str(v)=>Ok(Value::Text(v.trim_matches('"').to_string())),
            Expr::Var(v)=>vars.get(v).cloned().ok_or_else(||format!("error[NOVA-PROC005]: unknown compile-time variable '{v}'")),
            Expr::Call(name,args)=>{
                let mut values=Vec::new();for a in args{values.push(self.expr(a,ns,vars,depth)?);}
                self.evaluate(name,ns,values,depth+1)
            },
            Expr::Binary(a,op,b)=>{
                let lhs=self.expr(a,ns,vars,depth)?;
                if *op==ast::BinaryOp::And && !lhs.clone().boolean()?{return Ok(Value::Bool(false));}
                if *op==ast::BinaryOp::Or && lhs.clone().boolean()?{return Ok(Value::Bool(true));}
                let rhs=self.expr(b,ns,vars,depth)?;
                match op {
                    ast::BinaryOp::Add=>match (lhs,rhs){
                        (Value::Text(a),Value::Text(b))=>Ok(Value::Text(a+&b)),
                        (Value::Int(a),Value::Int(b))=>a.checked_add(b).map(Value::Int).ok_or("compile-time overflow".into()),
                        _=>Err("invalid compile-time addition".into()),},
                    ast::BinaryOp::Eq=>Ok(Value::Bool(equal(&lhs,&rhs))),
                    ast::BinaryOp::Ne=>Ok(Value::Bool(!equal(&lhs,&rhs))),
                    ast::BinaryOp::And=>Ok(Value::Bool(lhs.boolean()? && rhs.boolean()?)),
                    ast::BinaryOp::Or=>Ok(Value::Bool(lhs.boolean()? || rhs.boolean()?)),
                    _=>Err("error[NOVA-PROC005]: unsupported procedural expression operation".into()),
                }
            },
            Expr::If{condition,then_body,else_body}=>{
                let yes=self.expr(condition,ns,vars,depth)?.boolean()?;
                let mut scope=vars.clone();
                self.block(if yes{then_body}else{else_body},ns,&mut scope,depth)?
                    .ok_or_else(||"procedural if needs a value".into())
            },
            Expr::Block(body)=>{
                let mut scope=vars.clone();self.block(body,ns,&mut scope,depth)?
                    .ok_or_else(||"procedural block needs a value".into())
            },
            _=>Err("error[NOVA-PROC005]: unsupported procedural expression (no host I/O, unsafe or native calls)".into()),
        }
    }
    fn block(&mut self,stmts:&[Stmt],ns:&str,vars:&mut HashMap<String,Value>,depth:usize)->Result<Option<Value>,String>{
        for stmt in stmts {
            self.tick()?;
            match stmt {
                Stmt::Let{name,init,..}=>{let value=self.expr(init,ns,vars,depth)?;vars.insert(name.clone(),value);}
                Stmt::Set{name,value}=>{
                    if !vars.contains_key(name){return Err(format!("error[NOVA-PROC005]: unknown assignment target '{name}'"));}
                    let next=self.expr(value,ns,vars,depth)?;vars.insert(name.clone(),next);
                },
                Stmt::Return(Some(expr))|Stmt::Tail(expr)=>return self.expr(expr,ns,vars,depth).map(Some),
                Stmt::Expr(expr)=>{self.expr(expr,ns,vars,depth)?;},
                Stmt::ForEach{name,iterable,body}=>{
                    let fields=match self.expr(iterable,ns,vars,depth)? {
                        Value::Fields(fields)=>fields,_=>return Err("procedural foreach requires MetaFields".into()),};
                    for field in fields {
                        self.tick()?;
                        let mut scope=vars.clone();scope.insert(name.clone(),Value::Field(field));
                        if let Some(v)=self.block(body,ns,&mut scope,depth)?{return Ok(Some(v));}
                        for (key,value) in scope {if key!=*name && vars.contains_key(&key){vars.insert(key,value);}}
                    }
                },
                Stmt::For{name,start,end,body}=>{
                    let begin=match self.expr(start,ns,vars,depth)?{Value::Int(i)=>i,_=>return Err("for range needs integer".into())};
                    let end=match self.expr(end,ns,vars,depth)?{Value::Int(i)=>i,_=>return Err("for range needs integer".into())};
                    for i in begin..end {
                        self.tick()?;
                        let mut scope=vars.clone();scope.insert(name.clone(),Value::Int(i));
                        if let Some(v)=self.block(body,ns,&mut scope,depth)?{return Ok(Some(v));}
                        for (key,value) in scope {if key!=*name && vars.contains_key(&key){vars.insert(key,value);}}
                    }
                },
                Stmt::If{condition,then_body,else_body}=>{
                    let yes=self.expr(condition,ns,vars,depth)?.boolean()?;
                    if yes || else_body.is_some() {
                        let mut scope=vars.clone();
                        if let Some(v)=self.block(if yes{then_body}else{else_body.as_deref().unwrap_or(&[])},ns,&mut scope,depth)?{return Ok(Some(v));}
                        for (key,value) in scope{if vars.contains_key(&key){vars.insert(key,value);}}
                    }
                },
                _=>return Err("error[NOVA-PROC005]: unsupported compile-time statement".into()),
            }
        }
        Ok(None)
    }
    fn invoke(&mut self, key:&str, proc:&MacroDef, target:&ast::Struct, arguments:&[String])->Result<Vec<Item>,String>{
        self.steps=0;
        debug_assert!(proc.mode!=Mode::Expression);
        let mut values=vec![Value::Target(target.clone())];
        if proc.function.params.len()==2 {values.push(Value::Arguments(arguments.to_vec()));}
        else if !arguments.is_empty(){return Err(format!("error[NOVA-PROC007]: '{key}' does not accept metadata arguments"));}
        let output=self.evaluate(&qualified(&proc.namespace,&proc.function.name),&proc.namespace,values,0)
            .map_err(|e|format!("error[NOVA-PROC006]: in procedural macro '{key}' on '{}': {e}",target.name))?;
        let result=output.into_items().map_err(|e|format!("error[NOVA-PROC006]: in '{key}': {e}"))?;
        if result.len()>MAX_GENERATED{return Err(format!("error[NOVA-PROC004]: '{key}' generated too many declarations"));}
        self.emitted+=result.len();
        if self.emitted>MAX_GENERATED{return Err("error[NOVA-PROC004]: project exceeded generated declaration limit".into());}
        for item in &result {
            if !matches!(item,Item::Impl(_)|Item::Struct(_)|Item::Function(_)){
                return Err(format!("error[NOVA-PROC006]: '{key}' emitted unsupported declaration"));
            }
        }
        Ok(result)
    }

    fn walk_expr(&mut self, expression: &mut Expr, ns: &str, depth: usize) -> Result<(),String> {
        if depth>MAX_DEPTH{return Err("error[NOVA-PROC004]: macro expansion depth exceeded".into());}
        match expression {
            Expr::MacroCall(name, arguments) => {
                for value in arguments.iter_mut(){self.walk_expr(value,ns,depth+1)?;}
                if let Some((key,proc))=Self::lookup(&self.expressions,ns,name).map(|(a,b)|(a.clone(),b.clone())) {
                    if !proc.public && proc.namespace!=ns {
                        return Err(format!("error[NOVA-PROC007]: private procedural macro '{key}' is not accessible from '{ns}'"));
                    }
                    if arguments.len()!=proc.function.params.len() {return Err(format!("error[NOVA-PROC007]: '{key}!' expects {} AST expressions",proc.function.params.len()));}
                    self.steps=0;
                    let mut caller_identifiers=HashSet::new();
                    for original in arguments.iter() {collect_expr_names(original,&mut caller_identifiers);}
                    let values=arguments.iter().cloned().map(Value::Expression).collect();
                    let result=self.evaluate(&qualified(&proc.namespace,&proc.function.name),&proc.namespace,values,0)
                        .map_err(|e|format!("error[NOVA-PROC006]: in expression macro '{key}!': {e}"))?;
                    let generated=result.expression().map_err(|e|format!("error[NOVA-PROC006]: '{key}!' returned invalid AST: {e}"))?;
                    let mut generated_identifiers=HashSet::new();
                    collect_expr_names(&generated,&mut generated_identifiers);
                    if let Some(new_name)=generated_identifiers.difference(&caller_identifiers).min() {
                        return Err(format!("error[NOVA-PROC010]: '{key}!' introduces unresolved identifier '{new_name}' into caller scope; pass the reference as a macro argument"));
                    }
                    *expression=generated;
                    self.walk_expr(expression,ns,depth+1)?;
                }
            }
            Expr::Unary(_,inner)|Expr::Deref(inner)|Expr::Yield(inner)|Expr::Await(inner)|Expr::Spread(inner)|Expr::Cast(inner,_) => self.walk_expr(inner,ns,depth)?,
            Expr::AddressOf{value,..}=>self.walk_expr(value,ns,depth)?,
            Expr::Binary(a,_,b)|Expr::Index(a,b)=>{self.walk_expr(a,ns,depth)?;self.walk_expr(b,ns,depth)?;},
            Expr::Call(_,args)|Expr::GenericCall(_,_,args)|Expr::Array(args)=>{
                for e in args{self.walk_expr(e,ns,depth)?;}
            }
            Expr::CallIndirect(receiver,args)|Expr::MethodCall(receiver,_,args)=>{
                self.walk_expr(receiver,ns,depth)?;
                for e in args{self.walk_expr(e,ns,depth)?;}
            }
            Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>{
                for (_,e) in fields{self.walk_expr(e,ns,depth)?;}
            }
            Expr::Field(receiver,_)=>self.walk_expr(receiver,ns,depth)?,
            Expr::If{condition,then_body,else_body}=>{
                self.walk_expr(condition,ns,depth)?;
                self.walk_stmts(then_body,ns,depth)?;
                self.walk_stmts(else_body,ns,depth)?;
            }
            Expr::Block(body)=>self.walk_stmts(body,ns,depth)?,
            Expr::Lambda{body,..}=>self.walk_stmts(body,ns,depth)?,
            _=>{},
        }
        Ok(())
    }
    fn walk_stmts(&mut self, statements:&mut [Stmt], ns:&str, depth:usize)->Result<(),String>{
        for stmt in statements {
            match stmt {
                Stmt::Let{init,defer,..}=>{
                    self.walk_expr(init,ns,depth)?;
                    if let Some(expr)=defer{self.walk_expr(expr,ns,depth)?;}
                }
                Stmt::Defer(e)|Stmt::Expr(e)|Stmt::Tail(e)=>self.walk_expr(e,ns,depth)?,
                Stmt::Return(Some(e))=>self.walk_expr(e,ns,depth)?,
                Stmt::Set{value,..}|Stmt::DerefSet{value,..}|Stmt::FieldSet{value,..}|Stmt::CompoundSet{value,..}=>self.walk_expr(value,ns,depth)?,
                Stmt::If{condition,then_body,else_body}=>{
                    self.walk_expr(condition,ns,depth)?;
                    self.walk_stmts(then_body,ns,depth)?;
                    if let Some(body)=else_body{self.walk_stmts(body,ns,depth)?;}
                }
                Stmt::While{condition,body}=>{self.walk_expr(condition,ns,depth)?;self.walk_stmts(body,ns,depth)?;}
                Stmt::Loop(body)|Stmt::Unsafe(body)=>self.walk_stmts(body,ns,depth)?,
                Stmt::For{start,end,body,..}=>{
                    self.walk_expr(start,ns,depth)?;self.walk_expr(end,ns,depth)?;
                    self.walk_stmts(body,ns,depth)?;
                }
                Stmt::ForEach{iterable,body,..}=>{
                    self.walk_expr(iterable,ns,depth)?;self.walk_stmts(body,ns,depth)?;
                }
                Stmt::Match{value,arms}=>{
                    self.walk_expr(value,ns,depth)?;
                    for arm in arms{self.walk_stmts(&mut arm.body,ns,depth)?;}
                }
                _=>{},
            }
        }
        Ok(())
    }
    fn items(&mut self,items:&[Item],ns:&str)->Result<Vec<Item>,String>{
        let mut output=Vec::new();
        for item in items {
            match item {
                Item::Namespace{name,items}=>output.push(Item::Namespace{name:name.clone(),items:self.items(items,&qualified(ns,name))?}),
                Item::Public(inner)=>{
                    let expanded=self.items(std::slice::from_ref(inner.as_ref()),ns)?;
                    for generated in expanded {
                        // Generated impls are never visibility declarations.
                        match generated {
                            Item::Impl(_)=>output.push(generated),
                            other=>output.push(Item::Public(Box::new(other))),
                        }
                    }
                },
                Item::Function(f) if f.attributes.iter().any(|a|matches!(a.name.as_str(),"proc_derive"|"proc_attribute"|"proc_macro"))=>{},
                Item::Struct(source)=>{
                    let mut s=source.clone();
                    let mut generated=Vec::new();
                    let mut consumed=HashSet::new();
                    for attribute in &s.attributes {
                        if let Some((name,proc))=Self::lookup(&self.attributes,ns,&attribute.name).map(|(n,p)|(n.clone(),p.clone())) {
                            if !proc.public && proc.namespace!=ns{return Err(format!("error[NOVA-PROC007]: private attribute macro '{name}' is not accessible from '{ns}'"));}
                            generated.extend(self.invoke(&name,&proc,&s,&attribute.args)?);
                            consumed.insert(attribute.name.clone());
                        }
                    }
                    if let Some(attr)=s.attributes.iter().find(|a|a.name=="derive") {
                        let mut remaining=Vec::new();
                        for derive in &attr.args {
                            if let Some((name,proc))=Self::lookup(&self.derives,ns,derive).map(|(n,p)|(n.clone(),p.clone())) {
                                if !proc.public && proc.namespace!=ns{return Err(format!("error[NOVA-PROC007]: private derive macro '{name}' is not accessible from '{ns}'"));}
                                generated.extend(self.invoke(&name,&proc,&s,&[])?);
                            }else{remaining.push(derive.clone());}
                        }
                        s.attributes.iter_mut().find(|a|a.name=="derive").unwrap().args=remaining;
                        if s.attributes.iter().any(|a|a.name=="derive"&&a.args.is_empty()){consumed.insert("derive".into());}
                    }
                    s.attributes.retain(|a|!consumed.contains(&a.name));
                    output.push(Item::Struct(s));output.extend(generated);
                },
                Item::Function(f)=>{
                    let mut f=f.clone();self.walk_stmts(&mut f.body,ns,0)?;
                    output.push(Item::Function(f));
                }
                Item::Impl(block)=>{
                    let mut block=block.clone();
                    for method in &mut block.methods{self.walk_stmts(&mut method.body,ns,0)?;}
                    output.push(Item::Impl(block));
                }
                Item::Trait(trait_def)=>{
                    let mut trait_def=trait_def.clone();
                    for method in &mut trait_def.methods {
                        if let Some(body)=&mut method.default_body{self.walk_stmts(body,ns,0)?;}
                    }
                    output.push(Item::Trait(trait_def));
                }
                other=>output.push(other.clone()),
            }
        }
        Ok(output)
    }
}

/// Expression macros do not receive implicit access to caller locals. AST
/// identifiers returned by the macro must originate in a supplied AST value.
/// This is deliberately more restrictive than unrestricted token capture;
/// generated methods use their own explicit parameter declarations.
fn collect_expr_names(expr:&Expr,names:&mut HashSet<String>){
    match expr {
        Expr::Var(name)=>{names.insert(name.clone());}
        Expr::Unary(_,v)|Expr::Deref(v)|Expr::Yield(v)|Expr::Await(v)|Expr::Spread(v)|Expr::Cast(v,_)|
        Expr::AddressOf{value:v,..}|Expr::Field(v,_)=>collect_expr_names(v,names),
        Expr::Binary(a,_,b)|Expr::Index(a,b)=>{collect_expr_names(a,names);collect_expr_names(b,names);},
        Expr::Call(_,args)|Expr::GenericCall(_,_,args)|Expr::MacroCall(_,args)|Expr::Array(args)=>{
            for arg in args{collect_expr_names(arg,names);}
        }
        Expr::CallIndirect(receiver,args)|Expr::MethodCall(receiver,_,args)=>{
            collect_expr_names(receiver,names);
            for arg in args{collect_expr_names(arg,names);}
        }
        Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>{
            for (_,v) in fields{collect_expr_names(v,names);}
        }
        Expr::If{condition,then_body,else_body}=>{
            collect_expr_names(condition,names);
            for block in [then_body,else_body] {for s in block {collect_stmt_names(s,names);}}
        }
        Expr::Block(body)|Expr::Lambda{body,..}=>{for s in body{collect_stmt_names(s,names);}}
        _=>{},
    }
}
fn collect_stmt_names(stmt:&Stmt,names:&mut HashSet<String>){
    match stmt{
        Stmt::Let{init,defer,..}=>{collect_expr_names(init,names);if let Some(e)=defer{collect_expr_names(e,names);}}
        Stmt::Expr(e)|Stmt::Tail(e)|Stmt::Defer(e)|Stmt::Return(Some(e))|
        Stmt::Set{value:e,..}|Stmt::DerefSet{value:e,..}|Stmt::FieldSet{value:e,..}|
        Stmt::CompoundSet{value:e,..}=>collect_expr_names(e,names),
        Stmt::If{condition,then_body,else_body}=>{
            collect_expr_names(condition,names);
            for s in then_body{collect_stmt_names(s,names);}
            if let Some(body)=else_body{for s in body{collect_stmt_names(s,names);}}
        }
        Stmt::While{condition,body}=>{collect_expr_names(condition,names);for s in body{collect_stmt_names(s,names);}}
        Stmt::Loop(body)|Stmt::Unsafe(body)=>for s in body{collect_stmt_names(s,names);},
        Stmt::For{start,end,body,..}=>{
            collect_expr_names(start,names);collect_expr_names(end,names);
            for s in body{collect_stmt_names(s,names);}
        }
        Stmt::ForEach{iterable,body,..}=>{
            collect_expr_names(iterable,names);for s in body{collect_stmt_names(s,names);}
        }
        Stmt::Match{value,arms}=>{
            collect_expr_names(value,names);
            for a in arms{for s in &a.body{collect_stmt_names(s,names);}}
        }
        _=>{},
    }
}
fn equal(a:&Value,b:&Value)->bool {
    match (a,b){(Value::Int(a),Value::Int(b))=>a==b,(Value::Text(a),Value::Text(b))=>a==b,
        (Value::Bool(a),Value::Bool(b))=>a==b,_=>false,}
}
fn valid_ident(s:&str)->Result<(),String>{
    let mut chars=s.chars();
    if !matches!(chars.next(),Some(c) if c.is_ascii_alphabetic() || c=='_') ||
        !chars.all(|c|c.is_ascii_alphanumeric()||c=='_') {
        return Err(format!("error[NOVA-PROC008]: invalid generated identifier '{s}'"));
    }
    Ok(())
}
fn valid_type_path(s:&str)->Result<(),String>{
    if s.split("::").all(|part|valid_ident(part).is_ok()){Ok(())}
    else{Err(format!("error[NOVA-PROC008]: invalid generated type path '{s}'"))}
}

pub fn expand(program:&ast::Program)->Result<ast::Program,String>{
    let mut engine=Engine::new(program)?;
    Ok(ast::Program{items:engine.items(&program.items,"")?})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn run(source:&str)->Result<ast::Program,String>{
        let ast=grammar::ProgramParser::new().parse(source).map_err(|e|format!("{e:?}"))?;
        expand(&ast)
    }
    const BUILTINS:&str=r#"
        namespace std { namespace meta {
            struct MetaType {} struct MetaFields {} struct MetaField {} struct MetaTypeRef {} struct MetaExpr {} struct MetaDecl {} struct MetaDecls {}
            #[comptime] #[meta_proc_intrinsic(fields)] fn fields(t:MetaType)->MetaFields{return 0;}
            #[comptime] #[meta_proc_intrinsic(field_name)] fn field_name(t:MetaField)->str{return "";}
            #[comptime] #[meta_proc_intrinsic(field_type_ref)] fn field_type_ref(t:MetaField)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(type_ref)] fn type_ref(t:MetaType)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(reference)] fn reference(t:MetaTypeRef)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(impl_for)] fn impl_for(t:MetaType)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(function)] fn function(n:str,t:MetaTypeRef,e:MetaExpr)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(parameter)] fn parameter(d:MetaDecl,n:str,t:MetaTypeRef)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(method)] fn method(d:MetaDecl,m:MetaDecl)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(variable)] fn variable(n:str)->MetaExpr{return 0;}
            #[comptime] #[meta_proc_intrinsic(access)] fn access(e:MetaExpr,n:str)->MetaExpr{return 0;}
        }}
    "#;
    #[test]
    fn derive_generates_methods_as_typed_ast(){
        let src=format!(r#"{BUILTINS}
            #[comptime] #[proc_derive(Getters)]
            fn derive_getters(target:MetaType)->MetaDecl {{
                let mut block=std::meta::impl_for(target);
                for field in std::meta::fields(target) {{
                    let mut getter=std::meta::function("get_"+std::meta::field_name(field),
                        std::meta::field_type_ref(field),
                        std::meta::access(std::meta::variable("self"),std::meta::field_name(field)));
                    getter=std::meta::parameter(getter,"self",std::meta::reference(std::meta::type_ref(target)));
                    block=std::meta::method(block,getter);
                }}
                block
            }}
            #[derive(Getters)] struct Person {{ id:i64, active:bool }}
        "#);
        let program=run(&src).unwrap();
        assert!(program.items.iter().any(|i|matches!(i,Item::Impl(b) if b.methods.len()==2)));
    }
    #[test]
    fn unsafe_procedural_calls_rejected(){
        let src=format!(r#"{BUILTINS}
            fn danger(t:MetaType)->MetaDecl {{return 0;}}
            #[comptime] #[proc_derive(Untrusted)]
            fn bad(t:MetaType)->MetaDecl {{return danger(t);}}
            #[derive(Untrusted)] struct User {{ id:i64 }}
        "#);
        let error=run(&src).unwrap_err();
        assert!(error.contains("NOVA-PROC005")||error.contains("NOVA-PROC006"),"{error}");
    }
    #[test]
    fn bad_signature_rejected(){
        let src="#[proc_derive(Bad)] fn bad(t:i64)->i64 {return 0;}";
        assert!(run(src).unwrap_err().contains("NOVA-PROC002"));
    }
}
