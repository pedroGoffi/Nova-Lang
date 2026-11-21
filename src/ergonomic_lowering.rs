//! Zero-runtime desugaring for `?`, `?.` and `??`.
//!
//! Expressions with control flow are continued inside ordinary typed `match`
//! arms. This preserves laziness and transfers ownership exactly once. In
//! particular, `fallback` of `??` is never evaluated on the `Some` path.
use crate::ast::{self, Expr, Item, MatchArm, Pattern, Stmt, TypeRef};
use std::rc::Rc;

type Continuation = Rc<dyn Fn(&mut Lowerer, Expr) -> Result<Vec<Stmt>, String>>;
type ArgContinuation = Rc<dyn Fn(&mut Lowerer, Vec<Expr>) -> Result<Vec<Stmt>, String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReturnKind { Optional, Outcome, Other }

impl ReturnKind {
    fn from_type(ty: &TypeRef) -> Self {
        let name = match ty {
            TypeRef::Applied(name, _) | TypeRef::Named(name) => name.as_str(),
            _ => return Self::Other,
        };
        match name.rsplit("::").next().unwrap_or(name) {
            "Option" => Self::Optional,
            "Result" => Self::Outcome,
            _ => Self::Other,
        }
    }
}

struct Lowerer {
    counter: usize,
    result: ReturnKind,
}

impl Lowerer {
    fn new(result: &TypeRef) -> Self {
        Self { counter: 0, result: ReturnKind::from_type(result) }
    }

    fn name(&mut self) -> String {
        let index=self.counter;
        self.counter+=1;
        format!("__nova_ergonomic_{index}")
    }

    fn bind(name:String, init:Expr) -> Stmt {
        Stmt::Let{name,mutable:false,ty:None,init,defer:None}
    }

    fn enum_arm(name:&str,bindings:Vec<String>,body:Vec<Stmt>) -> MatchArm {
        MatchArm{pattern:Pattern::Variant{path:name.into(),bindings},body}
    }

    fn any_marker(e:&Expr) -> bool {
        match e {
            Expr::Call(name, _) if name.starts_with("__nova_internal_try")
                || name.starts_with("__nova_internal_coalesce")
                || name.starts_with("__nova_internal_optional_") => true,
            Expr::Call(_,xs) | Expr::MacroCall(_,xs) | Expr::GenericCall(_,_,xs) | Expr::Array(xs) =>
                xs.iter().any(Self::any_marker),
            Expr::CallIndirect(f,xs) => Self::any_marker(f) || xs.iter().any(Self::any_marker),
            Expr::Binary(a,_,b) | Expr::Index(a,b) => Self::any_marker(a)||Self::any_marker(b),
            Expr::MethodCall(value,_,args,_) => Self::any_marker(value)||args.iter().any(Self::any_marker),
            Expr::StructInit(_,fields) | Expr::GenericStructInit(_,_,fields) =>
                fields.iter().any(|(_,v)|Self::any_marker(v)),
            Expr::Unary(_,v) | Expr::Cast(v,_) | Expr::Field(v,_) | Expr::AddressOf{value:v,..}
                | Expr::Deref(v) | Expr::Spread(v) | Expr::Yield(v) | Expr::Await(v) => Self::any_marker(v),
            Expr::If{condition,then_body,else_body} => Self::any_marker(condition) ||
                Self::stmts_have_markers(then_body) || Self::stmts_have_markers(else_body),
            Expr::Block(body) => Self::stmts_have_markers(body),
            Expr::Var(name) if name=="__nova_internal_match_value" => true,
            Expr::Lambda{..} => false, // Lowered in its own function/return context.
            _ => false,
        }
    }

    fn stmts_have_markers(body:&[Stmt]) -> bool {
        body.iter().any(|stmt|match stmt {
            Stmt::Let{init,..} => Self::any_marker(init),
            Stmt::Expr(x)|Stmt::Tail(x)|Stmt::Defer(x)|Stmt::Return(Some(x)) => Self::any_marker(x),
            Stmt::If{condition,then_body,else_body} => Self::any_marker(condition) ||
                Self::stmts_have_markers(then_body) || else_body.as_ref().is_some_and(|b|Self::stmts_have_markers(b)),
            Stmt::While{condition,body}=>Self::any_marker(condition)||Self::stmts_have_markers(body),
            Stmt::For{start,end,body,..}=>Self::any_marker(start)||Self::any_marker(end)||Self::stmts_have_markers(body),
            Stmt::ForEach{iterable,body,..}=>Self::any_marker(iterable)||Self::stmts_have_markers(body),
            Stmt::Loop(body)|Stmt::Unsafe(body)=>Self::stmts_have_markers(body),
            Stmt::Set{value,..}|Stmt::DerefSet{value,..}|Stmt::CompoundSet{value,..}
                |Stmt::FieldSet{value,..}=>Self::any_marker(value),
            Stmt::Match{value,arms}=>Self::any_marker(value)||arms.iter().any(|a|Self::stmts_have_markers(&a.body)),
            _ => false,
        })
    }

