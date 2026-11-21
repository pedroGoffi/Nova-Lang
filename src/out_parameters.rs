//! C#-style `out` parameters, implemented entirely as an AST transformation.
//!
//! `out x: T` becomes `x: &mut T` at the ABI boundary. Call sites must use
//! `out x` or `out let x`. Callees must assign each output
//! on every normal exit; reading an unassigned output is rejected.
//! No extra runtime helpers, allocations, or native ABI primitives.
use crate::ast::{self, Expr, Item, Stmt, TypeRef};
use std::collections::{HashMap, HashSet};

const OUT_TYPE: &str = "__nova_internal_out_type";
const OUT_ARG: &str = "__nova_internal_out_argument";
const OUT_DECL: &str = "__nova_internal_out_declare";
const NAMED: &str = "__nova_internal_named_arg";

fn out_type(ty: &TypeRef) -> Option<&TypeRef> {
    match ty {
        TypeRef::Applied(name, args) if name == OUT_TYPE && args.len() == 1 => Some(&args[0]),
        _ => None,
    }
}
fn qualified(ns: &str, name: &str) -> String {
    if ns.is_empty() { name.to_owned() } else { format!("{ns}::{name}") }
}
fn base_type(ty: &TypeRef) -> Option<&str> {
    match ty {
        TypeRef::Named(s) | TypeRef::Applied(s, _) => Some(s),
        TypeRef::Reference{inner,..} | TypeRef::Pointer{inner,..} => base_type(inner),
        _ => None,
    }
}
fn is_out_marker(e: &Expr) -> bool {
    matches!(e, Expr::Call(name, _) if name==OUT_ARG || name==OUT_DECL)
}
fn out_target(e: &Expr) -> Option<&str> {
    match e {
        Expr::Call(name,args) if name == OUT_ARG && args.len()==1 =>
            match &args[0] { Expr::Var(name)=>Some(name), _=>None },
        Expr::Call(name,args) if name == OUT_DECL && args.len()==2 =>
            match &args[0] { Expr::Var(name)=>Some(name), _=>None },
        _ => None,
    }
}

