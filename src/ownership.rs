//! Flow-aware ownership / borrowing pass over typed HIR.
//!
//! Ownership and borrowing are compiler semantics, not runtime calls:
//! * `Copy` values may be read repeatedly, owning locals are consumed at by-value sites.
//! * `&T` is shared and `&mut T` is exclusive; outstanding lexical loans prevent moves.
//! * join points retain a value only when it exists on every predecessor.
//! * moved owners have their destruction suppressed through MIR drop flags.
//!
//! Borrowing currently has lexical lifetime inference. Precise NLL and struct
//! field partial moves are intentionally rejected; fixed-array moves at
//! constant indices use independent slot flags instead.
use crate::{
    hir::{self, Expr, ExprKind, LocalId, PlaceProjection, Stmt},
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
    // The first struct field selected from the root. Tracking this projection
    // lets `&mut self.player` coexist with `&self.level.map`, while retaining
    // exclusivity for two borrows of the same field.
    field: Option<usize>,
}
#[derive(Clone)]
struct State {
    available: Vec<Availability>,
    loans: HashMap<LocalId, Loan>, // borrower local -> source local
    protected: HashSet<LocalId>,   // named locals read by deferred cleanups
    // Fixed-array owner slots are tracked independently from the root. This
    // permits `let item = values[i]` without disabling cleanup for siblings.
    slots: HashMap<(LocalId, usize), Availability>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum UseMode {
    Read,
    Consume,
}

fn merge_slots(state: &mut State, left: &State, right: &State) {
    let keys: HashSet<(LocalId, usize)> = left
        .slots
        .keys()
        .chain(right.slots.keys())
        .copied()
        .collect();
    state.slots.clear();
    for key in keys {
        let left = left.slots.get(&key).copied().unwrap_or(Availability::Available);
        let right = right.slots.get(&key).copied().unwrap_or(Availability::Available);
        let merged = if left == right { left } else { Availability::MaybeMoved };
        if merged != Availability::Available {
            state.slots.insert(key, merged);
        }
    }
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
                // `class` is identity-bearing and therefore move-only even
                // when it does not currently define a destructor. A plain
                // `struct` may remain implicitly Copy when all of its fields
                // are Copyable; keeping this distinction aligned with the
                // concept registry prevents class handles from being copied
                // merely because their cleanup is provided externally.
                let owning = info.is_class
                    || explicit_drop.contains(&info.name)
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
    fn loans_overlap(left: Loan, right: Loan) -> bool {
        left.source == right.source && (left.field.is_none() || right.field.is_none() || left.field == right.field)
    }

    fn field_projection(place: &hir::Place) -> Option<usize> {
        place.projections.iter().find_map(|projection| match projection {
            PlaceProjection::Field(field) => Some(*field),
            PlaceProjection::Deref | PlaceProjection::Index(_) => None,
        })
    }

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
            ExprKind::If { condition, then_body, else_body, result } => {
                self.inspect(condition, state, UseMode::Read)?;
                let mut on_true = state.clone();
                let true_exits = self.visit_block(then_body, &mut on_true)?;
                let mut on_false = state.clone();
                let false_exits = self.visit_block(else_body, &mut on_false)?;
                match (true_exits, false_exits) {
                    (true, true) => return Err("both if branches return before yielding an expression".into()),
                    (true, false) => *state = on_false,
                    (false, true) => *state = on_true,
                    (false, false) => {
                        for (position,(left,right)) in on_true.available.iter().zip(&on_false.available).enumerate() {
                            state.available[position] = if left==right { *left } else { Availability::MaybeMoved };
                        }
                        merge_slots(state, &on_true, &on_false);
                        state.loans.extend(on_true.loans);
                        state.loans.extend(on_false.loans);
                    }
                }
                if let Some(local) = result {
                    self.alive(state,*local)?;
                    if mode == UseMode::Consume && owned {
                        self.ensure_not_borrowed(state,*local,"move")?;
                        state.available[*local] = Availability::Moved;
                    }
                }
                Ok(None)
            }
            ExprKind::Block { body, result } => {
                self.visit_block(body, state)?;
                if let Some(local) = result {
                    self.alive(state,*local)?;
                    if mode == UseMode::Consume && owned {
                        self.ensure_not_borrowed(state,*local,"move")?;
                        state.available[*local] = Availability::Moved;
                    }
                }
                Ok(None)
            }

            ExprKind::AddressOf { place, mutable } => {
                let root = place.root;
                self.alive(state, root)?;
                // Index calculations may depend on other named locals. Check
                // them as reads, never consume the aggregate being borrowed.
                for projection in &mut place.projections {
                    if let PlaceProjection::Index(index) = projection {
                        self.inspect(index, state, UseMode::Read)?;
                    }
                }
                if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. }) {
                    // Conservatively lock the entire root (even if indices differ).
                    // This also prevents mutating/reallocating an array while an
                    // element reference remains live.
                    let requested = Loan { source: root, exclusive: *mutable, field: Self::field_projection(place) };
                    if state.loans.values().any(|loan|
                        Self::loans_overlap(*loan, requested) && (*mutable || loan.exclusive)) {
                        return Err(format!("conflicting {}borrow of '{}'",
                            if *mutable { "mutable " } else { "shared " }, self.name(root)));
                    }
                    Ok(Some(requested))
                } else {
                    Ok(None)
                }
            }
            ExprKind::DynObjectRef { value, .. } => self.inspect(value, state, mode),
            ExprKind::Field(parent, _) => {
                // Reading a field does not consume its whole parent; moving
                // individual fields requires per-field drop flags (not yet legal).
                let _ = self.inspect(parent, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned && !self.function.name.ends_with("__drop") {
                    return Err("moving an owning field out of a struct is not supported; move the whole struct".into());
                }
                Ok(None)
            }
            ExprKind::TakeMatchField(parent, _) => {
                // A match owns the enum temporary. Extracting its single
                // active owning payload consumes that temporary, so its
                // generated Drop must not execute on this control-flow path.
                let ExprKind::Local(local) = &parent.kind else {
                    return Err("internal enum move requires a named match temporary".into());
                };
                let local = *local;
                if mode != UseMode::Consume {
                    return Err("cannot borrow an owning match payload after moving it".into());
                }
                self.alive(state, local)?;
                self.ensure_not_borrowed(state, local, "move")?;
                state.available[local] = Availability::Moved;
                Ok(None)
            }
            ExprKind::Index(parent, index) => {
                self.inspect(parent, state, UseMode::Read)?;
                self.inspect(index, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned && !self.function.name.ends_with("__drop") {
                    let Some(array) = (match &parent.kind {
                        ExprKind::Local(local) => Some(*local),
                        _ => None,
                    }) else {
                        return Err("moving an owning indexed element requires a direct array local".into());
                    };
                    let Some(slot) = (match &index.kind {
                        ExprKind::Int(value) if *value >= 0 => usize::try_from(*value).ok(),
                        _ => None,
                    }) else {
                        return Err("moving an owning array element requires a constant index".into());
                    };
                    self.alive(state, array)?;
                    self.ensure_not_borrowed(state, array, "move")?;
                    match state.slots.get(&(array, slot)).copied().unwrap_or(Availability::Available) {
                        Availability::Available => {}
                        Availability::Moved => return Err(format!("use of moved array element '{}[{slot}]'", self.name(array))),
                        Availability::MaybeMoved => return Err(format!("array element '{}[{slot}]' may have been moved", self.name(array))),
                    }
                    state.slots.insert((array, slot), Availability::Moved);
                    expression.kind = ExprKind::MoveIndex(parent.clone(), index.clone());
                }
                Ok(None)
            }
            ExprKind::MoveIndex(..) => Err("compiler bug: ownership pass was run twice".into()),
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
                let mut returned_loan = None;
                for (index, argument) in arguments.iter_mut().enumerate() {
                    let parameter_ty = signature.params.get(index).map(|p| signature.locals[*p].ty);
                    let mutable_reference = parameter_ty.is_some_and(|ty| matches!(
                        self.types.kind(ty), TypeKind::Reference { mutable: true, .. }
                    ));
                    // Passing an existing mutable reference to a method creates a
                    // temporary reborrow, not a move of the caller's capability.
                    let argument_mode = if mutable_reference || parameter_ty.is_none_or(|ty|
                        is_copy(self.types, ty, self.explicit_drop)) {
                        UseMode::Read
                    } else {
                        UseMode::Consume
                    };
                    let loan = self.inspect(argument, state, argument_mode)?;
                    let loan = if mutable_reference {
                        match &argument.kind {
                            ExprKind::Local(local) => {
                                // A reference parameter has an external referent;
                                // locally borrowed references retain their source.
                                loan.or(Some(Loan { source: *local, exclusive: true, field: None }))
                            }
                            _ => loan,
                        }
                    } else { loan };
                    if let Some(loan) = loan {
                        if temporary_loans.iter().any(|existing| {
                            Self::loans_overlap(*existing, loan) && (existing.exclusive || loan.exclusive)
                        }) {
                            return Err(format!(
                                "conflicting borrows of '{}' in the same function call",
                                self.name(loan.source)
                            ));
                        }
                        if index == 0 && mutable_reference {
                            returned_loan = Some(loan);
                        }
                        temporary_loans.push(loan);
                    }
                }
                if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. }) {
                    // Only a method returning its receiver's reference type can
                    // propagate a loan. Arbitrary borrowed function results still
                    // require explicit lifetime relationships.
                    let receiver_ty = signature.params.first().map(|p| signature.locals[*p].ty);
                    if !signature.name.contains("__") || receiver_ty != Some(expression.ty) {
                        return Err("borrowed return from a function needs explicit lifetime relationships (not supported in this release)".into());
                    }
                    return returned_loan.map(Some).ok_or_else(||
                        "cannot establish receiver lifetime for borrowed method result".into());
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
                            Self::loans_overlap(*previous, loan) && (previous.exclusive || loan.exclusive)
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
            ExprKind::Deref(inner) => {
                let borrow = self.inspect(inner, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned {
                    // HIR already requires an `unsafe` block for every raw
                    // dereference. A mutable raw pointer represents an
                    // explicitly unchecked storage capability: reading a T
                    // transfers ownership from that initialized slot. This
                    // is NOT permitted through an ordinary &T / &mut T.
                    if !matches!(self.types.kind(inner.ty), TypeKind::Pointer {mutable:true,..}) {
                        return Err("cannot move an owning value through a reference; use unsafe *mut T storage".into());
                    }
                }
                Ok(borrow)
            }
            ExprKind::Cast(inner) => {
                let borrow = self.inspect(inner, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned {
                    return Err("cannot move a non-Copy value through a cast; move the owner".into());
                }
                // The reference is an intermediate in `&mut value as *mut T`.
                // Raw pointers are unsafe capabilities, not borrow-checked
                // references: do not register the temporary reference's loan
                // against the new pointer local. Normal &T/&mut T values keep
                // their reference lifetime checks.
                if matches!(self.types.kind(expression.ty), TypeKind::Pointer { .. }) {
                    Ok(None)
                } else {
                    Ok(borrow)
                }
            }
            ExprKind::Unary(_, inner) => {
                let borrow = self.inspect(inner, state, UseMode::Read)?;
                if mode == UseMode::Consume && owned {
                    return Err("cannot move a non-Copy value through a cast; move the owner".into());
                }
                Ok(borrow)
            }
            ExprKind::Binary(left, _, right) => {
                self.inspect(left, state, UseMode::Read)?;
                self.inspect(right, state, UseMode::Read)?;
                Ok(None)
            }
            ExprKind::DropArray { .. } // Generated cleanup, not ordinary user code.
            | ExprKind::FunctionRef(_)
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
                .any(|old| Self::loans_overlap(*old, new_loan) && (old.exclusive || new_loan.exclusive))
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
                    state.slots.retain(|(root, _), _| *root != *local);
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
                    state.slots.retain(|(root, _), _| *root != *local);
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
                            merge_slots(state, &on_true, &on_false);
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
        for ((root, slot), first) in &before.slots {
            let last = after
                .slots
                .get(&(*root, *slot))
                .copied()
                .unwrap_or(Availability::Available);
            if *first == Availability::Available && last != Availability::Available {
                return Err(format!(
                    "array element '{}[{slot}]' is consumed in a loop; reinitialize it before the next iteration",
                    self.name(*root)
                ));
            }
        }
        Ok(())
    }
}
fn collect_stmt_locals(stmt: &Stmt, out: &mut HashSet<LocalId>) {
    match stmt {
        Stmt::Let { init, .. } | Stmt::Set { value:init, .. }
        | Stmt::Expr(init) | Stmt::DerefSet { value:init, .. }
        | Stmt::FieldSet { value:init, .. } | Stmt::Defer { expr:init, .. }
        | Stmt::Return(Some(init)) => collect_locals(init, out),
        Stmt::If { condition, then_body, else_body } => {
            collect_locals(condition,out);
            for s in then_body { collect_stmt_locals(s,out); }
            if let Some(body)=else_body { for s in body {collect_stmt_locals(s,out);} }
        }
        Stmt::While { condition, body } => {
            collect_locals(condition,out);
            for s in body {collect_stmt_locals(s,out);}
        }
        Stmt::For { start,end,body,.. } => {
            collect_locals(start,out); collect_locals(end,out);
            for s in body {collect_stmt_locals(s,out);}
        }
        Stmt::Loop(body) | Stmt::Unsafe(body) => {
            for s in body {collect_stmt_locals(s,out);}
        }
        _ => {}
    }
}
fn collect_locals(expr: &Expr, out: &mut HashSet<LocalId>) {
    match &expr.kind {
        ExprKind::If { condition, then_body, else_body, result } => {
            collect_locals(condition, out);
            if let Some(local) = result { out.insert(*local); }
            for stmt in then_body.iter().chain(else_body) {
                collect_stmt_locals(stmt, out);
            }
        }
        ExprKind::Block { body, result } => {
            if let Some(local) = result { out.insert(*local); }
            for stmt in body { collect_stmt_locals(stmt, out); }
        }
        ExprKind::Local(id) | ExprKind::MoveLocal(id)
        | ExprKind::DropArray { root: id, .. } => { out.insert(*id); }
        ExprKind::AddressOf { place, .. } => {
            out.insert(place.root);
            for projection in &place.projections {
                if let PlaceProjection::Index(index) = projection { collect_locals(index, out); }
            }
        }
        ExprKind::DynObjectRef { value, .. } => collect_locals(value, out),
        ExprKind::Unary(_, item)
        | ExprKind::Deref(item)
        | ExprKind::Cast(item)
        | ExprKind::Field(item, _)
        | ExprKind::TakeMatchField(item, _) => collect_locals(item, out),
        ExprKind::Binary(left, _, right) | ExprKind::Index(left, right) => {
            collect_locals(left, out);
            collect_locals(right, out);
        }
        ExprKind::MoveIndex(left, right) => {
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
    check_impl(program, None)
}
/// Collect one ownership/borrow error from each independent function.
/// No MIR is produced if any function violates the ownership contract.
pub fn check_all_errors(program: &mut hir::Program) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    check_impl(program, Some(&mut errors)).map_err(|error| {
        if errors.is_empty() { vec![error] } else { errors }
    })
}
fn check_impl(
    program: &mut hir::Program,
    mut collected: Option<&mut Vec<String>>,
) -> Result<(), String> {
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
            slots: HashMap::new(),
        };
        for parameter in &function.params {
            state.available[*parameter] = Availability::Available;
        }
        let result = checker.visit_block(&mut body, &mut state).map_err(|error| {
            format!("{error}\n  = ownership: in function '{}'", checker.function.name)
        });
        drop(checker);
        function.body = body;
        if let Err(error) = result {
            if let Some(errors) = collected.as_deref_mut() {
                if errors.len() < 64 { errors.push(error); }
                if errors.len() >= 64 { break; }
            } else { return Err(error); }
        }
    }
    if let Some(errors) = collected {
        if !errors.is_empty() {
            return Err(format!("{} ownership error(s)", errors.len()));
        }
    }
    Ok(())
}