    fn is_atom(expr:&Expr) -> bool {
        matches!(expr,Expr::Var(_) | Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Unit)
    }

    // Materialize earlier operands when later operands can branch: ordering
    // remains left-to-right even for side-effecting function calls.
    fn arguments(&mut self, mut args:Vec<Expr>, prefix:Vec<Expr>, k:ArgContinuation)
        -> Result<Vec<Stmt>,String> {
        if args.is_empty() { return k(self,prefix); }
        let first=args.remove(0);
        let branch_after=args.iter().any(Self::any_marker);
        self.value(first,Rc::new(move |this, value| {
            let mut prefix=prefix.clone();
            if branch_after && !matches!(&value,Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Unit) {
                let tmp=this.name();
                prefix.push(Expr::Var(tmp.clone()));
                let rest=this.arguments(args.clone(),prefix,Rc::clone(&k))?;
                let mut result=vec![Self::bind(tmp,value)];
                result.extend(rest);
                Ok(result)
            } else {
                prefix.push(value);
                this.arguments(args.clone(),prefix,Rc::clone(&k))
            }
        }))
    }

    fn two(&mut self,left:Expr,right:Expr,
        construct:Rc<dyn Fn(Expr,Expr)->Expr>,k:Continuation)
        -> Result<Vec<Stmt>,String> {
        let branch_after=Self::any_marker(&right);
        self.value(left,Rc::new(move |this,first|{
            let make:Continuation={let k=Rc::clone(&k);let construct=Rc::clone(&construct);
                let first=first.clone();
                Rc::new(move |this,second|k(this,construct(first.clone(),second)))};
            if branch_after && !matches!(&first,Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Unit) {
                let temporary=this.name();
                let make:Continuation={let k=Rc::clone(&k);let construct=Rc::clone(&construct);
                    let temporary=temporary.clone();
                    Rc::new(move |this,second|k(this,construct(Expr::Var(temporary.clone()),second)))};
                let mut out=vec![Self::bind(temporary,first)];
                out.extend(this.value(right.clone(),make)?);
                Ok(out)
            } else {this.value(right.clone(),make)}
        }))
    }