#[derive(Clone)]
struct Signature {
    params: Vec<ast::Param>,
    generics: Vec<String>,
    owner: Option<TypeRef>,
}
impl Signature {
    fn contains_out(&self) -> bool { self.params.iter().any(|p|out_type(&p.ty).is_some()) }
}
#[derive(Default)]
struct Registry {
    functions: HashMap<String,Signature>,
    methods: Vec<(String,Signature)>,
}
impl Registry {
    fn collect(&mut self, items: &[Item], ns: &str) -> Result<(),String> {
        for item in items {
            match item {
                Item::Public(inner) => self.collect(std::slice::from_ref(inner.as_ref()),ns)?,
                Item::Namespace{name,items} => self.collect(items,&qualified(ns,name))?,
                Item::Function(f) => {
                    self.validate(&f.name,&f.params,f.variadic)?;
                    self.functions.insert(qualified(ns,&f.name),Signature {
                        params:f.params.clone(),generics:f.generics.clone(),owner:None
                    });
                }
                Item::Impl(block) => for method in &block.methods {
                    self.validate(&method.name,&method.params,method.variadic)?;
                    let sig=Signature{
                        params:method.params.clone(),
                        generics:block.generics.iter().chain(&method.generics).cloned().collect(),
                        owner:Some(block.target.clone())
                    };
                    let owner=base_type(&block.target).unwrap_or("");
                    if method.attributes.iter().any(|a|a.name=="static") {
                        self.functions.insert(qualified(ns,&format!("{owner}::{}",method.name)),sig);
                    } else {
                        self.methods.push((qualified(ns,&method.name),sig));
                    }
                },
                Item::Trait(t) => {
                    if t.methods.iter().any(|m|m.params.iter().any(|p|out_type(&p.ty).is_some())) {
                        return Err(format!("error[NOVA-OUT009]: `out` in trait '{}' is not supported yet",t.name));
                    }
                }
                Item::Extern(f) => {
                    if f.params.iter().any(|p|out_type(&p.ty).is_some()) {
                        return Err(format!("error[NOVA-OUT009]: `out` is not supported for extern fn '{}'",f.name));
                    }
                }
                _=>{}
            }
        }
        Ok(())
    }
    fn validate(&self, name:&str, params:&[ast::Param], variadic:bool) -> Result<(),String> {
        for (index,p) in params.iter().enumerate() {
            if out_type(&p.ty).is_some() {
                if p.name=="self" || variadic && index==params.len()-1 {
                    return Err(format!("error[NOVA-OUT009]: invalid `out` receiver or variadic parameter '{}' in '{name}'",p.name));
                }
                if p.default.is_some() { return Err(format!("error[NOVA-OUT009]: `out {}` cannot have a default",p.name)); }
            }
        }
        Ok(())
    }
    fn function(&self,name:&str,ns:&str) -> Option<Signature> {
        if name.contains("::") { return self.functions.get(name).cloned(); }
        let mut here=ns;
        loop {
            if let Some(sig)=self.functions.get(&qualified(here,name)) {return Some(sig.clone());}
            if here.is_empty() {break;}
            here=here.rsplit_once("::").map(|x|x.0).unwrap_or("");
        }
        None
    }
    fn method(&self,name:&str,ns:&str,receiver:Option<&TypeRef>,arity:usize,contains_out:bool)
        -> Result<Option<Signature>,String> {
        let mut candidates:Vec<_>=self.methods.iter().filter(|(key,sig)| {
            key.rsplit("::").next()==Some(name) &&
            sig.params.len().saturating_sub(1)==arity &&
            (key==&qualified(ns,name) || key.ends_with(&format!("::{name}")) || key==name)
        }).map(|(_,s)|s.clone()).collect();
        if let Some(got)=receiver.and_then(base_type) {
            let filtered:Vec<_>=candidates.iter().filter(|s| {
                s.owner.as_ref().and_then(base_type).is_some_and(|expected|
                    expected==got || got.ends_with(&format!("::{expected}")))
            }).cloned().collect();
            if !filtered.is_empty() {candidates=filtered;}
        }
        if candidates.is_empty() {return if contains_out {Err(format!("error[NOVA-OUT006]: cannot resolve output method '{name}'"))} else {Ok(None)};}
        if candidates.len()==1 {return Ok(candidates.pop());}
        // If the receiver is not yet typed, only a unique output contract is safe.
        let contract=|s:&Signature|s.params.iter().skip(1).map(|p|out_type(&p.ty).cloned()).collect::<Vec<_>>();
        if candidates.iter().all(|s|contract(s)==contract(&candidates[0])) {
            return Ok(Some(candidates.remove(0)));
        }
        if contains_out {
            return Err(format!("error[NOVA-OUT006]: ambiguous `out` method '{name}'; qualify the receiver or use a function"));
        }
        // The typed surface pass will resolve ordinary overloaded methods.
        // Do not reject a non-out call merely because some other receiver's
        // method of the same name happens to have an `out` parameter.
        Ok(None)
    }
}

