//! Flow-aware ownership / borrowing pass over typed HIR.
//!
//! Ownership and borrowing are compiler semantics, not runtime calls:
//! * `Copy` values may be read repeatedly, owning locals are consumed at by-value sites.
//! * `&T` is shared and `&mut T` is exclusive; outstanding lexical loans prevent moves.
//! * join points retain a value only when it exists on every predecessor.
//! * moved owners have their destruction suppressed through MIR drop flags.
//!
//! Borrowing currently has lexical lifetime inference. Precise NLL and partial
//! moves are intentionally rejected rather than incorrectly accepted.
use crate::{
    hir::{self, Expr, ExprKind, LocalId, Stmt},
    types::{TypeId, TypeKind, Types},
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Availability {
    Available,
    Moved,
    MaybeMoved,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Loan {
    source: LocalId,
    exclusive: bool,
}
#[derive(Clone)]
struct State {
    available: Vec<Availability>,
    loans: HashMap<LocalId, Loan>, // borrower local -> source local
    protected: HashSet<LocalId>,   // named locals read by deferred cleanups
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum UseMode {
    Read,
    Consume,
}

fn owns_value(types: &Types, ty: TypeId, explicit_drop: &HashSet<String>) -> bool {
    fn visit(
        types: &Types,
        ty: TypeId,
        explicit_drop: &HashSet<String>,
        seen: &mut HashSet<usize>,
    ) -> bool {
        match types.kind(ty) {
            TypeKind::Struct(struct_id) => {
                if !seen.insert(struct_id) {
                    return true;
                }
                let info = &types.structs[struct_id];
                let owning = explicit_drop.contains(&info.name)
                    || info
                        .fields
                        .iter()
                        .any(|field| visit(types, field.ty, explicit_drop, seen));
                seen.remove(&struct_id); // recursion stack, not a global visited set
                owning
            }
            TypeKind::Array { element, .. } => visit(types, element, explicit_drop, seen),
            _ => false,
        }
    }
    visit(types, ty, explicit_drop, &mut HashSet::new())
}
fn is_copy(types: &Types, ty: TypeId, explicit_drop: &HashSet<String>) -> bool {
    if let TypeKind::Reference { mutable: true, .. } = types.kind(ty) {
        return false;
    }
    !owns_value(types, ty, explicit_drop)
}
struct Checker<'a> {
    types: &'a Types,
    function: &'a hir::Function,
    signatures: &'a [hir::Function],
    explicit_drop: &'a HashSet<String>,
}
impl Checker<'_> {
    fn name(&self, local: LocalId) -> &str {
        &self.function.locals[local].name
    }
    fn alive(&self, state: &State, local: LocalId) -> Result<(), String> {
        match state.available[local] {
            Availability::Available => Ok(()),
            Availability::Moved => Err(format!("use of moved value '{}'", self.name(local))),
            Availability::MaybeMoved => Err(format!(
                "value '{}' may have been moved on another control-flow path",
                self.name(local)
            )),
        }
    }
    fn ensure_not_borrowed(
        &self,
        state: &State,
        source: LocalId,
        action: &str,
    ) -> Result<(), String> {
        if state.loans.values().any(|loan| loan.source == source) {
            return Err(format!(
                "cannot {action} '{}' while it is borrowed",
                self.name(source)
            ));
        }
        if state.protected.contains(&source) {
            return Err(format!(
                "cannot {action} '{}' while a defer cleanup uses it",
                self.name(source)
            ));
        }
        Ok(())
    }
    fn inspect(
        &self,
        expression: &mut Expr,
        state: &mut State,
        mode: UseMode,
    ) -> Result<Option<Loan>, String> {
        let owned = !is_copy(self.types, expression.ty, self.explicit_drop);
        match &mut expression.kind {
            ExprKind::Local(local) => {
                let local = *local;
                self.alive(state, local)?;
                if mode == UseMode::Read
                    && state
                        .loans
                        .values()
                        .any(|loan| loan.source == local && loan.exclusive)
                {
                    return Err(format!(
                        "cannot read '{}' while it has an active mutable borrow",
                        self.name(local)
                    ));
                }
                let existing_loan = state.loans.get(&local).copied();
                if mode == UseMode::Consume && owned {
                    self.ensure_not_borrowed(state, local, "move")?;
                    state.available[local] = Availability::Moved;
                    if matches!(
                        self.types.kind(expression.ty),
                        TypeKind::Reference { mutable: true, .. }
                    ) {
                        // Moving an exclusive reference transfers its loan.
                        state.loans.remove(&local);
                    }
                    expression.kind = ExprKind::MoveLocal(local);
                }
                Ok(existing_loan)
            }
            ExprKind::MoveLocal(..) => Err("compiler bug: ownership pass was run twice".into()),
            ExprKind::AddressOf { local, mutable } => {
                self.alive(state, *local)?;
                if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. }) {
                    if state
                        .loans
                        .values()
                        .any(|loan| loan.source == *local && (*mutable || loan.exclusive))
                    {
                        return Err(format!(
                            "conflicting {}borrow of '{}'",
                            if *mutable { "mutable " } else { "shared " },
                            self.name(*local)
                        ));
                    }
                    Ok(Some(Loan {
                        source: *local,
                        exclusive: *mutable,
                    }))
                } else {
                    Ok(None)
                } // Raw pointers are only permitted in `unsafe`.
            }
            ExprKind::Field(parent, _) => {
                // Reading a field does not consume its whole parent; moving
                // individual fields requires per-field drop flags (not yet legal).
                let _ = self.inspect(parent, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned && !self.function.name.ends_with("__drop") {
                    return Err("moving an owning field out of a struct is not supported; move the whole struct".into());
                }
                Ok(None)
            }
            ExprKind::Index(parent, index) => {
                self.inspect(parent, state, UseMode::Read)?;
                self.inspect(index, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned && !self.function.name.ends_with("__drop") {
                    return Err(
                        "moving an owning indexed element requires an owned iterator".into(),
                    );
                }
                Ok(None)
            }
            ExprKind::Call(id, arguments) => {
                let signature = self
                    .signatures
                    .get(*id)
                    .ok_or("compiler bug: missing callee signature")?;
                let mut temporary_loans: Vec<Loan> = Vec::new();
                if arguments.len() < signature.params.len()
                    || (!signature.c_variadic && arguments.len() != signature.params.len())
                {
                    return Err("compiler bug: typed HIR call arity mismatch".into());
                }
                for (index, argument) in arguments.iter_mut().enumerate() {
                    // C varargs are already ABI-checked as scalars/pointers in
                    // typed HIR. Extra arguments have no declared parameter.
                    let argument_mode = if let Some(parameter) = signature.params.get(index) {
                        let parameter_ty = signature.locals[*parameter].ty;
                        if is_copy(self.types, parameter_ty, self.explicit_drop) {
                            UseMode::Read
                        } else {
                            UseMode::Consume
                        }
                    } else {
                        UseMode::Read
                    };
                    let loan = self.inspect(argument, state, argument_mode)?;
                    if let Some(loan) = loan {
                        if temporary_loans.iter().any(|existing| {
                            existing.source == loan.source && (existing.exclusive || loan.exclusive)
                        }) {
                            return Err(format!(
                                "conflicting borrows of '{}' in the same function call",
                                self.name(loan.source)
                            ));
                        }
                        temporary_loans.push(loan);
                    }
                }
                if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. }) {
                    return Err("borrowed return from a function needs explicit lifetime relationships (not supported in this release)".into());
                }
                Ok(None)
            }
            ExprKind::CallIndirect(callee, arguments) => {
                let callee_type = callee.ty;
                self.inspect(callee, state, UseMode::Read)?;
                let TypeKind::Function { params, .. } = self.types.kind(callee_type) else {
                    return Err("compiler bug: indirect call requires function".into());
                };
                let mut temporary_loans: Vec<Loan> = Vec::new();
                for (argument, parameter_ty) in arguments.iter_mut().zip(params) {
                    if let Some(loan) = self.inspect(
                        argument,
                        state,
                        if is_copy(self.types, parameter_ty, self.explicit_drop) {
                            UseMode::Read
                        } else {
                            UseMode::Consume
                        },
                    )? {
                        if temporary_loans.iter().any(|previous| {
                            previous.source == loan.source && (previous.exclusive || loan.exclusive)
                        }) {
                            return Err(format!(
                                "conflicting borrows of '{}' in indirect call",
                                self.name(loan.source)
                            ));
                        }
                        temporary_loans.push(loan);
                    }
                }
                if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. }) {
                    return Err(
                        "borrowed result from indirect call requires lifetime bounds".into(),
                    );
                }
                Ok(None)
            }
            ExprKind::StructInit(_, members) | ExprKind::Array(members) => {
                for member in members {
                    let loan = self.inspect(member, state, UseMode::Consume)?;
                    if let Some(loan) = loan {
                        return Err(format!("storing borrowed '{}' in an aggregate requires explicit lifetime parameters",self.name(loan.source)));
                    }
                }
                Ok(None)
            }
            ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::Cast(inner) => {
                let borrow = self.inspect(inner, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned {
                    return Err("cannot move a non-Copy value through a dereference or cast; move the owner".into());
                }
                Ok(borrow)
            }
            ExprKind::Binary(left, _, right) => {
                self.inspect(left, state, UseMode::Read)?;
                self.inspect(right, state, UseMode::Read)?;
                Ok(None)
            }
            ExprKind::FunctionRef(_)
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Unit
            | ExprKind::Zero => Ok(None),
        }
    }
    fn record_loan(
        &self,
        state: &mut State,
        borrower: LocalId,
        loan: Option<Loan>,
    ) -> Result<(), String> {
        if let Some(new_loan) = loan {
            if state
                .loans
                .values()
                .any(|old| old.source == new_loan.source && (old.exclusive || new_loan.exclusive))
            {
                return Err(format!(
                    "conflicting borrow of '{}'",
                    self.name(new_loan.source)
                ));
            }
            state.loans.insert(borrower, new_loan);
        }
        Ok(())
    }
    fn visit_block(&self, body: &mut [Stmt], state: &mut State) -> Result<bool, String> {
        let mut declared = Vec::new();
        let mut terminates = false;
        for statement in body.iter_mut() {
            if terminates {
                break;
            }
            match statement {
                Stmt::Let { local, init } => {
                    let loan = self.inspect(init, state, UseMode::Consume)?;
                    state.available[*local] = Availability::Available;
                    self.record_loan(state, *local, loan)?;
                    declared.push(*local);
                }
                Stmt::Set { local, value } => {
                    if state.available[*local] == Availability::Available
                        && !is_copy(
                            self.types,
                            self.function.locals[*local].ty,
                            self.explicit_drop,
                        )
                    {
                        return Err(format!(
                            "assigning '{}' would overwrite a live owned value; move it first",
                            self.name(*local)
                        ));
                    }
                    self.ensure_not_borrowed(state, *local, "overwrite")?;
                    state.loans.remove(local);
                    let loan = self.inspect(value, state, UseMode::Consume)?;
                    state.available[*local] = Availability::Available;
                    self.record_loan(state, *local, loan)?;
                }
                Stmt::DerefSet { local, value } => {
                    self.alive(state, *local)?;
                    // A mut borrow is the unique capability to write through
                    // this reference. No second alias may coexist.
                    self.inspect(value, state, UseMode::Consume)?;
                }
                Stmt::FieldSet { root, value, .. } => {
                    self.alive(state, *root)?;
                    self.ensure_not_borrowed(state, *root, "mutate")?;
                    self.inspect(value, state, UseMode::Consume)?;
                }
                Stmt::Defer { owner: Some(_), .. } => {} // Compiler-generated RAII cleanup.
                Stmt::Defer { expr, owner: None } => {
                    // Check cleanup accesses without moving anything now.
                    let mut accessed = HashSet::new();
                    collect_locals(expr, &mut accessed);
                    for &local in &accessed {
                        self.alive(state, local)?;
                        if !is_copy(
                            self.types,
                            self.function.locals[local].ty,
                            self.explicit_drop,
                        ) {
                            state.protected.insert(local);
                        }
                    }
                }
                Stmt::Return(value) => {
                    if let Some(value) = value {
                        let loan = self.inspect(value, state, UseMode::Consume)?;
                        if matches!(self.types.kind(value.ty), TypeKind::Reference { .. }) {
                            let safe_parameter_return = matches!(&value.kind,ExprKind::Local(local)|ExprKind::MoveLocal(local) if self.function.params.contains(local));
                            if !safe_parameter_return {
                                return Err("cannot return borrowed reference without lifetime proof; only a reference parameter may currently be returned".into());
                            }
                        }
                        if let Some(loan) = loan {
                            if !self.function.params.contains(&loan.source) {
                                return Err(format!("cannot return reference to local '{}' (borrow would outlive owner)",self.name(loan.source)));
                            }
                        }
                    }
                    terminates = true;
                }
                Stmt::Expr(expression) => {
                    self.inspect(expression, state, UseMode::Consume)?;
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.inspect(condition, state, UseMode::Read)?;
                    let mut on_true = state.clone();
                    let true_exits = self.visit_block(then_body, &mut on_true)?;
                    let mut on_false = state.clone();
                    let false_exits = if let Some(body) = else_body {
                        self.visit_block(body, &mut on_false)?
                    } else {
                        false
                    };
                    match (true_exits, false_exits) {
                        (true, true) => terminates = true,
                        (true, false) => *state = on_false,
                        (false, true) => *state = on_true,
                        (false, false) => {
                            for (position, (left, right)) in on_true
                                .available
                                .iter()
                                .zip(&on_false.available)
                                .enumerate()
                            {
                                state.available[position] = if left == right {
                                    *left
                                } else {
                                    Availability::MaybeMoved
                                };
                            }
                            state.loans.extend(on_true.loans);
                            state.loans.extend(on_false.loans);
                        }
                    }
                }
                Stmt::While { condition, body } => {
                    let mut iteration = state.clone();
                    self.inspect(condition, &mut iteration, UseMode::Read)?;
                    self.visit_block(body, &mut iteration)?;
                    self.check_loop_invariants(state, &iteration)?;
                }
                Stmt::Loop(body) => {
                    let mut iteration = state.clone();
                    self.visit_block(body, &mut iteration)?;
                    self.check_loop_invariants(state, &iteration)?;
                }
                Stmt::For {
                    local,
                    start,
                    end,
                    body,
                } => {
                    self.inspect(start, state, UseMode::Read)?;
                    self.inspect(end, state, UseMode::Read)?;
                    let mut iteration = state.clone();
                    iteration.available[*local] = Availability::Available;
                    self.visit_block(body, &mut iteration)?;
                    self.check_loop_invariants(state, &iteration)?;
                }
                Stmt::Unsafe(body) => {
                    terminates = self.visit_block(body, state)?;
                }
                Stmt::Break | Stmt::Continue => {
                    terminates = true;
                }
            }
        }
        // A borrow may not escape its referent's lexical scope, even when
        // assigned into a reference local declared outside this block.
        for (&borrower, &loan) in &state.loans {
            if declared.contains(&loan.source)
                && !declared.contains(&borrower)
                && state.available[borrower] == Availability::Available
            {
                return Err(format!(
                    "borrow of '{}' escapes its owner's scope through '{}'",
                    self.name(loan.source),
                    self.name(borrower)
                ));
            }
        }
        for local in declared {
            state.loans.remove(&local);
            state.protected.remove(&local);
        }
        Ok(terminates)
    }
    fn check_loop_invariants(&self, before: &State, after: &State) -> Result<(), String> {
        for (local, (first, last)) in before
            .available
            .iter()
            .zip(after.available.iter())
            .enumerate()
        {
            if *first == Availability::Available
                && *last != Availability::Available
                && !is_copy(
                    self.types,
                    self.function.locals[local].ty,
                    self.explicit_drop,
                )
            {
                return Err(format!(
                    "owner '{}' is consumed in a loop; reinitialize it before the next iteration",
                    self.name(local)
                ));
            }
        }
        Ok(())
    }
}
fn collect_locals(expr: &Expr, out: &mut HashSet<LocalId>) {
    match &expr.kind {
        ExprKind::Local(id) | ExprKind::MoveLocal(id) | ExprKind::AddressOf { local: id, .. } => {
            out.insert(*id);
        }
        ExprKind::Unary(_, item)
        | ExprKind::Deref(item)
        | ExprKind::Cast(item)
        | ExprKind::Field(item, _) => collect_locals(item, out),
        ExprKind::Binary(left, _, right) | ExprKind::Index(left, right) => {
            collect_locals(left, out);
            collect_locals(right, out);
        }
        ExprKind::Call(_, items) | ExprKind::StructInit(_, items) | ExprKind::Array(items) => {
            for item in items {
                collect_locals(item, out);
            }
        }
        ExprKind::CallIndirect(callee, items) => {
            collect_locals(callee, out);
            for item in items {
                collect_locals(item, out);
            }
        }
        _ => {}
    }
}
pub fn check(program: &mut hir::Program) -> Result<(), String> {
    let explicit_drop: HashSet<String> = program
        .functions
        .iter()
        .filter_map(|f| f.name.strip_suffix("__drop").map(str::to_string))
        .collect();
    let signatures = program.functions.clone();
    for function in &mut program.functions {
        if !matches!(function.kind, hir::FunctionKind::Defined) {
            continue;
        }
        let mut body = std::mem::take(&mut function.body);
        let checker = Checker {
            types: &program.types,
            function: &*function,
            signatures: &signatures,
            explicit_drop: &explicit_drop,
        };
        let mut state = State {
            available: vec![Availability::Moved; function.locals.len()],
            loans: HashMap::new(),
            protected: HashSet::new(),
        };
        for parameter in &function.params {
            state.available[*parameter] = Availability::Available;
        }
        checker
            .visit_block(&mut body, &mut state)
            .map_err(|error| {
                format!(
                    "{error}\n  = ownership: in function '{}'",
                    checker.function.name
                )
            })?;
        drop(checker);
        function.body = body;
    }
    Ok(())
}