    fn value(&mut self, expr:Expr, k:Continuation) -> Result<Vec<Stmt>,String> {
        if let Expr::Lambda{params,result,body}=expr {
            let mut lambda=Self::new(&result);
            return k(self,Expr::Lambda{params,result,body:lambda.sequence(body,None)?});
        }
        if !Self::any_marker(&expr) { return k(self,expr); }
        match expr {
            Expr::Block(mut body) if matches!(body.first(),
                Some(Stmt::Expr(Expr::Var(name))) if name=="__nova_internal_match_value") => {
                body.remove(0);
                let Some(Stmt::Match{value,arms})=body.pop() else {
                    return Err("compiler bug: malformed match-expression marker".into());
                };
                if !body.is_empty() {return Err("compiler bug: malformed match-expression body".into());}
                self.value(value,Rc::new(move |this,value|{
                    let mut expanded=Vec::with_capacity(arms.len());
                    for arm in &arms {
                        let body=this.sequence(arm.body.clone(),Some(Rc::clone(&k)))?;
                        expanded.push(MatchArm{pattern:arm.pattern.clone(),body});
                    }
                    Ok(vec![Stmt::Match{value,arms:expanded}])
                }))
            }
            Expr::Call(name, mut args) if name=="__nova_internal_try" => {
                if args.len()!=1 {return Err("internal try marker expects one operand".into());}
                let inner=args.remove(0);
                let kind=self.result;
                if kind==ReturnKind::Other {
                    return Err("error[NOVA-TRY001]: `?` requires an Option<T> or Result<T,E> return type".into());
                }
                self.value(inner,Rc::new(move |this,subject|{
                    let success=this.name();
                    let good=k(this,Expr::Var(success.clone()))?;
                    let (ok,fail)=match kind {
                        ReturnKind::Optional => (
                            Self::enum_arm("Some",vec![success],good),
                            MatchArm{pattern:Pattern::Wildcard,body:vec![Stmt::Return(Some(Expr::Var("None".into())))]}
                        ),
                        ReturnKind::Outcome => {
                            let error=this.name();
                            (Self::enum_arm("Ok",vec![success],good),Self::enum_arm("Err",vec![error.clone()],vec![
                                Stmt::Return(Some(Expr::Call("Err".into(),vec![Expr::Var(error)])))
                            ]))
                        },
                        ReturnKind::Other => unreachable!(),
                    };
                    Ok(vec![Stmt::Match{value:subject,arms:vec![ok,fail]}])
                }))
            }
            Expr::Call(name, mut args) if name=="__nova_internal_coalesce" => {
                if args.len()!=2 {return Err("internal coalesce marker expects two operands".into());}
                let fallback=args.pop().unwrap();
                let subject=args.pop().unwrap();
                self.value(subject,Rc::new(move |this,value|{
                    let name=this.name();
                    let some=k(this,Expr::Var(name.clone()))?;
                    let none=this.value(fallback.clone(),Rc::clone(&k))?;
                    Ok(vec![Stmt::Match{value,arms:vec![
                        Self::enum_arm("Some",vec![name],some),
                        Self::enum_arm("None",vec![],none),
                    ]}])
                }))
            }
            Expr::Call(name, mut args) if name=="__nova_internal_optional_field" => {
                if args.len()!=2 {return Err("internal optional field requires receiver and name".into());}
                let Expr::Str(field)=args.pop().unwrap() else {return Err("internal optional field name must be literal".into())};
                let receiver=args.remove(0);
                self.value(receiver,Rc::new(move |this,value|{
                    let name=this.name();
                    let success=k(this,Expr::Call("Some".into(),vec![
                        Expr::Field(Box::new(Expr::Var(name.clone())),field.clone())]))?;
                    let empty=k(this,Expr::Var("None".into()))?;
                    Ok(vec![Stmt::Match{value,arms:vec![
                        Self::enum_arm("Some",vec![name],success),
                        Self::enum_arm("None",vec![],empty),
                    ]}])
                }))
            }
            Expr::Call(name, mut args) if name=="__nova_internal_optional_method" => {
                if args.len()<2 {return Err("internal optional method requires receiver and name".into());}
                let receiver=args.remove(0);
                let Expr::Str(method)=args.remove(0) else {return Err("internal optional method name must be literal".into())};
                self.value(receiver,Rc::new(move |this,value|{
                    let name=this.name();
                    let call_k:ArgContinuation={
                        let k=Rc::clone(&k);
                        let name=name.clone();let method=method.clone();
                        Rc::new(move |inner,args| {
                            k(inner,Expr::Call("Some".into(),vec![Expr::MethodCall(
                                Box::new(Expr::Var(name.clone())),method.clone(),args,vec![])]))
                        })
                    };
                    let good=this.arguments(args.clone(),Vec::new(),call_k)?;
                    let empty=k(this,Expr::Var("None".into()))?;
                    Ok(vec![Stmt::Match{value,arms:vec![
                        Self::enum_arm("Some",vec![name],good),
                        Self::enum_arm("None",vec![],empty),
                    ]}])
                }))
            }
            Expr::Binary(a,ast::BinaryOp::And,b) if Self::any_marker(&b) => {
                self.value(*a,Rc::new(move |this,left| {
                    let yes=this.value(*b.clone(),Rc::clone(&k))?;
                    let no=k(this,Expr::Bool(false))?;
                    Ok(vec![Stmt::If{condition:left,then_body:yes,else_body:Some(no)}])
                }))
            }
            Expr::Binary(a,ast::BinaryOp::Or,b) if Self::any_marker(&b) => {
                self.value(*a,Rc::new(move |this,left| {
                    let yes=k(this,Expr::Bool(true))?;
                    let no=this.value(*b.clone(),Rc::clone(&k))?;
                    Ok(vec![Stmt::If{condition:left,then_body:yes,else_body:Some(no)}])
                }))
            }
            Expr::Binary(a,op,b) => self.two(*a,*b,
                Rc::new(move |a,b|Expr::Binary(Box::new(a),op,Box::new(b))),k),
            Expr::Index(a,b) => self.two(*a,*b,
                Rc::new(move |a,b|Expr::Index(Box::new(a),Box::new(b))),k),
            Expr::Call(name,args) => {
                self.arguments(args,Vec::new(),Rc::new(move |this,args|k(this,Expr::Call(name.clone(),args))))
            }
            Expr::GenericCall(name,types,args) => {
                self.arguments(args,Vec::new(),Rc::new(move |this,args|k(this,Expr::GenericCall(name.clone(),types.clone(),args))))
            }
            Expr::MacroCall(name,args) => {
                self.arguments(args,Vec::new(),Rc::new(move |this,args|k(this,Expr::MacroCall(name.clone(),args))))
            }
            Expr::CallIndirect(callee,args) => {
                let args_branch=args.iter().any(Self::any_marker);
                self.value(*callee,Rc::new(move |this,func|{
                    if args_branch && !Self::is_atom(&func) {
                        let tmp=this.name();
                        let out=this.arguments(args.clone(),Vec::new(),Rc::new({let k=Rc::clone(&k);let tmp=tmp.clone();
                            move |this,args|k(this,Expr::CallIndirect(Box::new(Expr::Var(tmp.clone())),args))}))?;
                        let mut combined=vec![Self::bind(tmp,func)];combined.extend(out);Ok(combined)
                    } else {
                        this.arguments(args.clone(),Vec::new(),Rc::new({let k=Rc::clone(&k);
                            move |this,args|k(this,Expr::CallIndirect(Box::new(func.clone()),args))}))
                    }
                }))
            }
            Expr::MethodCall(receiver,method,args,generics) => {
                let pending=args.iter().any(Self::any_marker);
                self.value(*receiver,Rc::new(move |this,recv|{
                    let receiver=recv.clone();
                    let build:ArgContinuation={let k=Rc::clone(&k);let method=method.clone();let generics=generics.clone();
                        Rc::new(move |this,args|k(this,Expr::MethodCall(Box::new(receiver.clone()),method.clone(),args,generics.clone())))};
                    if pending && !Self::is_atom(&recv) {
                        let temp=this.name();
                        let build:ArgContinuation={let k=Rc::clone(&k);let method=method.clone();let generics=generics.clone();let temp=temp.clone();
                            Rc::new(move |this,args|k(this,Expr::MethodCall(Box::new(Expr::Var(temp.clone())),method.clone(),args,generics.clone())))};
                        let mut out=vec![Self::bind(temp,recv)];
                        out.extend(this.arguments(args.clone(),vec![],build)?);
                        Ok(out)
                    } else {this.arguments(args.clone(),vec![],build)}
                }))
            }
            Expr::StructInit(name,fields) => {
                let keys=fields.iter().map(|(n,_)|n.clone()).collect::<Vec<_>>();
                let values=fields.into_iter().map(|(_,v)|v).collect();
                self.arguments(values,vec![],Rc::new(move |this,args|k(this,
                    Expr::StructInit(name.clone(),keys.iter().cloned().zip(args).collect()))))
            }
            Expr::GenericStructInit(name,types,fields) => {
                let keys=fields.iter().map(|(n,_)|n.clone()).collect::<Vec<_>>();
                let values=fields.into_iter().map(|(_,v)|v).collect();
                self.arguments(values,vec![],Rc::new(move |this,args|k(this,
                    Expr::GenericStructInit(name.clone(),types.clone(),keys.iter().cloned().zip(args).collect()))))
            }
            Expr::Array(args) => self.arguments(args,vec![],Rc::new(move |this,args|k(this,Expr::Array(args)))),
            Expr::Unary(op,inner) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Unary(op,Box::new(e))))),
            Expr::Deref(inner) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Deref(Box::new(e))))),
            Expr::Cast(inner,ty) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Cast(Box::new(e),ty.clone())))),
            Expr::Field(inner,field) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Field(Box::new(e),field.clone())))),
            Expr::AddressOf{mutable,value} => self.value(*value,Rc::new(move |this,e|k(this,Expr::AddressOf{mutable,value:Box::new(e)}))),
            Expr::Spread(inner) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Spread(Box::new(e))))),
            Expr::Yield(inner) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Yield(Box::new(e))))),
            Expr::Await(inner) => self.value(*inner,Rc::new(move |this,e|k(this,Expr::Await(Box::new(e))))),
            Expr::If{condition,then_body,else_body} => self.value(*condition,Rc::new(move |this,condition| {
                let then_body=this.sequence(then_body.clone(),Some(Rc::clone(&k)))?;
                let else_body=this.sequence(else_body.clone(),Some(Rc::clone(&k)))?;
                Ok(vec![Stmt::If{condition,then_body,else_body:Some(else_body)}])
            })),
            Expr::Block(body) => {
                let body=self.sequence(body,Some(k))?;
                Ok(vec![Stmt::If{condition:Expr::Bool(true),then_body:body,else_body:None}])
            }
            other => k(self,other),
        }
    }

    fn sequence(&mut self,mut statements:Vec<Stmt>,tail:Option<Continuation>) -> Result<Vec<Stmt>,String>{
        if statements.is_empty(){return Ok(vec![]);}
        let head=statements.remove(0);
        match head {
            Stmt::Let{name,mutable,ty,init,defer} => {
                if defer.as_ref().is_some_and(Self::any_marker) {
                    return Err("error[NOVA-TRY002]: optional operators are not allowed in deferred cleanup expressions".into());
                }
                self.value(init,Rc::new(move |this,init| {
                let mut after=this.sequence(statements.clone(),tail.clone())?;
                after.insert(0,Stmt::Let{name:name.clone(),mutable,ty:ty.clone(),init,defer:defer.clone()});
                Ok(after)
            }))
            },
            Stmt::Return(Some(value)) => self.value(value,Rc::new(|_,value|Ok(vec![Stmt::Return(Some(value))]))),
            Stmt::Expr(value) => self.value(value,Rc::new(move |this,value| {
                let mut out=vec![Stmt::Expr(value)];
                out.extend(this.sequence(statements.clone(),tail.clone())?);
                Ok(out)
            })),
            Stmt::Tail(value) => self.value(value,Rc::new(move |this,value| {
                if let Some(k)=&tail {return k(this,value);}
                let mut out=vec![Stmt::Tail(value)];
                out.extend(this.sequence(statements.clone(),None)?);
                Ok(out)
            })),
            Stmt::Defer(value) => {
                if Self::any_marker(&value){return Err("error[NOVA-TRY002]: use `?` and optional operators before `defer`, not inside its cleanup expression".into());}
                let mut tail_stmt=self.sequence(statements,tail)?;
                tail_stmt.insert(0,Stmt::Defer(value));Ok(tail_stmt)
            }
            Stmt::Set{name,value} => self.value(value,Rc::new(move |this,v| {
                let mut out=vec![Stmt::Set{name:name.clone(),value:v}];out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::DerefSet{name,value} => self.value(value,Rc::new(move |this,v| {
                let mut out=vec![Stmt::DerefSet{name:name.clone(),value:v}];out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::FieldSet{root,field,op,value} => self.value(value,Rc::new(move |this,v| {
                let mut out=vec![Stmt::FieldSet{root:root.clone(),field:field.clone(),op,value:v}];out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::CompoundSet{name,op,value} => self.value(value,Rc::new(move |this,v| {
                let mut out=vec![Stmt::CompoundSet{name:name.clone(),op,value:v}];out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::If{condition,then_body,else_body} => self.value(condition,Rc::new(move |this,cond| {
                let then_body=this.sequence(then_body.clone(),None)?;
                let else_body=else_body.clone().map(|b|this.sequence(b,None)).transpose()?;
                let mut out=vec![Stmt::If{condition:cond,then_body,else_body}];
                out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::While{condition,body} if Self::any_marker(&condition) => {
                let loop_body=self.value(condition,Rc::new(move |this,condition| {
                    let mut lowered_body=vec![Stmt::If{
                        condition:Expr::Unary(ast::UnaryOp::Not,Box::new(condition)),
                        then_body:vec![Stmt::Break],else_body:None
                    }];
                    // Preserve the original while body after the early exit check.
                    lowered_body.extend(this.sequence(body.clone(),None)?);
                    Ok(lowered_body)
                }))?;
                let mut out=vec![Stmt::Loop(loop_body)];out.extend(self.sequence(statements,tail)?);Ok(out)
            }
            Stmt::While{condition,body} => {
                let body=self.sequence(body,None)?;
                let mut out=vec![Stmt::While{condition,body}];out.extend(self.sequence(statements,tail)?);Ok(out)
            }
            Stmt::Loop(body) => {
                let body=self.sequence(body,None)?;
                let mut out=vec![Stmt::Loop(body)];out.extend(self.sequence(statements,tail)?);Ok(out)
            }
            Stmt::For{name,start,end,body} => {
                let end_branches=Self::any_marker(&end);
                self.value(start,Rc::new(move |this,start| {
                    let execute:Continuation={
                        let name=name.clone();let body=body.clone();let statements=statements.clone();let tail=tail.clone();let start=start.clone();
                        Rc::new(move |this,end|{
                            let body=this.sequence(body.clone(),None)?;
                            let mut out=vec![Stmt::For{name:name.clone(),start:start.clone(),end,body}];
                            out.extend(this.sequence(statements.clone(),tail.clone())?);
                            Ok(out)
                        })
                    };
                    if end_branches && !matches!(&start,Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_)) {
                        let variable=this.name();
                        let execute:Continuation={
                            let name=name.clone();let body=body.clone();let statements=statements.clone();let tail=tail.clone();let variable=variable.clone();
                            Rc::new(move |this,end|{
                                let body=this.sequence(body.clone(),None)?;
                                let mut out=vec![Stmt::For{name:name.clone(),start:Expr::Var(variable.clone()),end,body}];
                                out.extend(this.sequence(statements.clone(),tail.clone())?);
                                Ok(out)
                            })
                        };
                        let mut result=vec![Self::bind(variable,start)];
                        result.extend(this.value(end.clone(),execute)?);
                        Ok(result)
                    } else {this.value(end.clone(),execute)}
                }))
            },
            Stmt::ForEach{name,iterable,body} => self.value(iterable,Rc::new(move |this,iterable| {
                let body=this.sequence(body.clone(),None)?;
                let mut out=vec![Stmt::ForEach{name:name.clone(),iterable,body}];
                out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::Match{value,arms} => self.value(value,Rc::new(move |this,value| {
                let arms=arms.iter().map(|arm|Ok(MatchArm{pattern:arm.pattern.clone(),body:this.sequence(arm.body.clone(),None)?}))
                    .collect::<Result<Vec<_>,String>>()?;
                let mut out=vec![Stmt::Match{value,arms}];out.extend(this.sequence(statements.clone(),tail.clone())?);Ok(out)
            })),
            Stmt::Unsafe(body) => {
                let body=self.sequence(body,None)?;
                let mut out=vec![Stmt::Unsafe(body)];out.extend(self.sequence(statements,tail)?);Ok(out)
            }
            other => {
                let mut out=vec![other];out.extend(self.sequence(statements,tail)?);Ok(out)
            }
        }
    }
}

fn items(items:&mut [Item]) -> Result<(),String> {
    for item in items {
        match item {
            Item::Function(f) => {
                let mut lowerer=Lowerer::new(&f.result);
                // The surface pass ordinarily rewrites the final tail into a
                // return. `?` may wrap it in a match first, so perform the
                // equivalent rewrite before continuation lowering.
                if let Some(Stmt::Tail(expression))=f.body.last() {
                    if Lowerer::any_marker(expression) {
                        if let Some(Stmt::Tail(expression))=f.body.pop() {
                            f.body.push(Stmt::Return(Some(expression)));
                        }
                    }
                }
                f.body=lowerer.sequence(std::mem::take(&mut f.body),None)?;
            }
            Item::Impl(block) => for method in &mut block.methods {
                let mut lowerer=Lowerer::new(&method.result);
                if let Some(Stmt::Tail(expression))=method.body.last() {
                    if Lowerer::any_marker(expression) {
                        if let Some(Stmt::Tail(expression))=method.body.pop() {
                            method.body.push(Stmt::Return(Some(expression)));
                        }
                    }
                }
                method.body=lowerer.sequence(std::mem::take(&mut method.body),None)?;
            },
            Item::Namespace{items,..} => self::items(items)?,
            Item::Public(inner) => self::items(std::slice::from_mut(inner.as_mut()))?,
            _=>{},
        }
    }
    Ok(())
}

pub fn lower(program:&ast::Program) -> Result<ast::Program,String> {
    let mut program=program.clone();
    items(&mut program.items)?;
    Ok(program)
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn while_with_try_condition_retains_the_original_body() {
        let result_ty = TypeRef::Applied("Result".into(), vec![
            TypeRef::Named("bool".into()), TypeRef::Named("str".into())
        ]);
        let statements = vec![Stmt::While {
            condition: Expr::Call("__nova_internal_try".into(),
                vec![Expr::Var("next".into())]),
            body: vec![Stmt::Expr(Expr::Call("perform_work".into(), vec![]))],
        }];
        let lowered = Lowerer::new(&result_ty).sequence(statements, None).unwrap();
        let rendered = format!("{lowered:?}");
        assert!(rendered.contains("Loop("), "{rendered}");
        assert_eq!(rendered.matches("perform_work").count(), 1, "{rendered}");
    }
}
