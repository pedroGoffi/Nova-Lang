//! Generic lowering of user-defined field validation predicates to Nova AST.
//!
//! The compiler does not know the semantics of `@Int`, `@Email`, etc. Each
//! #[proc_field_validator(Name)] function is authored in Nova and returns an
//! AST predicate. This module only materializes Result-returning validate().
use crate::ast::{self, Expr, Item, Stmt, TypeRef, UnaryOp};

/// One predicate supplied by a compile-time Nova field validator.
pub struct FieldCheck {
    pub field: String,
    pub annotation: String,
    pub predicate: Expr,
}

pub fn generate(decl: &ast::Struct, checks: Vec<FieldCheck>) -> Option<Item> {
    if checks.is_empty() { return None; }
    let mut body=Vec::new();
    for check in checks {
        let message=format!("{}.{} failed @{} validation",decl.name,check.field,check.annotation);
        body.push(Stmt::If {
            condition: Expr::Unary(UnaryOp::Not, Box::new(check.predicate)),
            then_body: vec![Stmt::Return(Some(Expr::Call("Err".into(),vec![Expr::Str(format!("{message:?}"))])))],
            else_body: None,
        });
    }
    body.push(Stmt::Return(Some(Expr::Call("Ok".into(),vec![Expr::Unit]))));
    let target=if decl.generics.is_empty(){TypeRef::Named(decl.name.clone())}else{
        TypeRef::Applied(decl.name.clone(),decl.generics.iter().map(|x|TypeRef::Named(x.clone())).collect())
    };
    Some(Item::Impl(ast::Impl {
        generics:decl.generics.clone(),trait_name:None,target,associated_types:vec![],
        methods:vec![ast::Function {
            mode:ast::FunctionMode::Normal,name:"validate".into(),generics:vec![],
            concept_bounds:decl.concept_bounds.clone(),
            params:vec![ast::Param{name:"self".into(),ty:TypeRef::Reference{mutable:false,inner:Box::new(TypeRef::Named("Self".into()))},default:None,attributes:vec![]}],
            variadic:false,
            result:TypeRef::Applied("std::old::result::Result".into(),vec![TypeRef::Named("()".into()),TypeRef::Named("str".into())]),
            body,is_virtual:false,is_override:false,attributes:vec![],
        }],
    }))
}