#[derive(Clone)]
struct Flow {
    assigned: HashSet<String>,
    reachable: bool,
}
struct Checker<'a> {
    registry: &'a Registry,
    namespace: &'a str,
    outputs: HashSet<String>,
}
impl Checker<'_> {
    fn read(&self,name:&str,state:&Flow) -> Result<(),String> {
        if self.outputs.contains(name) && !state.assigned.contains(name) {
            return Err(format!("error[NOVA-OUT002]: `out {name}` is read before being assigned"));
        }
        Ok(())
    }
    fn require(&self,state:&Flow) -> Result<(),String> {
        if !state.reachable {return Ok(());}
        let mut missing=self.outputs.difference(&state.assigned).collect::<Vec<_>>();
        missing.sort();
        if !missing.is_empty() {
            return Err(format!("error[NOVA-OUT001]: output parameter(s) {} must be assigned on every return path",missing.iter().map(|n|format!("'{}'",n)).collect::<Vec<_>>().join(", ")));
        }
        Ok(())
    }
    fn call(&self,sig:Option<Signature>,args:&[Expr],offset:usize,state:&mut Flow) -> Result<(),String> {
        let mut written=Vec::new();
        if let Some(sig)=sig {
            for (index,arg) in args.iter().enumerate() {
                let expected=sig.params.get(index+offset).and_then(|p|out_type(&p.ty));
                if expected.is_some() && !is_out_marker(arg) {
                    return Err(format!("error[NOVA-OUT003]: argument '{}' requires `out`",sig.params[index+offset].name));
                }
                if expected.is_none() && is_out_marker(arg) {
                    return Err("error[NOVA-OUT004]: `out` supplied for a non-out parameter".into());
                }
                if is_out_marker(arg) {
                    if let Some(name)=out_target(arg) {
                        written.push(name.to_owned());
                    } else if let Expr::Call(_,values)=arg {
                        // `out object.field` requires an already initialized root.
                        if let Some(Expr::Field(root,_)|Expr::Index(root,_))=values.first() {self.expr(root,state)?;}
                    }
                } else {self.expr(arg,state)?;}
            }
        } else {
            for arg in args {
                if is_out_marker(arg) {return Err("error[NOVA-OUT006]: output argument requires a known function signature".into());}
                self.expr(arg,state)?;
            }
        }
        for name in written {if self.outputs.contains(&name) {state.assigned.insert(name);}}
        Ok(())
    }
    fn expr(&self,e:&Expr,state:&mut Flow) -> Result<(),String> {
        match e {
            Expr::Var(n)=>self.read(n,state)?,
            Expr::Call(name,args) => {
                if name==OUT_ARG || name==OUT_DECL {return Err("error[NOVA-OUT005]: `out` may only appear as a call argument".into());}
                self.call(self.registry.function(name,self.namespace),args,0,state)?;
            }
            Expr::GenericCall(name,_,args)=>self.call(self.registry.function(name,self.namespace),args,0,state)?,
            Expr::MethodCall(receiver,name,args,_) => {
                self.expr(receiver,state)?;
                let sig=self.registry.method(name,self.namespace,None,args.len(),args.iter().any(is_out_marker))?;
                self.call(sig,args,1,state)?;
            }
            Expr::CallIndirect(fun,args)=> {
                self.expr(fun,state)?;
                self.call(None,args,0,state)?;
            }
            Expr::Binary(a,op,b) => {
                self.expr(a,state)?;
                if matches!(op,ast::BinaryOp::And|ast::BinaryOp::Or) {
                    let mut optional=state.clone(); self.expr(b,&mut optional)?;
                } else {self.expr(b,state)?;}
            }
            Expr::If{condition,then_body,else_body} => {
                self.expr(condition,state)?;
                let mut left=state.clone(); let mut right=state.clone();
                self.statements(then_body,&mut left)?;self.statements(else_body,&mut right)?;
                *state=Self::join(left,right);
            }
            Expr::Block(body)=>self.statements(body,state)?,
            Expr::Lambda{body,..}=>{
                // Capturing an uninitialized output across a closure boundary is unsafe.
                let mut child=state.clone();self.statements(body,&mut child)?;
            }
            Expr::Unary(_,v)|Expr::Field(v,_)|Expr::AddressOf{value:v,..}|
            Expr::Deref(v)|Expr::Cast(v,_)|Expr::Spread(v)|Expr::Yield(v)|Expr::Await(v)=>self.expr(v,state)?,
            Expr::Index(a,b)=>{self.expr(a,state)?;self.expr(b,state)?;}
            Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>for (_,v) in fields {self.expr(v,state)?;},
            Expr::Array(values)|Expr::MacroCall(_,values)=>for v in values {self.expr(v,state)?;},
            _=>{}
        }
        Ok(())
    }
    fn join(a:Flow,b:Flow)->Flow {
        match (a.reachable,b.reachable) {
            (true,true)=>Flow{assigned:a.assigned.intersection(&b.assigned).cloned().collect(),reachable:true},
            (true,false)=>a,
            (false,true)=>b,
            (false,false)=>Flow{assigned:HashSet::new(),reachable:false}
        }
    }
    fn statements(&self,body:&[Stmt],state:&mut Flow)->Result<(),String> {
        for stmt in body {
            if !state.reachable {break;}
            match stmt {
                Stmt::Let{name,init,defer,..} => {
                    if self.outputs.contains(name) {return Err(format!("error[NOVA-OUT008]: cannot shadow output parameter '{name}'"));}
                    self.expr(init,state)?;
                    if let Some(v)=defer {self.expr(v,state)?;}
                }
                Stmt::Set{name,value}=>{self.expr(value,state)?;if self.outputs.contains(name){state.assigned.insert(name.clone());}}
                Stmt::CompoundSet{name,value,..}=>{self.read(name,state)?;self.expr(value,state)?;}
                Stmt::FieldSet{root,value,..}=>{self.read(root,state)?;self.expr(value,state)?;}
                Stmt::DerefSet{name,value}=>{self.read(name,state)?;self.expr(value,state)?;}
                Stmt::Return(value)=>{
                    if let Some(e)=value {self.expr(e,state)?;}
                    self.require(state)?; state.reachable=false;
                }
                Stmt::Tail(e)=>{self.expr(e,state)?;self.require(state)?;state.reachable=false;}
                Stmt::If{condition,then_body,else_body}=>{
                    self.expr(condition,state)?;
                    let mut yes=state.clone();let mut no=state.clone();
                    self.statements(then_body,&mut yes)?;
                    if let Some(body)=else_body {self.statements(body,&mut no)?;}
                    *state=Self::join(yes,no);
                }
                Stmt::Match{value,arms}=>{
                    self.expr(value,state)?;
                    let mut outcome:Option<Flow>=None;
                    for arm in arms {
                        let mut branch=state.clone();self.statements(&arm.body,&mut branch)?;
                        outcome=Some(if let Some(existing)=outcome {Self::join(existing,branch)} else {branch});
                    }
                    // Without a wildcard, a nonexhaustive match may skip all arms.
                    if arms.iter().any(|a|matches!(&a.pattern,ast::Pattern::Wildcard)) {
                        if let Some(result)=outcome {*state=result;}
                    }
                }
                Stmt::While{condition,body}=>{self.expr(condition,state)?;let mut branch=state.clone();self.statements(body,&mut branch)?;}
                Stmt::For{start,end,body,..}=>{self.expr(start,state)?;self.expr(end,state)?;let mut branch=state.clone();self.statements(body,&mut branch)?;}
                Stmt::ForEach{iterable,body,..}=>{self.expr(iterable,state)?;let mut branch=state.clone();self.statements(body,&mut branch)?;}
                Stmt::Loop(body)|Stmt::Unsafe(body)=>{
                    let mut branch=state.clone();self.statements(body,&mut branch)?;
                    // Unsafe is lexical, not optional execution.
                    if matches!(stmt,Stmt::Unsafe(_)) {*state=branch;}
                }
                Stmt::Defer(v)|Stmt::Expr(v)=>self.expr(v,state)?,
                Stmt::Break|Stmt::Continue=>{state.reachable=false;}
            }
        }
        Ok(())
    }
}

