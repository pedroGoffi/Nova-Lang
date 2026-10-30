//! `for element in collection` lowering for user-defined iterable structs.
//!
//! Protocol: collection.begin() -> Cursor, collection.end() -> Cursor;
//! cursor.equals(end) -> bool; cursor.value() -> T; cursor.next() -> Cursor.
//! This is an AST transform: no iterable-specific runtime, hidden integer IDs,
//! or special-purpose LLVM opcodes. Range iteration continues to use `a..b`.
use crate::ast::{self, Expr, Item, Stmt, UnaryOp};
use std::collections::HashMap;

#[derive(Default)]
struct IteratorLowerer {
    next_id: usize,
    protocol: HashMap<String,String>,
}
impl IteratorLowerer {
    fn method(&self,key:&str,default:&str)->String {
        self.protocol.get(key).cloned().unwrap_or_else(||default.into())
    }
}
impl IteratorLowerer {
    fn statement_list(&mut self, statements: &[Stmt]) -> Vec<Stmt> {
        let mut result = Vec::new();
        for statement in statements {
            result.extend(self.statement(statement));
        }
        result
    }

    fn advance(&self,cursor_name: &str) -> Stmt {
        Stmt::Set {
            name: cursor_name.into(),
            value: Expr::MethodCall(
                Box::new(Expr::Var(cursor_name.into())),
                self.method("foreach_advance","next"),
                vec![],
            ),
        }
    }

    // Rewrite only `continue` referring to THIS foreach, not nested loops.
    fn continue_with_advance(&self,statement: Stmt, cursor_name: &str) -> Vec<Stmt> {
        match statement {
            Stmt::Continue => vec![self.advance(cursor_name), Stmt::Continue],
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => vec![Stmt::If {
                condition,
                then_body: then_body
                    .into_iter()
                    .flat_map(|nested| self.continue_with_advance(nested, cursor_name))
                    .collect(),
                else_body: else_body.map(|body| {
                    body.into_iter()
                        .flat_map(|nested| self.continue_with_advance(nested, cursor_name))
                        .collect()
                }),
            }],
            Stmt::Match { value, arms } => vec![Stmt::Match {
                value,
                arms: arms
                    .into_iter()
                    .map(|mut arm| {
                        arm.body = arm
                            .body
                            .into_iter()
                            .flat_map(|nested| self.continue_with_advance(nested, cursor_name))
                            .collect();
                        arm
                    })
                    .collect(),
            }],
            Stmt::Unsafe(body) => vec![Stmt::Unsafe(
                body.into_iter()
                    .flat_map(|nested| self.continue_with_advance(nested, cursor_name))
                    .collect(),
            )],
            // A continue inside any nested loop targets that nested loop.
            other => vec![other],
        }
    }

