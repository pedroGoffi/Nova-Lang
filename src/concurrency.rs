//! Compile-time, deliberately conservative concurrency checking.
//!
//! `sync(mutex) as guard { ... }` is recognized before name/type resolution.
//! The marker never enters HIR: it becomes a lexical owning guard with an
//! automatic drop on all exits, using the existing ownership pipeline.
//!
//! This pass proves local lock-order cycles. It does not claim to decide
//! arbitrary interprocedural or data-dependent deadlocks.
use crate::ast::{self, Expr, Item, Stmt};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MARKER: &str = "__nova_internal_sync_region::";
type Graph = BTreeMap<String, BTreeSet<String>>;

pub fn lower_and_check(program: &ast::Program) -> Result<ast::Program, String> {
    check_thread_callbacks(program)?;
    let mut serial = 0usize;
    Ok(ast::Program {
        items: rewrite_items(&program.items, "", &mut serial)?,
    })
}
// Bootstrap threading only accepts named, non-capturing [[pure]] functions.
// This is intentionally stronger than an unchecked Send/Sync claim: the
// legacy single-threaded resource-handle table cannot be used from a worker.
// Capturing closures, shared ownership, and access to legacy STD allocation
// are deferred until effect- and lifetime-aware native threading exists.
fn check_thread_callbacks(program: &ast::Program) -> Result<(), String> {
    let mut funcs = HashMap::new();
    fn gather(items: &[Item], ns: &str, funcs: &mut HashMap<String, ast::Function>) {
        for item in items {
            match item {
                Item::Public(inner) => gather(std::slice::from_ref(inner.as_ref()), ns, funcs),
                Item::Namespace { name, items } => gather(items, &format!("{ns}{name}::"), funcs),
                Item::Function(function) => {
                    funcs.insert(format!("{ns}{}", function.name), function.clone());
                }
                _ => {}
            }
        }
    }
    gather(&program.items, "", &mut funcs);
    fn verify_expr(
        expr: &Expr,
        ns: &str,
        funcs: &HashMap<String, ast::Function>,
    ) -> Result<(), String> {
        match expr {
            Expr::Call(name, args) | Expr::GenericCall(name, _, args) => {
                if name == "std::thread::spawn" || name.ends_with("::std::thread::spawn") {
                    if args.len() != 1 {
                        return Err("error[NOVA-C010]: std::thread::spawn expects one named [[pure]] callback".into());
                    }
                    let Expr::Var(callback) = &args[0] else {
                        return Err("error[NOVA-C010]: native thread callback must be a named, non-capturing [[pure]] function; closures with captures are not supported".into());
                    };
                    let qualified = if callback.contains("::") {
                        callback.clone()
                    } else {
                        format!("{ns}{callback}")
                    };
                    let function = funcs.get(&qualified).or_else(|| funcs.get(callback));
                    let Some(function) = function else {
                        return Err(format!("error[NOVA-C010]: native thread callback '{callback}' must be declared in the same compilation unit and marked [[pure]]"));
                    };
                    if !function.attributes.iter().any(|attr| attr.name == "pure")
                        || !function.params.is_empty()
                        || !function.generics.is_empty()
                        || !matches!(function.mode, ast::FunctionMode::Normal)
                        || function.result != ast::TypeRef::Named("()".into())
                    {
                        return Err(format!("error[NOVA-C010]: callback '{callback}' cannot run as a native thread\n  = requires [[pure]] fn {callback}() -> () with no captures, generics, or unsafe effects\n  help: restrict the worker to pure local computation until shared-state Send/Sync checking is implemented"));
                    }
                }
                for value in args {
                    verify_expr(value, ns, funcs)?;
                }
            }
            Expr::If { condition, then_body, else_body } => {
                verify_expr(condition,ns,funcs)?;
                verify_stmts(then_body,ns,funcs)?;
                verify_stmts(else_body,ns,funcs)?;
            }
            Expr::Block(body) => verify_stmts(body,ns,funcs)?,
            Expr::MethodCall(receiver, _, args) => {
                verify_expr(receiver, ns, funcs)?;
                for value in args {
                    verify_expr(value, ns, funcs)?;
                }
            }
            Expr::CallIndirect(callee, args) => {
                verify_expr(callee, ns, funcs)?;
                for value in args {
                    verify_expr(value, ns, funcs)?;
                }
            }
            Expr::Unary(_, child)
            | Expr::Field(child, _)
            | Expr::AddressOf { value: child, .. }
            | Expr::Deref(child)
            | Expr::Cast(child, _)
            | Expr::Yield(child)
            | Expr::Await(child)
            | Expr::Spread(child) => verify_expr(child, ns, funcs)?,
            Expr::Binary(left, _, right) | Expr::Index(left, right) => {
                verify_expr(left, ns, funcs)?;
                verify_expr(right, ns, funcs)?;
            }
            Expr::Array(values) | Expr::MacroCall(_, values) => {
                for value in values {
                    verify_expr(value, ns, funcs)?;
                }
            }
            Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
                for (_, value) in fields {
                    verify_expr(value, ns, funcs)?;
                }
            }
            Expr::Lambda { body, .. } => verify_stmts(body, ns, funcs)?,
            _ => {}
        }
        Ok(())
    }
    fn verify_stmts(
        body: &[Stmt],
        ns: &str,
        funcs: &HashMap<String, ast::Function>,
    ) -> Result<(), String> {
        for stmt in body {
            match stmt {
                Stmt::Let { init, defer, .. } => {
                    verify_expr(init, ns, funcs)?;
                    if let Some(expr) = defer {
                        verify_expr(expr, ns, funcs)?;
                    }
                }
                Stmt::Set { value, .. }
                | Stmt::DerefSet { value, .. }
                | Stmt::FieldSet { value, .. }
                | Stmt::CompoundSet { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value) | Stmt::Tail(value)
                | Stmt::Defer(value) => verify_expr(value, ns, funcs)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    verify_expr(condition, ns, funcs)?;
                    verify_stmts(then_body, ns, funcs)?;
                    if let Some(body) = else_body {
                        verify_stmts(body, ns, funcs)?;
                    }
                }
                Stmt::While { condition, body } => {
                    verify_expr(condition, ns, funcs)?;
                    verify_stmts(body, ns, funcs)?;
                }
                Stmt::For {
                    start, end, body, ..
                } => {
                    verify_expr(start, ns, funcs)?;
                    verify_expr(end, ns, funcs)?;
                    verify_stmts(body, ns, funcs)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    verify_expr(iterable, ns, funcs)?;
                    verify_stmts(body, ns, funcs)?;
                }
                Stmt::Loop(body) | Stmt::Unsafe(body) => verify_stmts(body, ns, funcs)?,
                Stmt::Match { value, arms } => {
                    verify_expr(value, ns, funcs)?;
                    for arm in arms {
                        verify_stmts(&arm.body, ns, funcs)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn verify_items(
        items: &[Item],
        ns: &str,
        funcs: &HashMap<String, ast::Function>,
    ) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => {
                    verify_items(std::slice::from_ref(inner.as_ref()), ns, funcs)?
                }
                Item::Namespace { name, items } => {
                    verify_items(items, &format!("{ns}{name}::"), funcs)?
                }
                Item::Function(f) => verify_stmts(&f.body, ns, funcs)?,
                Item::Impl(block) => {
                    for method in &block.methods {
                        verify_stmts(&method.body, ns, funcs)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    verify_items(&program.items, "", &funcs)
}

fn rewrite_items(items: &[Item], namespace: &str, serial: &mut usize) -> Result<Vec<Item>, String> {
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        output.push(match item {
            Item::Public(inner) => Item::Public(Box::new(
                rewrite_items(std::slice::from_ref(inner.as_ref()), namespace, serial)?.remove(0),
            )),
            Item::Namespace { name, items } => Item::Namespace {
                name: name.clone(),
                items: rewrite_items(items, &format!("{namespace}{name}::"), serial)?,
            },
            Item::Function(function) => Item::Function(rewrite_function(
                function,
                &format!("{namespace}{}", function.name),
                serial,
            )?),
            Item::Impl(block) => {
                let mut next = block.clone();
                let owner = format!("{namespace}{:?}", block.target);
                next.methods = block
                    .methods
                    .iter()
                    .map(|function| {
                        rewrite_function(function, &format!("{owner}::{}", function.name), serial)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Item::Impl(next)
            }
            Item::Trait(decl) => {
                let mut next = decl.clone();
                for method in &mut next.methods {
                    if let Some(body) = &method.default_body {
                        let mut graph = Graph::new();
                        method.default_body = Some(rewrite_block(
                            body,
                            &[],
                            &mut graph,
                            &format!("{namespace}{}::{}", decl.name, method.name),
                            serial,
                        )?);
                    }
                }
                Item::Trait(next)
            }
            other => other.clone(),
        });
    }
    Ok(output)
}
fn rewrite_function(
    function: &ast::Function,
    name: &str,
    serial: &mut usize,
) -> Result<ast::Function, String> {
    let mut next = function.clone();
    let mut graph = Graph::new();
    next.body = rewrite_block(&function.body, &[], &mut graph, name, serial)?;
    Ok(next)
}
fn route(
    graph: &Graph,
    start: &str,
    goal: &str,
    seen: &mut BTreeSet<String>,
) -> Option<Vec<String>> {
    if start == goal {
        return Some(vec![goal.to_string()]);
    }
    if !seen.insert(start.to_owned()) {
        return None;
    }
    for next in graph.get(start).into_iter().flat_map(|items| items.iter()) {
        if let Some(mut path) = route(graph, next, goal, seen) {
            path.insert(0, start.to_owned());
            return Some(path);
        }
    }
    None
}
fn add_edge(graph: &mut Graph, from: &str, to: &str, function: &str) -> Result<(), String> {
    if from == to {
        return Err(format!("error[NOVA-C002]: recursive acquisition of mutex '{from}' in '{function}'\n  = a non-recursive mutex cannot be acquired while it is held\n  help: reuse the existing guard instead of acquiring '{from}' again"));
    }
    if let Some(path) = route(graph, to, from, &mut BTreeSet::new()) {
        return Err(format!("error[NOVA-C001]: possible deadlock: conflicting mutex acquisition order in '{function}'\n  = cycle: {from} -> {}\n  help: acquire mutexes in the same order, or use sync({from}, {to}) for ordered joint acquisition",path.join(" -> ")));
    }
    graph.entry(from.into()).or_default().insert(to.into());
    Ok(())
}
fn check_expr(expr: &Expr, held: &[String], function: &str) -> Result<(), String> {
    match expr {
        Expr::Call(name,_) | Expr::GenericCall(name,_,_)
            if name == "std::sync::acquire" || name == "std::sync::acquire_pair" => {
            return Err(format!("error[NOVA-C007]: direct invocation of '{name}' bypasses compiler lock checking in '{function}'\n  help: use sync(mutex) as guard {{ ... }} instead"));
        },
        Expr::Var(name) if held.contains(name) => {
            return Err(format!("error[NOVA-C005]: cannot access or move mutex '{name}' directly while its guard is active in '{function}'\n  help: use the value through the guard (for example guard.get() or guard.set(value))"));
        },
        Expr::Await(_) if !held.is_empty() => return Err(format!("error[NOVA-C003]: await inside sync region in '{function}'\n  = locks held: {}\n  help: release the guard before awaiting; another task may require the same mutex",held.join(", "))),
        Expr::Call(name,_) | Expr::GenericCall(name,_,_)
            if !held.is_empty() && (name.ends_with("::native_block_on") || name.ends_with("::join") || name == "join") => {
            return Err(format!("error[NOVA-C004]: blocking join inside sync region in '{function}'\n  = locks held: {}\n  help: move the join outside the sync block to avoid circular waits",held.join(", ")))
        }
        Expr::MethodCall(_,name,_) if !held.is_empty() && name == "join" => {
            return Err(format!("error[NOVA-C004]: blocking join inside sync region in '{function}'\n  = locks held: {}\n  help: finish the sync block before joining a thread",held.join(", ")))
        }
        Expr::If { condition, then_body, else_body } => {
            check_expr(condition,held,function)?;
            let mut graph=Graph::new(); let mut serial=0;
            rewrite_block(then_body,held,&mut graph,function,&mut serial)?;
            rewrite_block(else_body,held,&mut graph,function,&mut serial)?;
        }
        Expr::Block(body) => {
            let mut graph=Graph::new(); let mut serial=0;
            rewrite_block(body,held,&mut graph,function,&mut serial)?;
        }
        Expr::Call(_,args) | Expr::GenericCall(_,_,args) | Expr::MacroCall(_,args) | Expr::Array(args) => {
            for arg in args { check_expr(arg,held,function)?; }
        },
        Expr::MethodCall(receiver,_,args) => {
            check_expr(receiver,held,function)?;
            for arg in args { check_expr(arg,held,function)?; }
        },
        Expr::CallIndirect(callee,args) => {
            check_expr(callee,held,function)?;
            for arg in args { check_expr(arg,held,function)?; }
        },
        Expr::Binary(left,_,right) | Expr::Index(left,right) => {
            check_expr(left,held,function)?;
            check_expr(right,held,function)?;
        },
        Expr::Unary(_,child) | Expr::Field(child,_) | Expr::AddressOf{value:child,..} |
        Expr::Deref(child) | Expr::Cast(child,_) | Expr::Yield(child) | Expr::Await(child) | Expr::Spread(child) => check_expr(child,held,function)?,
        Expr::StructInit(_,fields) | Expr::GenericStructInit(_,_,fields) => {
            for (_,value) in fields { check_expr(value,held,function)?; }
        },
        Expr::Lambda{body,..} => {
            // A lambda is lifted later; conservatively reject sync within its
            // body until capture- and lifetime-aware thread closures exist.
            let mut graph=Graph::new();
            let mut serial=0;
            let lowered=rewrite_block(body,held,&mut graph,function,&mut serial)?;
            if lowered.iter().zip(body).any(|(new,old)| format!("{new:?}")!=format!("{old:?}")) {
                return Err(format!("error[NOVA-C008]: sync inside a lambda is not supported in '{function}'\n  help: move the critical section into a named function"));
            }
        },
        _ => {},
    }
    Ok(())
}
fn forbid_move(expr: &Expr, held: &[String], function: &str) -> Result<(), String> {
    if let Expr::Var(name) = expr {
        if held.iter().any(|lock| lock == name) {
            return Err(format!("error[NOVA-C005]: cannot move mutex '{name}' while sync guard is active in '{function}'\n  help: operate through the guard, then move the mutex after the sync block"));
        }
    }
    Ok(())
}
/// The guard must remain a lexical RAII owner. Its name is only accepted
/// as the receiver of the safe accessor methods. In particular it cannot be
/// returned, stored in a container, copied, passed to a callback, or dropped
/// explicitly while a sync region is active.
fn guard_expr(expr: &Expr, guard: &str, function: &str) -> Result<(), String> {
    match expr {
        Expr::If { condition, then_body, else_body } => {
            guard_expr(condition,guard,function)?;
            for stmt in then_body.iter().chain(else_body) { guard_statement(stmt,guard,function)?; }
        }
        Expr::Block(body) => {
            for stmt in body { guard_statement(stmt,guard,function)?; }
        }
        Expr::MethodCall(receiver, method, args)
            if matches!(receiver.as_ref(),Expr::Var(name) if name==guard)
                && matches!(
                    method.as_str(),
                    "get" | "set" | "get_first" | "get_second" | "set_first" | "set_second"
                ) =>
        {
            for arg in args {
                guard_expr(arg, guard, function)?;
            }
        }
        Expr::Var(name) if name == guard => {
            return Err(format!("error[NOVA-C009]: guard '{guard}' cannot escape or be used as an untracked value in '{function}'\n  help: only call get/set on the guard inside its sync region; it is released automatically"));
        }
        Expr::Unary(_, child)
        | Expr::Field(child, _)
        | Expr::AddressOf { value: child, .. }
        | Expr::Deref(child)
        | Expr::Yield(child)
        | Expr::Await(child)
        | Expr::Spread(child)
        | Expr::Cast(child, _) => guard_expr(child, guard, function)?,
        Expr::Binary(left, _, right) | Expr::Index(left, right) => {
            guard_expr(left, guard, function)?;
            guard_expr(right, guard, function)?;
        }
        Expr::Call(_, args)
        | Expr::GenericCall(_, _, args)
        | Expr::MacroCall(_, args)
        | Expr::Array(args) => {
            for item in args {
                guard_expr(item, guard, function)?;
            }
        }
        Expr::MethodCall(receiver, _, args) => {
            guard_expr(receiver, guard, function)?;
            for item in args {
                guard_expr(item, guard, function)?;
            }
        }
        Expr::CallIndirect(callee, args) => {
            guard_expr(callee, guard, function)?;
            for item in args {
                guard_expr(item, guard, function)?;
            }
        }
        Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
            for (_, value) in fields {
                guard_expr(value, guard, function)?;
            }
        }
        Expr::Lambda { body, .. } => {
            for statement in body {
                guard_statement(statement, guard, function)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn guard_statement(statement: &Stmt, guard: &str, function: &str) -> Result<(), String> {
    match statement {
        Stmt::Let {
            name, init, defer, ..
        } => {
            if name == guard {
                return Err(format!(
                    "error[NOVA-C009]: guard '{guard}' cannot be shadowed in '{function}'"
                ));
            }
            guard_expr(init, guard, function)?;
            if let Some(value) = defer {
                guard_expr(value, guard, function)?;
            }
        }
        Stmt::Set { name, value } | Stmt::CompoundSet { name, value, .. } => {
            if name == guard {
                return Err(format!(
                    "error[NOVA-C009]: guard '{guard}' cannot be reassigned in '{function}'"
                ));
            }
            guard_expr(value, guard, function)?;
        }
        Stmt::DerefSet { name, value } => {
            if name == guard {
                return Err(format!("error[NOVA-C009]: guard '{guard}' cannot be mutated through dereference in '{function}'"));
            }
            guard_expr(value, guard, function)?;
        }
        Stmt::FieldSet { root, value, .. } => {
            if root == guard {
                return Err(format!("error[NOVA-C009]: guard '{guard}' cannot expose internal fields in '{function}'"));
            }
            guard_expr(value, guard, function)?;
        }
        Stmt::If {
            condition,
            then_body,
            else_body,
        } => {
            guard_expr(condition, guard, function)?;
            for statement in then_body {
                guard_statement(statement, guard, function)?;
            }
            if let Some(body) = else_body {
                for statement in body {
                    guard_statement(statement, guard, function)?;
                }
            }
        }
        Stmt::While { condition, body } => {
            guard_expr(condition, guard, function)?;
            for statement in body {
                guard_statement(statement, guard, function)?;
            }
        }
        Stmt::For {
            name,
            start,
            end,
            body,
        } => {
            if name == guard {
                return Err(format!(
                    "error[NOVA-C009]: loop variable cannot shadow guard '{guard}'"
                ));
            }
            guard_expr(start, guard, function)?;
            guard_expr(end, guard, function)?;
            for statement in body {
                guard_statement(statement, guard, function)?;
            }
        }
        Stmt::ForEach {
            name,
            iterable,
            body,
        } => {
            if name == guard {
                return Err(format!(
                    "error[NOVA-C009]: iterator cannot shadow guard '{guard}'"
                ));
            }
            guard_expr(iterable, guard, function)?;
            for statement in body {
                guard_statement(statement, guard, function)?;
            }
        }
        Stmt::Loop(body) | Stmt::Unsafe(body) => {
            for statement in body {
                guard_statement(statement, guard, function)?;
            }
        }
        Stmt::Match { value, arms } => {
            guard_expr(value, guard, function)?;
            for arm in arms {
                for statement in &arm.body {
                    guard_statement(statement, guard, function)?;
                }
            }
        }
        Stmt::Expr(expr) | Stmt::Tail(expr) | Stmt::Defer(expr) => guard_expr(expr, guard, function)?,
        Stmt::Return(Some(expr)) => guard_expr(expr, guard, function)?,
        _ => {}
    }
    Ok(())
}
fn rewrite_block(
    stmts: &[Stmt],
    held: &[String],
    graph: &mut Graph,
    function: &str,
    serial: &mut usize,
) -> Result<Vec<Stmt>, String> {
    let mut result = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        result.push(rewrite_statement(stmt, held, graph, function, serial)?);
    }
    Ok(result)
}
fn rewrite_statement(
    stmt: &Stmt,
    held: &[String],
    graph: &mut Graph,
    function: &str,
    serial: &mut usize,
) -> Result<Stmt, String> {
    match stmt {
        Stmt::If {
            condition: Expr::Call(marker, arguments),
            then_body,
            else_body: None,
        } if marker.starts_with(MARKER) => {
            if arguments.is_empty() || arguments.len() > 2 {
                return Err(format!("error[NOVA-C006]: sync in '{function}' requires one or two named mutexes\n  help: use sync(balance) as guard {{ ... }} or sync(a, b) as pair {{ ... }}"));
            }
            let mut names = Vec::new();
            for argument in arguments {
                let Expr::Var(name) = argument else {
                    return Err(format!(
                        "error[NOVA-C006]: sync in '{function}' requires named local mutexes"
                    ));
                };
                if names.contains(name) || held.contains(name) {
                    return Err(format!("error[NOVA-C002]: mutex '{name}' acquired again while already held in '{function}'\n  help: avoid recursive locking; reuse the active guard"));
                }
                names.push(name.clone());
            }
            for previous in held {
                for name in &names {
                    add_edge(graph, previous, name, function)?;
                }
            }
            let mut held_next = held.to_vec();
            held_next.extend(names.iter().cloned());
            let body = rewrite_block(then_body, &held_next, graph, function, serial)?;
            let alias = marker.strip_prefix(MARKER).unwrap_or("");
            let guard = if alias.is_empty() {
                let name = format!("__nova_sync_guard_{}", *serial);
                *serial += 1;
                name
            } else {
                if names.iter().any(|name| name == alias) {
                    return Err(format!("error[NOVA-C006]: guard name '{alias}' shadows its mutex in '{function}'\n  help: select a distinct name after 'as'"));
                }
                alias.to_string()
            };
            let target = if names.len() == 1 {
                "std::sync::acquire"
            } else {
                "std::sync::acquire_pair"
            };
            let args = names
                .iter()
                .map(|name| Expr::AddressOf {
                    mutable: false,
                    value: Box::new(Expr::Var(name.clone())),
                })
                .collect();
            for statement in &body {
                guard_statement(statement, &guard, function)?;
            }
            let init = Stmt::Let {
                name: guard,
                mutable: false,
                ty: None,
                init: Expr::Call(target.into(), args),
                defer: None,
            };
            let mut guarded = Vec::with_capacity(body.len() + 1);
            guarded.push(init);
            guarded.extend(body);
            Ok(Stmt::If {
                condition: Expr::Bool(true),
                then_body: guarded,
                else_body: None,
            })
        }
        Stmt::If {
            condition,
            then_body,
            else_body,
        } => {
            check_expr(condition, held, function)?;
            Ok(Stmt::If {
                condition: condition.clone(),
                then_body: rewrite_block(then_body, held, graph, function, serial)?,
                else_body: else_body
                    .as_ref()
                    .map(|branch| rewrite_block(branch, held, graph, function, serial))
                    .transpose()?,
            })
        }
        Stmt::While { condition, body } => {
            check_expr(condition, held, function)?;
            Ok(Stmt::While {
                condition: condition.clone(),
                body: rewrite_block(body, held, graph, function, serial)?,
            })
        }
        Stmt::Loop(body) => Ok(Stmt::Loop(rewrite_block(
            body, held, graph, function, serial,
        )?)),
        Stmt::For {
            name,
            start,
            end,
            body,
        } => {
            check_expr(start, held, function)?;
            check_expr(end, held, function)?;
            Ok(Stmt::For {
                name: name.clone(),
                start: start.clone(),
                end: end.clone(),
                body: rewrite_block(body, held, graph, function, serial)?,
            })
        }
        Stmt::ForEach {
            name,
            iterable,
            body,
        } => {
            check_expr(iterable, held, function)?;
            Ok(Stmt::ForEach {
                name: name.clone(),
                iterable: iterable.clone(),
                body: rewrite_block(body, held, graph, function, serial)?,
            })
        }
        Stmt::Unsafe(body) => Ok(Stmt::Unsafe(rewrite_block(
            body, held, graph, function, serial,
        )?)),
        Stmt::Match { value, arms } => {
            check_expr(value, held, function)?;
            let mut next = Vec::new();
            for arm in arms {
                let mut item = arm.clone();
                item.body = rewrite_block(&arm.body, held, graph, function, serial)?;
                next.push(item);
            }
            Ok(Stmt::Match {
                value: value.clone(),
                arms: next,
            })
        }
        Stmt::Let {
            name, init, defer, ..
        } => {
            check_expr(init, held, function)?;
            forbid_move(init, held, function)?;
            if let Some(deferred) = defer {
                check_expr(deferred, held, function)?;
            }
            if held.contains(name) {
                return Err(format!(
                    "error[NOVA-C005]: cannot shadow locked mutex '{name}' in '{function}'"
                ));
            }
            Ok(stmt.clone())
        }
        Stmt::Set { name, value } | Stmt::CompoundSet { name, value, .. } => {
            check_expr(value, held, function)?;
            if held.contains(name) {
                return Err(format!("error[NOVA-C005]: cannot reassign locked mutex '{name}' in '{function}'\n  help: change its value through the guard"));
            }
            Ok(stmt.clone())
        }
        Stmt::Return(Some(value)) => {
            check_expr(value, held, function)?;
            forbid_move(value, held, function)?;
            Ok(stmt.clone())
        }
        Stmt::Expr(value) | Stmt::Tail(value) | Stmt::Defer(value) => {
            check_expr(value, held, function)?;
            Ok(stmt.clone())
        }
        Stmt::FieldSet { root, value, .. } | Stmt::DerefSet { name: root, value } => {
            check_expr(value, held, function)?;
            if held.contains(root) {
                return Err(format!("error[NOVA-C005]: cannot mutate locked mutex '{root}' while its guard is active in '{function}'"));
            }
            Ok(stmt.clone())
        }
        _ => Ok(stmt.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(code: &str) -> ast::Program {
        crate::grammar::ProgramParser::new().parse(code).unwrap()
    }
    #[test]
    fn sync_lowers_to_raii_guard_and_lexical_scope() {
        let parsed=parse("fn main() -> i32 { let a = std::sync::new_i64(3); sync(a) as guard { let value = guard.get(); } return 0; }");
        let result = lower_and_check(&parsed).unwrap();
        let Item::Function(function) = &result.items[0] else {
            panic!("expected function")
        };
        let Stmt::If {
            condition: Expr::Bool(true),
            then_body,
            ..
        } = &function.body[1]
        else {
            panic!("expected lexical guard scope")
        };
        assert!(
            matches!(&then_body[0], Stmt::Let{ name, init:Expr::Call(call,_),.. } if name=="guard" && call=="std::sync::acquire")
        );
    }
    #[test]
    fn detects_inverse_lock_order() {
        let parsed = parse(
            "fn transfer() -> i32 { sync(a) { sync(b) { } } sync(b) { sync(a) { } } return 0; }",
        );
        let error = lower_and_check(&parsed).unwrap_err();
        assert!(error.contains("NOVA-C001"), "{error}");
        assert!(
            error.contains("a -> b -> a") || error.contains("b -> a -> b"),
            "{error}"
        );
    }
    #[test]
    fn detects_recursive_acquisition_and_await() {
        let recursive = parse("fn f() -> i32 { sync(a) { sync(a) { } } return 0; }");
        assert!(lower_and_check(&recursive)
            .unwrap_err()
            .contains("NOVA-C002"));
        let waiting = parse("fn f() -> i32 { sync(a) { let x = await work; } return 0; }");
        assert!(lower_and_check(&waiting).unwrap_err().contains("NOVA-C003"));
        let joining = parse("fn f() -> i32 { sync(a) { worker.join(); } return 0; }");
        assert!(lower_and_check(&joining).unwrap_err().contains("NOVA-C004"));
    }
    #[test]
    fn blocking_checks_only_apply_while_a_mutex_is_held() {
        // Await, direct runtime block_on, and thread.join are not inherently
        // illegal. Only waiting while a sync guard is held risks a deadlock.
        let outside=parse("fn f() -> i32 { let awaited = await work; let joined = std::task::native_block_on(handle); worker.join(); return 0; }");
        lower_and_check(&outside)
            .expect("normal async/blocking operations outside sync must be valid");

        let nested_outside =
            parse("fn f() -> i32 { sync(a) { } if true { worker.join(); } return 0; }");
        lower_and_check(&nested_outside).expect("a completed sync does not retain the lock");

        for snippet in [
            "sync(a) { worker.join(); }",
            "sync(a) { std::task::native_block_on(handle); }",
            "sync(a) { let result = await work; }",
        ] {
            let source = format!("fn f() -> i32 {{ {snippet} return 0; }}");
            let error = lower_and_check(&parse(&source)).unwrap_err();
            assert!(
                error.contains("NOVA-C003") || error.contains("NOVA-C004"),
                "{error}"
            );
            assert!(error.contains("locks held: a"), "{error}");
        }
    }
    #[test]
    fn joint_acquisition_is_ordered_without_a_cycle() {
        let parsed =
            parse("fn f() -> i32 { sync(a,b) as locked { } sync(b,a) as reversed { } return 0; }");
        let result = lower_and_check(&parsed).unwrap();
        let Item::Function(function) = &result.items[0] else {
            panic!("expected function")
        };
        assert!(
            matches!(&function.body[0], Stmt::If{then_body,..} if matches!(&then_body[0],Stmt::Let{init:Expr::Call(name,..),..} if name=="std::sync::acquire_pair"))
        );
    }
    #[test]
    fn nova_028_std_sync_and_thread_reach_typed_llvm() {
        let root = std::path::Path::new("examples/packages/std_concurrency_028");
        let (input, _) = crate::package::load_detailed(root).unwrap();
        let lowered =
            crate::surface::lower(&input).expect("concurrency syntax and std module resolve");
        let typed =
            crate::hir::lower(&lowered).expect("typed HIR accepts lexical RAII mutex guards");
        let mir = crate::mir::lower(&typed).expect("MIR accepts concurrency and thread callback");
        let llvm = crate::codegen::generate(&mir);
        assert!(llvm.contains("@__nova_std__sync____mutex_lock("), "{llvm}");
        assert!(
            llvm.contains("@__nova_std__thread____thread_start("),
            "{llvm}"
        );
    }
    #[test]
    fn native_thread_requires_pure_noncapturing_callback() {
        let bad=parse("fn worker() -> () { return; } fn main() -> i32 { let thread = std::thread::spawn(worker); return 0; }");
        assert!(lower_and_check(&bad).unwrap_err().contains("NOVA-C010"));
        let good=parse("[[pure]] fn worker() -> () { return; } fn main() -> i32 { let thread = std::thread::spawn(worker); return 0; }");
        lower_and_check(&good).unwrap();
    }
    #[test]
    fn guard_cannot_escape_scope() {
        let tree = parse("fn work() -> i32 { sync(a) as guard { let leaked = guard; } return 0; }");
        assert!(lower_and_check(&tree).unwrap_err().contains("NOVA-C009"));
    }
    #[test]
    fn direct_mutex_acquire_is_rejected() {
        let tree = parse("fn work() -> i32 { let guard = std::sync::acquire(&a); return 0; }");
        assert!(lower_and_check(&tree).unwrap_err().contains("NOVA-C007"));
    }
    #[test]
    fn scope_syntax_creates_a_lexical_block() {
        let parsed = parse("fn f() -> i32 { scope { let a = 1; } return 0; }");
        let result = lower_and_check(&parsed).unwrap();
        assert_eq!(result.items.len(), 1);
    }
}