struct Rewriter<'a> {
    registry: &'a Registry,
    namespace: &'a str,
    out_params: HashSet<String>,
    locals: HashMap<String,TypeRef>,
}
impl Rewriter<'_> {
    fn subst(ty:&TypeRef,generics:&[String],args:&[TypeRef]) -> TypeRef {
        match ty {
            TypeRef::Named(name) => generics.iter().position(|g|g==name)
                .and_then(|i|args.get(i)).cloned().unwrap_or_else(||ty.clone()),
            TypeRef::Applied(name,vals)=>TypeRef::Applied(name.clone(),vals.iter().map(|v|Self::subst(v,generics,args)).collect()),
            TypeRef::Reference{mutable,inner}=>TypeRef::Reference{mutable:*mutable,inner:Box::new(Self::subst(inner,generics,args))},
            TypeRef::Pointer{mutable,inner}=>TypeRef::Pointer{mutable:*mutable,inner:Box::new(Self::subst(inner,generics,args))},
            TypeRef::Array(inner,n)=>TypeRef::Array(Box::new(Self::subst(inner,generics,args)),*n),
            TypeRef::Function{params,result}=>TypeRef::Function{params:params.iter().map(|v|Self::subst(v,generics,args)).collect(),result:Box::new(Self::subst(result,generics,args))},
        }
    }
    fn call_args(&mut self,args:&mut [Expr],sig:Option<Signature>,offset:usize,generic_args:&[TypeRef],prefix:&mut Vec<Stmt>) -> Result<(),String> {
        // `out` is mandatory for every out position, and never allowed for others.
        let mut used=HashSet::new();
        for (index,arg) in args.iter_mut().enumerate() {
            let expected=sig.as_ref().and_then(|s|s.params.get(index+offset)).and_then(|p|out_type(&p.ty));
            if expected.is_some() && !is_out_marker(arg) {
                return Err(format!("error[NOVA-OUT003]: parameter '{}' must be passed with `out`",sig.as_ref().unwrap().params[index+offset].name));
            }
            if expected.is_none() && is_out_marker(arg) {
                return Err("error[NOVA-OUT004]: `out` used on a parameter that is not declared `out`".into());
            }
            if let Some(ty)=expected {
                let signature=sig.as_ref().unwrap();
                let ty=Self::subst(ty,&signature.generics,generic_args);
                let Expr::Call(marker,vals)=arg else {unreachable!()};
                if marker==OUT_DECL {
                    if vals.len()!=2 {return Err("error[NOVA-OUT005]: malformed output declaration".into());}
                    let Expr::Var(name)=&vals[0] else {return Err("error[NOVA-OUT005]: output declaration requires an identifier".into());};
                    let name=name.clone();
                    if self.locals.contains_key(&name) {return Err(format!("error[NOVA-OUT007]: output local '{name}' already exists"));}
                    if !used.insert(name.clone()) {return Err(format!("error[NOVA-OUT007]: duplicated output alias '{name}'"));}
                    let declared=match &vals[1] {Expr::Zero(t)=>Some(t.clone()),Expr::Unit=>None,_=>return Err("error[NOVA-OUT005]: invalid output declaration type".into())};
                    let value_ty=match declared {
                        // Alias equivalence and concrete type compatibility are
                        // checked by the existing typed surface/HIR passes.
                        Some(explicit) => explicit,
                        None => {
                            if signature.generics.iter().any(|g|contains_type_parameter(&ty,g)) {
                                return Err(format!("error[NOVA-OUT010]: cannot infer output type of '{name}'; write `out let {name}: T` or provide explicit generics"));
                            }
                            ty
                        }
                    };
                    self.locals.insert(name.clone(),value_ty.clone());
                    prefix.push(Stmt::Let{name:name.clone(),mutable:true,ty:Some(value_ty.clone()),init:Expr::Zero(value_ty),defer:None});
                    *arg=Expr::AddressOf{mutable:true,value:Box::new(Expr::Var(name))};
                } else {
                    if vals.len()!=1 {return Err("error[NOVA-OUT005]: malformed output argument".into());}
                    let place=vals[0].clone();
                    if let Expr::Var(name)=&place {
                        if !used.insert(name.clone()) {return Err(format!("error[NOVA-OUT007]: duplicated output alias '{name}'"));}
                        if self.out_params.contains(name) {
                            *arg=Expr::Var(name.clone()); // forward an existing &mut T, not &&mut T
                            continue;
                        }
                    }
                    if !matches!(&place,Expr::Var(_)|Expr::Field(_,_)|Expr::Index(_,_)) {
                        return Err("error[NOVA-OUT005]: `out` requires an assignable variable, field or index".into());
                    }
                    let place=self.expr(place,prefix)?;
                    *arg=Expr::AddressOf{mutable:true,value:Box::new(place)};
                }
            } else {
                *arg=self.expr(arg.clone(),prefix)?;
            }
        }
        Ok(())
    }
    fn expr(&mut self,e:Expr,prefix:&mut Vec<Stmt>) -> Result<Expr,String> {
        Ok(match e {
            Expr::Var(name) if self.out_params.contains(&name) =>Expr::Deref(Box::new(Expr::Var(name))),
            Expr::Call(name,mut args)=>{
                if name==OUT_ARG||name==OUT_DECL||name==NAMED {return Err("error[NOVA-OUT005]: output/named marker outside a regular call".into());}
                let sig=self.registry.function(&name,self.namespace);
                self.call_args(&mut args,sig,0,&[],prefix)?;
                Expr::Call(name,args)
            }
            Expr::GenericCall(name,types,mut args)=>{
                let sig=self.registry.function(&name,self.namespace);
                self.call_args(&mut args,sig,0,&types,prefix)?;
                Expr::GenericCall(name,types,args)
            }
            Expr::MethodCall(receiver,name,mut args,types)=>{
                let got=match receiver.as_ref() {Expr::Var(n)=>self.locals.get(n).cloned(),_=>None};
                let sig=self.registry.method(&name,self.namespace,got.as_ref(),args.len(),args.iter().any(is_out_marker))?;
                let receiver=Box::new(self.expr(*receiver,prefix)?);
                self.call_args(&mut args,sig,1,&types,prefix)?;
                Expr::MethodCall(receiver,name,args,types)
            }
            Expr::CallIndirect(fun,mut args)=>{
                let fun=Box::new(self.expr(*fun,prefix)?);
                self.call_args(&mut args,None,0,&[],prefix)?;
                Expr::CallIndirect(fun,args)
            }
            Expr::Binary(a,op,b)=>{
                let a=Box::new(self.expr(*a,prefix)?);
                let mut rhs=Vec::new();let b=Box::new(self.expr(*b,&mut rhs)?);
                if !rhs.is_empty() && matches!(op,ast::BinaryOp::And|ast::BinaryOp::Or) {
                    return Err("error[NOVA-OUT011]: `out let` in short-circuit expression needs a separate statement".into());
                }
                prefix.extend(rhs); Expr::Binary(a,op,b)
            }
            Expr::If{condition,then_body,else_body}=>{
                let condition=Box::new(self.expr(*condition,prefix)?);
                let then_body=self.block(then_body)?;let else_body=self.block(else_body)?;
                Expr::If{condition,then_body,else_body}
            }
            Expr::Block(body)=>Expr::Block(self.block(body)?),
            Expr::Lambda{params,result,body}=>{
                if params.iter().any(|p|out_type(&p.ty).is_some()) {
                    return Err("error[NOVA-OUT009]: output parameters in lambdas are not supported yet".into());
                }
                // A lifted lambda is a separate scope. Out parameters of its
                // enclosing function may not be captured before initialization.
                let prior=self.out_params.clone();
                for p in &params {self.out_params.remove(&p.name);}
                let body=self.block(body)?;self.out_params=prior;
                Expr::Lambda{params,result,body}
            }
            Expr::Unary(op,v)=>Expr::Unary(op,Box::new(self.expr(*v,prefix)?)),
            Expr::Field(v,name)=>Expr::Field(Box::new(self.expr(*v,prefix)?),name),
            Expr::Index(a,b)=>Expr::Index(Box::new(self.expr(*a,prefix)?),Box::new(self.expr(*b,prefix)?)),
            Expr::AddressOf{mutable,value}=>Expr::AddressOf{mutable,value:Box::new(self.expr(*value,prefix)?)},
            Expr::Deref(v)=>Expr::Deref(Box::new(self.expr(*v,prefix)?)),
            Expr::Cast(v,t)=>Expr::Cast(Box::new(self.expr(*v,prefix)?),t),
            Expr::Spread(v)=>Expr::Spread(Box::new(self.expr(*v,prefix)?)),
            Expr::Yield(v)=>Expr::Yield(Box::new(self.expr(*v,prefix)?)),
            Expr::Await(v)=>Expr::Await(Box::new(self.expr(*v,prefix)?)),
            Expr::Array(values)=>Expr::Array(values.into_iter().map(|v|self.expr(v,prefix)).collect::<Result<_,_>>()?),
            Expr::MacroCall(name,values)=>Expr::MacroCall(name,values.into_iter().map(|v|self.expr(v,prefix)).collect::<Result<_,_>>()?),
            Expr::StructInit(name,values)=>Expr::StructInit(name,values.into_iter().map(|(k,v)|Ok((k,self.expr(v,prefix)?))).collect::<Result<_,String>>()?),
            Expr::GenericStructInit(name,types,values)=>Expr::GenericStructInit(name,types,values.into_iter().map(|(k,v)|Ok((k,self.expr(v,prefix)?))).collect::<Result<_,String>>()?),
            other=>other
        })
    }
    fn block(&mut self,body:Vec<Stmt>)->Result<Vec<Stmt>,String> {
        let saved=self.locals.clone();
        let result=self.statements(body);
        self.locals=saved;
        result
    }
    fn statements(&mut self,body:Vec<Stmt>)->Result<Vec<Stmt>,String> {
        let mut result=Vec::new();
        for statement in body {
            let mut prefix=Vec::new();
            let statement=match statement {
                Stmt::Let{name,mutable,ty,init,defer}=>{
                    let init=self.expr(init,&mut prefix)?;
                    let defer=defer.map(|v|self.expr(v,&mut prefix)).transpose()?;
                    if let Some(ty)=ty.clone().or_else(||guess_type(&init)) {self.locals.insert(name.clone(),ty);}
                    Stmt::Let{name,mutable,ty,init,defer}
                }
                Stmt::Set{name,value}=>{
                    let value=self.expr(value,&mut prefix)?;
                    if self.out_params.contains(&name) {Stmt::DerefSet{name,value}}
                    else {Stmt::Set{name,value}}
                }
                Stmt::CompoundSet{name,op,value}=>{
                    let value=self.expr(value,&mut prefix)?;
                    if self.out_params.contains(&name) {
                        let current=Expr::Deref(Box::new(Expr::Var(name.clone())));
                        Stmt::DerefSet{name,value:Expr::Binary(Box::new(current),op,Box::new(value))}
                    } else {Stmt::CompoundSet{name,op,value}}
                }
                Stmt::FieldSet{root,field,op,value}=>Stmt::FieldSet{root,field,op,value:self.expr(value,&mut prefix)?},
                Stmt::DerefSet{name,value}=>Stmt::DerefSet{name,value:self.expr(value,&mut prefix)?},
                Stmt::Return(v)=>Stmt::Return(v.map(|v|self.expr(v,&mut prefix)).transpose()?),
                Stmt::Expr(v)=>Stmt::Expr(self.expr(v,&mut prefix)?),
                Stmt::Tail(v)=>Stmt::Tail(self.expr(v,&mut prefix)?),
                Stmt::Defer(v)=>Stmt::Defer(self.expr(v,&mut prefix)?),
                Stmt::If{condition,then_body,else_body}=>{
                    let condition=self.expr(condition,&mut prefix)?;
                    let then_body=self.block(then_body)?;
                    let else_body=else_body.map(|b|self.block(b)).transpose()?;
                    Stmt::If{condition,then_body,else_body}
                }
                Stmt::While{condition,body}=>{
                    let condition=self.expr(condition,&mut prefix)?;
                    Stmt::While{condition,body:self.block(body)?}
                }
                Stmt::Loop(body)=>Stmt::Loop(self.block(body)?),
                Stmt::Unsafe(body)=>Stmt::Unsafe(self.block(body)?),
                Stmt::For{name,start,end,body}=>{
                    let start=self.expr(start,&mut prefix)?;let end=self.expr(end,&mut prefix)?;
                    Stmt::For{name,start,end,body:self.block(body)?}
                }
                Stmt::ForEach{name,iterable,body}=>{
                    let iterable=self.expr(iterable,&mut prefix)?;
                    Stmt::ForEach{name,iterable,body:self.block(body)?}
                }
                Stmt::Match{value,arms}=>{
                    let value=self.expr(value,&mut prefix)?;
                    let arms=arms.into_iter().map(|mut a|{a.body=self.block(a.body)?;Ok(a)}).collect::<Result<_,String>>()?;
                    Stmt::Match{value,arms}
                }
                other=>other
            };
            result.extend(prefix);result.push(statement);
        }
        Ok(result)
    }
}
fn contains_type_parameter(ty:&TypeRef,param:&str)->bool {
    match ty {
        TypeRef::Named(n)=>n==param,
        TypeRef::Applied(_,args)|TypeRef::Function{params:args,..}=>args.iter().any(|x|contains_type_parameter(x,param)),
        TypeRef::Pointer{inner,..}|TypeRef::Reference{inner,..}|TypeRef::Array(inner,_)=>contains_type_parameter(inner,param),
    }
}
fn guess_type(init:&Expr)->Option<TypeRef>{
    match init {
        Expr::Zero(t)|Expr::Cast(_,t)=>Some(t.clone()),
        Expr::StructInit(name,_)=>Some(TypeRef::Named(name.clone())),
        Expr::Call(name,_) if name.ends_with("::new") =>
            Some(TypeRef::Named(name.trim_end_matches("::new").to_owned())),
        Expr::GenericStructInit(name,args,_)=>Some(TypeRef::Applied(name.clone(),args.clone())),
        Expr::AddressOf{mutable,value}=>guess_type(value).map(|ty|TypeRef::Reference{mutable:*mutable,inner:Box::new(ty)}),
        _=>None,
    }
}
fn process_function(f:&mut ast::Function,ns:&str,registry:&Registry)->Result<(),String> {
    let outputs=f.params.iter().filter(|p|out_type(&p.ty).is_some()).map(|p|p.name.clone()).collect::<HashSet<_>>();
    if !outputs.is_empty() {
        if !matches!(f.mode,ast::FunctionMode::Normal) {
            return Err(format!("error[NOVA-OUT009]: `out` requires a normal function ('{}')",f.name));
        }
        let check=Checker{registry,namespace:ns,outputs:outputs.clone()};
        let mut flow=Flow{assigned:HashSet::new(),reachable:true};
        check.statements(&f.body,&mut flow)?;
        check.require(&flow)?;
    }
    let mut writer=Rewriter{registry,namespace:ns,out_params:outputs,locals:f.params.iter().map(|p|{
        (p.name.clone(),out_type(&p.ty).cloned().unwrap_or_else(||p.ty.clone()))
    }).collect()};
    f.body=writer.statements(std::mem::take(&mut f.body))?;
    for p in &mut f.params {
        if let Some(ty)=out_type(&p.ty) {p.ty=TypeRef::Reference{mutable:true,inner:Box::new(ty.clone())};}
    }
    Ok(())
}
fn process_items(items:&mut [Item],ns:&str,registry:&Registry)->Result<(),String> {
    for item in items {
        match item {
            Item::Function(f)=>process_function(f,ns,registry)?,
            Item::Impl(block)=>for f in &mut block.methods {process_function(f,ns,registry)?;},
            Item::Namespace{name,items}=>process_items(items,&qualified(ns,name),registry)?,
            Item::Public(inner)=>process_items(std::slice::from_mut(inner.as_mut()),ns,registry)?,
            _=>{}
        }
    }
    Ok(())
}
pub fn lower(program:&ast::Program)->Result<ast::Program,String> {
    let mut registry=Registry::default();
    registry.collect(&program.items,"")?;
    let mut lowered=program.clone();
    process_items(&mut lowered.items,"",&registry)?;
    Ok(lowered)
}