    fn statement(&mut self, statement: &Stmt) -> Vec<Stmt> {
        match statement {
            Stmt::ForEach {
                name,
                iterable,
                body,
            } => {
                let unique = self.next_id;
                self.next_id += 1;
                let collection_name = format!("__nova_foreach_collection_{unique}");
                let cursor_name = format!("__nova_foreach_cursor_{unique}");
                let end_name = format!("__nova_foreach_end_{unique}");
                // Borrow a named collection rather than implicitly copying a
                // potentially owning struct (such as std::vec::Int64Vector).
                // A temporary expression is evaluated exactly once.
                let (source_definition, collection_ref) = match iterable {
                    Expr::Var(existing_name) => (None, Expr::Var(existing_name.clone())),
                    _ => (
                        Some(Stmt::Let {
                            name: collection_name.clone(),
                            mutable: false,
                            ty: None,
                            init: iterable.clone(),
                            defer: None,
                        }),
                        Expr::Var(collection_name.clone()),
                    ),
                };
                let cursor_ref = Expr::Var(cursor_name.clone());
                let end_ref = Expr::Var(end_name.clone());
                let make_call = |receiver: Expr, method: &str, args: Vec<Expr>| {
                    Expr::MethodCall(Box::new(receiver), method.into(), args)
                };

                let mut loop_body = vec![Stmt::Let {
                    name: name.clone(),
                    mutable: false,
                    ty: None,
                    init: make_call(cursor_ref.clone(), &self.method("foreach_value","value"), vec![]),
                    defer: None,
                }];
                for nested in self.statement_list(body) {
                    loop_body.extend(self.continue_with_advance(nested, &cursor_name));
                }
                loop_body.push(self.advance(&cursor_name));

                let mut lowering = Vec::new();
                if let Some(source) = source_definition {
                    lowering.push(source);
                }
                lowering.extend(vec![
                    Stmt::Let {
                        name: end_name,
                        mutable: false,
                        ty: None,
                        init: make_call(collection_ref.clone(), &self.method("foreach_end","end"), vec![]),
                        defer: None,
                    },
                    Stmt::Let {
                        name: cursor_name,
                        mutable: true,
                        ty: None,
                        init: make_call(collection_ref, &self.method("foreach_begin","begin"), vec![]),
                        defer: None,
                    },
                    Stmt::While {
                        condition: Expr::Unary(
                            UnaryOp::Not,
                            Box::new(make_call(cursor_ref, &self.method("foreach_equals","equals"), vec![end_ref])),
                        ),
                        body: loop_body,
                    },
                ]);
                // An explicit lexical scope prevents generated locals escaping
                // or colliding with user locals and ensures normal defer unwinding.
                vec![Stmt::If {
                    condition: Expr::Bool(true),
                    then_body: lowering,
                    else_body: None,
                }]
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => vec![Stmt::If {
                condition: condition.clone(),
                then_body: self.statement_list(then_body),
                else_body: else_body.as_ref().map(|body| self.statement_list(body)),
            }],
            Stmt::While { condition, body } => vec![Stmt::While {
                condition: condition.clone(),
                body: self.statement_list(body),
            }],
            Stmt::For {
                name,
                start,
                end,
                body,
            } => vec![Stmt::For {
                name: name.clone(),
                start: start.clone(),
                end: end.clone(),
                body: self.statement_list(body),
            }],
            Stmt::Loop(body) => vec![Stmt::Loop(self.statement_list(body))],
            Stmt::Unsafe(body) => vec![Stmt::Unsafe(self.statement_list(body))],
            Stmt::Match { value, arms } => vec![Stmt::Match {
                value: value.clone(),
                arms: arms
                    .iter()
                    .map(|arm| ast::MatchArm {
                        pattern: arm.pattern.clone(),
                        body: self.statement_list(&arm.body),
                    })
                    .collect(),
            }],
            other => vec![other.clone()],
        }
    }

    fn items(&mut self, items: &[Item]) -> Vec<Item> {
        items
            .iter()
            .map(|item| match item {
                Item::Public(inner) => Item::Public(Box::new(
                    self.items(std::slice::from_ref(inner.as_ref())).remove(0),
                )),
                Item::Namespace { name, items } => Item::Namespace {
                    name: name.clone(),
                    items: self.items(items),
                },
                Item::Function(function) => {
                    let mut function = function.clone();
                    function.body = self.statement_list(&function.body);
                    Item::Function(function)
                }
                Item::Impl(block) => {
                    let mut block = block.clone();
                    for method in &mut block.methods {
                        method.body = self.statement_list(&method.body);
                    }
                    Item::Impl(block)
                }
                other => other.clone(),
            })
            .collect()
    }
}

pub fn desugar(program: &ast::Program) -> ast::Program {
    // Meta is already checked by the surface pipeline. Standalone parser
    // unit tests use the legacy names when there are no language items.
    let mut lowerer=IteratorLowerer::default();
    if let Ok(registry)=crate::meta::Registry::collect(program) {
        for key in ["foreach_begin","foreach_end","foreach_equals","foreach_value","foreach_advance"] {
            if let Some(method)=registry.protocol_method(key) {
                lowerer.protocol.insert(key.into(),method.into());
            }
        }
    }
    ast::Program {items: lowerer.items(&program.items)}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn desugared(source: &str) -> ast::Program {
        desugar(&grammar::ProgramParser::new().parse(source).unwrap())
    }
    #[test]
    fn protocol_method_name_is_defined_in_nova() {
        let program=desugared("#[meta_protocol(foreach_advance, advance)] trait Advance {fn advance(self:Self)->Self;} fn test()->i32 {for x in values {}return 0;}");
        let debug=format!("{program:?}");
        assert!(debug.contains("advance"),"{debug}");
        assert!(!debug.contains("\"next\""),"{debug}");
    }
    #[test]
    fn foreach_uses_cursor_protocol_and_is_scoped() {
        let p = desugared(
            "fn test()->i32 {for element in numbers { if element == 4 { continue; } }return 0;}",
        );
        let Item::Function(function) = &p.items[0] else {
            panic!("no function")
        };
        let Stmt::If { then_body, .. } = &function.body[0] else {
            panic!("iteration must have its own scope")
        };
        let Stmt::While { body, .. } = then_body
            .iter()
            .find(|stmt| matches!(stmt, Stmt::While { .. }))
            .unwrap()
        else {
            panic!("loop expected")
        };
        let Stmt::If { then_body, .. } = &body[1] else {
            panic!("nested if expected")
        };
        assert!(matches!(then_body[0], Stmt::Set { .. }));
        assert!(matches!(then_body[1], Stmt::Continue));
    }
    #[test]
    fn nested_loop_continue_does_not_advance_outer_cursor() {
        let p = desugared(
            "fn test()->i32 {for element in numbers { while false {continue;} }return 0;}",
        );
        let Item::Function(function) = &p.items[0] else {
            panic!("no function")
        };
        let Stmt::If { then_body, .. } = &function.body[0] else {
            panic!("no scope")
        };
        let Stmt::While { body, .. } = then_body
            .iter()
            .find(|stmt| matches!(stmt, Stmt::While { .. }))
            .unwrap()
        else {
            panic!("no loop")
        };
        let Stmt::While { body: inner, .. } = &body[1] else {
            panic!("no inner loop")
        };
        assert!(matches!(inner[0], Stmt::Continue));
    }
}
