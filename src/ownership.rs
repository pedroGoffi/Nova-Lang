//! Flow-aware ownership / borrowing pass over typed HIR.
//!
//! Ownership and borrowing are compiler semantics, not runtime calls:
//! * `Copy` values may be read repeatedly, owning locals are consumed at by-value sites.
//! * `&T` is shared and `&mut T` is exclusive; outstanding lexical loans prevent moves.
//! * join points retain a value only when it exists on every predecessor.
//! * moved owners have their destruction suppressed through MIR drop flags.
//!
//! Borrowing uses block-local liveness and conservative control-flow joins.
//! A borrower may carry multiple possible origins after a branch join; all
//! origins are retained for subsequent conflict and escape checks. This is not
//! yet a full CFG/region-based borrow checker. Direct owning struct-field moves
//! use independent field states and MIR drop flags; fixed-array moves at
//! constant indices use slot flags.
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
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct LoanOrigin {
    source: LocalId,
    exclusive: bool,
    // Struct-field path from the root. Prefixes overlap: borrowing `x.a`
    // conflicts with `x.a.b`, while `x.a` and `x.b` are disjoint.
    // Dereferences and dynamic indices conservatively collapse to the root.
    fields: Option<Vec<usize>>,
}

/// A reference local can have several possible referents after a control-flow
/// join. Keeping every origin is essential: dropping one origin would permit
/// mutation through a path that is still borrowed on another predecessor.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Loan {
    origins: Vec<LoanOrigin>,
}
impl Loan {
    fn single(source: LocalId, exclusive: bool, fields: Option<Vec<usize>>) -> Self {
        Self { origins: vec![LoanOrigin { source, exclusive, fields }] }
    }

    fn insert_origin(&mut self, origin: LoanOrigin) {
        if !self.origins.contains(&origin) {
            self.origins.push(origin);
        }
    }

    fn first_source(&self) -> Option<LocalId> {
        self.origins.first().map(|origin| origin.source)
    }
}
#[derive(Clone)]
struct State {
    available: Vec<Availability>,
    loans: HashMap<LocalId, Loan>, // borrower local -> all possible source places
    protected: HashSet<LocalId>,   // named locals read by deferred cleanups
    // Fixed-array owner slots are tracked independently from the root. This
    // permits `let item = values[i]` without disabling cleanup for siblings.
    slots: HashMap<(LocalId, usize), Availability>,
    /// Independently initialized owning fields of direct struct locals.
    fields: HashMap<(LocalId, usize), Availability>,
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

fn merge_fields(state: &mut State, left: &State, right: &State) {
    let keys: HashSet<(LocalId, usize)> = left.fields.keys().chain(right.fields.keys()).copied().collect();
    state.fields.clear();
    for key in keys {
        let left = left.fields.get(&key).copied().unwrap_or(Availability::Available);
        let right = right.fields.get(&key).copied().unwrap_or(Availability::Available);
        let merged = if left == right { left } else { Availability::MaybeMoved };
        if merged != Availability::Available { state.fields.insert(key, merged); }
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
/// A small control-flow graph used by ownership liveness. Each HIR statement
/// gets a node; structured branches and loops become explicit edges. This is
/// intentionally an ownership CFG (not the MIR CFG): it is built before MIR
/// lowering and only tracks local uses/definitions needed by NLL.
#[derive(Clone, Debug, Default)]
struct CfgNode {
    uses: HashSet<LocalId>,
    defs: HashSet<LocalId>,
    successors: Vec<usize>,
    live_in: HashSet<LocalId>,
    live_out: HashSet<LocalId>,
}

#[derive(Clone, Debug, Default)]
struct OwnershipCfg {
    nodes: Vec<CfgNode>,
    root_nodes: Vec<usize>,
    // The continuation point after each statement in the analyzed block.
    // `None` means the statement has no reachable continuation (return,
    // break, or continue).
    root_after_nodes: Vec<Option<usize>>,
}

impl OwnershipCfg {
    fn add_node(&mut self, statement: Option<&Stmt>) -> usize {
        let mut node = CfgNode::default();
        if let Some(statement) = statement {
            match statement {
                Stmt::Let { local, init } => {
                    collect_locals(init, &mut node.uses);
                    node.defs.insert(*local);
                }
                Stmt::Set { local, value } => {
                    collect_locals(value, &mut node.uses);
                    node.defs.insert(*local);
                }
                Stmt::DerefSet { local, value } => {
                    node.uses.insert(*local);
                    collect_locals(value, &mut node.uses);
                }
                Stmt::FieldSet { root, value, .. } => {
                    node.uses.insert(*root);
                    collect_locals(value, &mut node.uses);
                }
                // A defer expression is evaluated at scope exit, not at its
                // declaration. Its uses are attached to cleanup edges by
                // `build_block` below.
                Stmt::Defer { .. } => {}
                Stmt::Expr(expr) => collect_locals(expr, &mut node.uses),
                Stmt::Return(value) => {
                    if let Some(value) = value {
                        collect_locals(value, &mut node.uses);
                    }
                }
                Stmt::If { condition, .. } | Stmt::While { condition, .. } => {
                    collect_locals(condition, &mut node.uses);
                }
                Stmt::For { local, start, end, .. } => {
                    collect_locals(start, &mut node.uses);
                    collect_locals(end, &mut node.uses);
                    node.defs.insert(*local);
                }
                Stmt::Loop(_) | Stmt::Break | Stmt::Continue | Stmt::Unsafe(_) => {}
            }
        }
        let id = self.nodes.len();
        self.nodes.push(node);
        id
    }

    fn edge(&mut self, from: usize, to: usize) {
        if !self.nodes[from].successors.contains(&to) {
            self.nodes[from].successors.push(to);
        }
    }

    fn build_block(
        &mut self,
        body: &[Stmt],
        mut incoming: Vec<usize>,
        loop_targets: Option<(usize, usize)>,
        root: bool,
    ) -> Vec<usize> {
        let mut deferred_uses = HashSet::new();
        for statement in body {
            if let Stmt::Defer { expr, .. } = statement {
                collect_locals(expr, &mut deferred_uses);
            }
            let node = self.add_node(Some(statement));
            if root {
                self.root_nodes.push(node);
            }
            for predecessor in incoming.drain(..) {
                self.edge(predecessor, node);
            }

            match statement {
                Stmt::If { then_body, else_body, .. } => {
                    let join = self.add_node(None);
                    let then_exits = self.build_block(then_body, vec![node], loop_targets, false);
                    for exit in then_exits.iter().copied() {
                        self.edge(exit, join);
                    }
                    if let Some(else_body) = else_body {
                        let else_exits = self.build_block(else_body, vec![node], loop_targets, false);
                        for exit in else_exits.iter().copied() {
                            self.edge(exit, join);
                        }
                        incoming = if then_exits.is_empty() && else_exits.is_empty() {
                            Vec::new()
                        } else {
                            vec![join]
                        };
                    } else {
                        self.edge(node, join);
                        incoming = vec![join];
                    }
                }
                Stmt::While { body, .. } | Stmt::Loop(body) => {
                    let exit = self.add_node(None);
                    // The edge to `exit` models a possible condition-false or
                    // externally terminated path. It is conservative for Loop.
                    self.edge(node, exit);
                    let body_exits = self.build_block(body, vec![node], Some((node, exit)), false);
                    for body_exit in body_exits {
                        self.edge(body_exit, node);
                    }
                    incoming = vec![exit];
                }
                Stmt::For { body, .. } => {
                    let exit = self.add_node(None);
                    self.edge(node, exit);
                    let body_exits = self.build_block(body, vec![node], Some((node, exit)), false);
                    for body_exit in body_exits {
                        self.edge(body_exit, node);
                    }
                    incoming = vec![exit];
                }
                Stmt::Unsafe(body) => {
                    incoming = self.build_block(body, vec![node], loop_targets, false);
                }
                Stmt::Break => {
                    if let Some((_, exit)) = loop_targets {
                        if deferred_uses.is_empty() {
                            self.edge(node, exit);
                        } else {
                            let cleanup = self.add_node(None);
                            self.nodes[cleanup].uses = deferred_uses.clone();
                            self.edge(node, cleanup);
                            self.edge(cleanup, exit);
                        }
                    }
                    incoming.clear();
                }
                Stmt::Continue => {
                    if let Some((header, _)) = loop_targets {
                        if deferred_uses.is_empty() {
                            self.edge(node, header);
                        } else {
                            let cleanup = self.add_node(None);
                            self.nodes[cleanup].uses = deferred_uses.clone();
                            self.edge(node, cleanup);
                            self.edge(cleanup, header);
                        }
                    }
                    incoming.clear();
                }
                Stmt::Return(_) => {
                    if !deferred_uses.is_empty() {
                        let cleanup = self.add_node(None);
                        self.nodes[cleanup].uses = deferred_uses.clone();
                        self.edge(node, cleanup);
                    }
                    incoming.clear();
                }
                _ => incoming = vec![node],
            }

            if root {
                if incoming.is_empty() {
                    self.root_after_nodes.push(None);
                } else {
                    let continuation = self.add_node(None);
                    for predecessor in incoming.drain(..) {
                        self.edge(predecessor, continuation);
                    }
                    incoming.push(continuation);
                    self.root_after_nodes.push(Some(continuation));
                }
            }
        }
        if !incoming.is_empty() && !deferred_uses.is_empty() {
            let cleanup = self.add_node(None);
            self.nodes[cleanup].uses = deferred_uses;
            for predecessor in incoming.drain(..) {
                self.edge(predecessor, cleanup);
            }
            incoming.push(cleanup);
        }
        incoming
    }

    fn from_block(body: &[Stmt]) -> Self {
        let mut cfg = Self::default();
        let entry = cfg.add_node(None);
        let exits = cfg.build_block(body, vec![entry], None, true);
        let exit = cfg.add_node(None);
        for predecessor in exits {
            cfg.edge(predecessor, exit);
        }
        cfg.solve_liveness();
        cfg
    }

    fn solve_liveness(&mut self) {
        // Backward may-analysis. Loops require iteration until a fixed point;
        // processing in reverse node order accelerates ordinary structured CFGs.
        loop {
            let mut changed = false;
            for index in (0..self.nodes.len()).rev() {
                let mut live_out = HashSet::new();
                for successor in &self.nodes[index].successors {
                    live_out.extend(self.nodes[*successor].live_in.iter().copied());
                }
                let mut live_in = self.nodes[index].uses.clone();
                live_in.extend(
                    live_out
                        .iter()
                        .filter(|local| !self.nodes[index].defs.contains(*local))
                        .copied(),
                );
                if live_out != self.nodes[index].live_out
                    || live_in != self.nodes[index].live_in
                {
                    self.nodes[index].live_out = live_out;
                    self.nodes[index].live_in = live_in;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn root_live_after(&self) -> Vec<HashSet<LocalId>> {
        self.root_after_nodes
            .iter()
            .map(|point| match *point {
                Some(node) => self.nodes[node].live_in.clone(),
                None => HashSet::new(),
            })
            .collect()
    }
}

struct Checker<'a> {
    types: &'a Types,
    function: &'a hir::Function,
    signatures: &'a [hir::Function],
    explicit_drop: &'a HashSet<String>,
}
impl Checker<'_> {
    fn origin_overlaps(left: &LoanOrigin, right: &LoanOrigin) -> bool {
        if left.source != right.source { return false; }
        match (&left.fields, &right.fields) {
            (Some(a), Some(b)) => a.iter().zip(b.iter()).all(|(x, y)| x == y),
            _ => true,
        }
    }

    fn loans_overlap(left: &Loan, right: &Loan) -> bool {
        left.origins.iter().any(|a| right.origins.iter().any(|b| Self::origin_overlaps(a, b)))
    }

    fn loans_conflict(left: &Loan, right: &Loan) -> bool {
        left.origins.iter().any(|a| {
            right.origins.iter().any(|b| {
                Self::origin_overlaps(a, b) && (a.exclusive || b.exclusive)
            })
        })
    }

    /// Join loan provenance from both reachable predecessors. A borrower must
    /// exist on both paths (definite initialization is checked separately),
    /// but its referent may differ. In that case retain the union of origins.
    /// All later conflict checks inspect every origin, so the join is
    /// conservative without rejecting valid conditional references.
    fn merge_loans_for_live_borrowers(
        &self,
        state: &mut State,
        left: &State,
        right: &State,
        live_borrowers: &HashSet<LocalId>,
    ) -> Result<(), String> {
        // A loan whose reference is dead at the join must not constrain the
        // continuation. In particular, a reference assigned on only one arm
        // is harmless when it is never read after the conditional. Keeping
        // such a loan used to reject valid code and could also retain stale
        // provenance beyond the reference's last use.
        let borrowers: HashSet<LocalId> = left
            .loans
            .keys()
            .chain(right.loans.keys())
            .filter(|borrower| live_borrowers.contains(borrower))
            .copied()
            .collect();
        let mut merged = HashMap::new();
        for borrower in borrowers {
            match (left.loans.get(&borrower), right.loans.get(&borrower)) {
                (Some(a), Some(b)) => {
                    let mut joined = a.clone();
                    for origin in &b.origins {
                        joined.insert_origin(origin.clone());
                    }
                    merged.insert(borrower, joined);
                }
                (None, None) => {}
                _ => {
                    return Err(format!(
                        "reference '{}' is borrowed on only one control-flow path; initialize it on every path before use",
                        self.name(borrower)
                    ));
                }
            }
        }
        state.loans = merged;
        Ok(())
    }

    fn merge_loans(&self, state: &mut State, left: &State, right: &State) -> Result<(), String> {
        // Expression joins do not yet carry statement-level liveness. Keep
        // their conservative behavior until expression CFG points are built.
        let borrowers: HashSet<LocalId> = left
            .loans
            .keys()
            .chain(right.loans.keys())
            .copied()
            .collect();
        self.merge_loans_for_live_borrowers(state, left, right, &borrowers)
    }

    fn field_projection(place: &hir::Place) -> Option<Vec<usize>> {
        let mut fields = Vec::new();
        for projection in &place.projections {
            match projection {
                PlaceProjection::Field(field) => fields.push(*field),
                // A dereference may alias arbitrary storage; an index may be
                // dynamic. Until those projections have their own provenance,
                // treat the borrow as covering the complete root local.
                PlaceProjection::Deref | PlaceProjection::Index(_) => return None,
            }
        }
        (!fields.is_empty()).then_some(fields)
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
        if state.loans.values().any(|loan| loan.origins.iter().any(|origin| origin.source == source)) {
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
    /// Require exclusive access to one direct field without unnecessarily
    /// invalidating loans to disjoint sibling fields. Whole-place operations
    /// continue to use `ensure_not_borrowed`.
    fn ensure_projection_not_borrowed(
        &self,
        state: &State,
        source: LocalId,
        fields: Vec<usize>,
        action: &str,
    ) -> Result<(), String> {
        let requested = Loan::single(source, true, Some(fields));
        if state.loans.values().any(|loan| Self::loans_overlap(loan, &requested)) {
            return Err(format!(
                "cannot {action} a field of '{}' while that field or an overlapping place is borrowed",
                self.name(source)
            ));
        }
        // `defer` currently records roots rather than projections. Retaining
        // the root-wide restriction is conservative and prevents cleanup from
        // observing a moved/replaced field.
        if state.protected.contains(&source) {
            return Err(format!(
                "cannot {action} a field of '{}' while a defer cleanup uses it",
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
                        .any(|loan| loan.origins.iter().any(|origin| origin.source == local && origin.exclusive))
                {
                    return Err(format!(
                        "cannot read '{}' while it has an active mutable borrow",
                        self.name(local)
                    ));
                }
                let existing_loan = state.loans.get(&local).cloned();
                if mode == UseMode::Consume && owned {
                    if state.fields.iter().any(|((root, _), availability)| *root == local && *availability != Availability::Available) {
                        return Err(format!("cannot move '{}' because one or more fields have already been moved", self.name(local)));
                    }
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
                        merge_fields(state, &on_true, &on_false);
                        self.merge_loans(state, &on_true, &on_false)?;
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
                let selected_projection = Self::field_projection(place);
                if let Some(field) = selected_projection.as_ref().and_then(|path| path.first()).copied() {
                    match state.fields.get(&(root, field)).copied().unwrap_or(Availability::Available) {
                        Availability::Available => {}
                        Availability::Moved => return Err(format!("cannot borrow moved field '{}.{}'", self.name(root), field)),
                        Availability::MaybeMoved => return Err(format!("cannot borrow field '{}.{}' because it may have been moved", self.name(root), field)),
                    }
                } else if state.fields.iter().any(|((owner, _), availability)| *owner == root && *availability != Availability::Available) {
                    return Err(format!("cannot borrow '{}' as a whole after one or more fields have been moved", self.name(root)));
                }
                // Index calculations may depend on other named locals. Check
                // them as reads, never consume the aggregate being borrowed.
                for projection in &mut place.projections {
                    if let PlaceProjection::Index(index) = projection {
                        self.inspect(index, state, UseMode::Read)?;
                    }
                }
                let tracks_provenance = matches!(
                    self.types.kind(expression.ty),
                    TypeKind::Reference { .. } | TypeKind::Pointer { .. }
                );
                if tracks_provenance {
                    // Conservatively lock the entire root (even if indices differ).
                    // This also prevents mutating/reallocating an array while an
                    // element reference remains live.
                    // If the place is a raw-pointer local, its stored loan is
                    // provenance for the pointee, not a borrow of the pointer
                    // variable itself. Reborrow from that original source so a
                    // returned reference is tied to `self` (or another real
                    // owner), never to the temporary pointer local.
                    let inherited = if self.is_raw_pointer_local(root) {
                        state.loans.get(&root).cloned()
                    } else {
                        None
                    };
                    let requested = if let Some(mut inherited) = inherited {
                        for origin in &mut inherited.origins {
                            origin.exclusive = origin.exclusive || *mutable;
                            if let (Some(existing), Some(extra)) =
                                (origin.fields.as_mut(), selected_projection.as_ref())
                            {
                                existing.extend(extra.iter().copied());
                            } else if origin.fields.is_none() && selected_projection.is_some() {
                                origin.fields = selected_projection.clone();
                            }
                        }
                        inherited
                    } else {
                        Loan::single(root, *mutable, selected_projection)
                    };
                    // A raw pointer only carries provenance for later escape
                    // checks. It is not a safe borrow and must not participate
                    // in the safe-reference alias-conflict rules.
                    if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. })
                        && state.loans.iter().any(|(borrower, loan)| {
                            *borrower != root
                                && !self.is_raw_pointer_local(*borrower)
                                && Self::loans_conflict(loan, &requested)
                        })
                    {
                        return Err(format!("conflicting {}borrow of '{}'",
                            if *mutable { "mutable " } else { "shared " },
                            self.name(requested.first_source().unwrap_or(root))));
                    }
                    Ok(Some(requested))
                } else {
                    Ok(None)
                }
            }
            ExprKind::DynObjectRef { value, .. } => self.inspect(value, state, mode),
            ExprKind::Field(parent, index) => {
                let field_index = *index;
                let parent_local = match &parent.kind { ExprKind::Local(local) => Some(*local), _ => None };
                let _ = self.inspect(parent, state, UseMode::Read)?;
                if let Some(root) = parent_local {
                    if let TypeKind::Struct(struct_id) = self.types.kind(parent.ty) {
                        match state.fields.get(&(root, field_index)).copied().unwrap_or(Availability::Available) {
                            Availability::Available => {}
                            Availability::Moved => return Err(format!("use of moved field '{}.{}'", self.name(root), self.types.structs[struct_id].fields[field_index].name)),
                            Availability::MaybeMoved => return Err(format!("field '{}.{}' may have been moved on another control-flow path", self.name(root), self.types.structs[struct_id].fields[field_index].name)),
                        }
                        if mode == UseMode::Consume && owned {
                            let info = &self.types.structs[struct_id];
                            if info.has_custom_drop && self.function.name == format!("{}__drop", info.name) {
                                return Err(format!("custom destructor '{}' cannot move field '{}'; fields are destroyed automatically after the destructor body", info.name, info.fields[field_index].name));
                            }
                            // Compiler-generated __drop bodies already contain
                            // the canonical field cleanup calls. Do not rewrite
                            // those calls as user-level partial moves.
                            if self.function.name == format!("{}__drop", info.name) && !info.has_custom_drop {
                                return Ok(None);
                            }
                            if info.has_custom_drop {
                                return Err(format!("cannot move field '{}.{}' out of a type with a custom destructor", self.name(root), info.fields[field_index].name));
                            }
                            if info.fields.first().is_some_and(|field| field.name == "__nova_tag") {
                                return Err("partial moves from enum payload storage are not supported".into());
                            }
                            if info.fields.iter().any(|field| matches!(self.types.kind(field.ty), TypeKind::Array { .. }) && owns_value(self.types, field.ty, self.explicit_drop)) {
                                return Err("partial moves are not yet supported for structs containing owning array fields".into());
                            }
                            // Only direct struct fields have an independently
                            // generated destructor in this ABI revision.
                            let field_ty = info.fields[field_index].ty;
                            if !matches!(self.types.kind(field_ty), TypeKind::Struct(_)) {
                                return Err("partial move is currently supported only for direct struct fields; arrays and nested projections must be moved as a whole".into());
                            }
                            self.ensure_projection_not_borrowed(state, root, vec![field_index], "move")?;
                            state.fields.insert((root, field_index), Availability::Moved);
                            expression.kind = ExprKind::MoveField { root, index: field_index };
                        }
                    } else if mode == UseMode::Consume && owned {
                        return Err("moving an owning field requires a direct struct local".into());
                    }
                } else if mode == UseMode::Consume && owned && !self.function.name.ends_with("__drop") {
                    return Err("moving an owning field requires a direct struct local".into());
                }
                Ok(None)
            }
            ExprKind::MoveField { .. } => Err("compiler bug: ownership pass was run twice".into()),
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
                let mut borrowed_arguments: Vec<(usize, Loan)> = Vec::new();
                let mut receiver_is_reference = false;
                for (index, argument) in arguments.iter_mut().enumerate() {
                    let parameter_ty = signature.params.get(index).map(|p| signature.locals[*p].ty);
                    let reference_parameter = parameter_ty.and_then(|ty| match self.types.kind(ty) {
                        TypeKind::Reference { mutable, .. } => Some(mutable),
                        _ => None,
                    });
                    let mutable_reference = reference_parameter == Some(true);
                    let is_receiver = signature.params.get(index).is_some_and(|p|
                        signature.locals[*p].name == "self");
                    if is_receiver && reference_parameter.is_some() {
                        receiver_is_reference = true;
                    }

                    // Passing a reference parameter creates a reborrow. Preserve
                    // an existing local provenance when available; otherwise the
                    // reference parameter itself is the lifetime source.
                    let argument_mode = if reference_parameter.is_some()
                        || parameter_ty.is_none_or(|ty| is_copy(self.types, ty, self.explicit_drop)) {
                        UseMode::Read
                    } else {
                        UseMode::Consume
                    };
                    let loan = self.inspect(argument, state, argument_mode)?;
                    let loan = if reference_parameter.is_some() {
                        match &argument.kind {
                            ExprKind::Local(local) | ExprKind::MoveLocal(local) => {
                                loan.or(Some(Loan::single(*local, mutable_reference, None)))
                            }
                            _ => loan,
                        }
                    } else { loan };
                    if let Some(loan) = loan {
                        if temporary_loans.iter().any(|existing| Self::loans_conflict(existing, &loan)) {
                            return Err(format!(
                                "conflicting borrows of '{}' in the same function call",
                                self.name(loan.first_source().unwrap_or(0))
                            ));
                        }
                        if reference_parameter.is_some() {
                            borrowed_arguments.push((index, loan.clone()));
                        }
                        temporary_loans.push(loan);
                    }
                }
                if matches!(self.types.kind(expression.ty), TypeKind::Reference { .. }) {
                    // Lifetime elision for borrowed results:
                    // 1. A reference receiver (`self: &T` / `&mut T`) is the
                    //    source, regardless of the returned referent type.
                    // 2. For free functions, a single borrowed input is the
                    //    source. Multiple borrowed inputs are ambiguous and
                    //    require an explicit lifetime relationship (not yet
                    //    expressible in this language release).
                    let selected = if receiver_is_reference {
                        borrowed_arguments.iter().find(|(index, _)|
                            signature.params.get(*index).is_some_and(|p| signature.locals[*p].name == "self"))
                    } else if borrowed_arguments.len() == 1 {
                        borrowed_arguments.first()
                    } else {
                        None
                    };
                    return selected.map(|(_, loan)| Some(loan.clone())).ok_or_else(|| {
                        if borrowed_arguments.is_empty() {
                            "cannot establish lifetime for borrowed return: no borrowed input parameter".into()
                        } else {
                            "borrowed return is ambiguous: multiple reference parameters require an explicit lifetime relationship".into()
                        }
                    });
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
                            Self::loans_conflict(previous, &loan)
                        }) {
                            return Err(format!(
                                "conflicting borrows of '{}' in indirect call",
                                self.name(loan.first_source().unwrap_or(0))
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
                        return Err(format!("storing borrowed '{}' in an aggregate requires explicit lifetime parameters",self.name(loan.first_source().unwrap_or(0))));
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
                // Preserve the source loan when a reference is converted to
                // a raw pointer. Raw pointers are not themselves borrow-checked
                // for aliasing, but retaining this provenance is necessary to
                // prove that a later `&*pointer` cannot outlive the storage it
                // points into. In particular, this supports unsafe APIs that
                // reborrow a field through a raw pointer local.
                Ok(borrow)
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
            ExprKind::DropArray { .. } | ExprKind::DropStruct { .. } // Generated cleanup, not ordinary user code.
            | ExprKind::FunctionRef(_)
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Unit
            | ExprKind::Zero => Ok(None),
        }
    }
    fn is_raw_pointer_local(&self, local: LocalId) -> bool {
        matches!(self.types.kind(self.function.locals[local].ty), TypeKind::Pointer { .. })
    }

    fn record_loan(
        &self,
        state: &mut State,
        borrower: LocalId,
        loan: Option<Loan>,
    ) -> Result<(), String> {
        if let Some(new_loan) = loan {
            // A raw pointer local carries provenance so a later unsafe reborrow
            // can be checked for escape, but it is not itself a live safe borrow.
            // Exclude such provenance carriers from alias-conflict checks.
            if state.loans.iter().any(|(old_borrower, old)| {
                *old_borrower != borrower
                    && !self.is_raw_pointer_local(*old_borrower)
                    && Self::loans_conflict(old, &new_loan)
            }) {
                return Err(format!(
                    "conflicting borrow of '{}'",
                    self.name(new_loan.first_source().unwrap_or(0))
                ));
            }
            state.loans.insert(borrower, new_loan);
        }
        Ok(())
    }
    fn visit_block(&self, body: &mut [Stmt], state: &mut State) -> Result<bool, String> {
        let mut declared = Vec::new();
        let mut terminates = false;
        // Compute local liveness at each statement boundary. A reference loan
        // is needed only while its borrower is used; keeping it until the end
        // of the lexical scope is unnecessarily conservative and is not NLL.
        // The analysis is deliberately block-local: outer borrowers remain
        // live across nested blocks and are pruned by their declaring block.
        let ownership_cfg = OwnershipCfg::from_block(body);
        let live_after = ownership_cfg.root_live_after();
        debug_assert_eq!(live_after.len(), body.len());
        for (statement_index, statement) in body.iter_mut().enumerate() {
            if terminates {
                break;
            }
            match statement {
                Stmt::Let { local, init } => {
                    let loan = self.inspect(init, state, UseMode::Consume)?;
                    state.available[*local] = Availability::Available;
                    state.slots.retain(|(root, _), _| *root != *local);
                    state.fields.retain(|(root, _), _| *root != *local);
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
                    state.fields.retain(|(root, _), _| *root != *local);
                    self.record_loan(state, *local, loan)?;
                }
                Stmt::DerefSet { local, value } => {
                    self.alive(state, *local)?;
                    // A mut borrow is the unique capability to write through
                    // this reference. No second alias may coexist.
                    self.inspect(value, state, UseMode::Consume)?;
                }
                Stmt::FieldSet { root, index, value, .. } => {
                    self.alive(state, *root)?;
                    self.ensure_projection_not_borrowed(state, *root, vec![*index], "mutate")?;
                    if let TypeKind::Struct(struct_id) = self.types.kind(self.function.locals[*root].ty) {
                        let field_ty = self.types.structs[struct_id].fields[*index].ty;
                        if owns_value(self.types, field_ty, self.explicit_drop) {
                            match state.fields.get(&(*root, *index)).copied().unwrap_or(Availability::Available) {
                                Availability::Available => return Err(format!("cannot overwrite live owning field '{}.{}'; move or replace it first", self.name(*root), self.types.structs[struct_id].fields[*index].name)),
                                Availability::MaybeMoved => return Err(format!("cannot assign '{}.{}' because its initialization state differs across control-flow paths", self.name(*root), self.types.structs[struct_id].fields[*index].name)),
                                Availability::Moved => {}
                            }
                            state.fields.insert((*root, *index), Availability::Available);
                        }
                    }
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
                            // Returning a reference parameter directly (for example,
                            // `return self` from a fluent `&mut Self` method) is also
                            // a valid lifetime proof. `inspect(Local)` has no stored
                            // loan for function parameters, so reconstruct the origin
                            // from the parameter's own reference type here.
                            let direct_parameter_loan = match &value.kind {
                                ExprKind::Local(local) | ExprKind::MoveLocal(local)
                                    if self.function.params.contains(local) =>
                                {
                                    match self.types.kind(value.ty) {
                                        TypeKind::Reference { mutable, .. } => {
                                            Some(Loan::single(*local, mutable, None))
                                        }
                                        _ => None,
                                    }
                                }
                                _ => None,
                            };
                            // A returned borrow need not be the parameter expression
                            // itself. Field/index projections such as
                            // `return &self.data[index]` are valid when their place
                            // root is a reference parameter. `inspect` records that
                            // root in the loan, which is the actual lifetime proof.
                            let returned_loan = loan.or(direct_parameter_loan);
                            let Some(returned_loan) = returned_loan.as_ref() else {
                                return Err("cannot return borrowed reference without lifetime proof; the reference must be derived from a reference parameter".into());
                            };
                            if let Some(origin) = returned_loan.origins.iter().find(
                                |origin| !self.function.params.contains(&origin.source)
                            ) {
                                return Err(format!(
                                    "cannot return reference to local '{}' (borrow would outlive owner)",
                                    self.name(origin.source)
                                ));
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
                            merge_fields(state, &on_true, &on_false);
                            self.merge_loans_for_live_borrowers(
                                state,
                                &on_true,
                                &on_false,
                                &live_after[statement_index],
                            )?;
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
            // NLL: after a statement, release loans whose reference locals
            // have no remaining uses in this lexical block. Only loans whose
            // borrower was declared here are considered; references declared
            // by an enclosing block may still be used after this nested block.
            let live = &live_after[statement_index];
            state.loans.retain(|borrower, _| {
                !declared.contains(borrower) || live.contains(borrower)
            });
        }
        // A borrow may not escape its referent's lexical scope, even when
        // assigned into a reference local declared outside this block.
        for (&borrower, loan) in &state.loans {
            if loan.origins.iter().any(|origin| declared.contains(&origin.source))
                && !declared.contains(&borrower)
                && state.available[borrower] == Availability::Available
            {
                return Err(format!(
                    "borrow of '{}' escapes its owner's scope through '{}'",
                    self.name(loan.first_source().unwrap_or(0)),
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
        let field_keys: HashSet<(LocalId, usize)> = before.fields.keys().chain(after.fields.keys()).copied().collect();
        for (root, field) in field_keys {
            let first = before.fields.get(&(root, field)).copied().unwrap_or(Availability::Available);
            let last = after.fields.get(&(root, field)).copied().unwrap_or(Availability::Available);
            if first == Availability::Available && last != Availability::Available {
                return Err(format!("field '{}.{}' is moved in a loop; reinitialize it before the next iteration", self.name(root), field));
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
        Stmt::Let { init, .. }
        | Stmt::Set { value: init, .. }
        | Stmt::Expr(init)
        | Stmt::Defer { expr: init, .. }
        | Stmt::Return(Some(init)) => collect_locals(init, out),
        Stmt::DerefSet { local, value } => {
            out.insert(*local);
            collect_locals(value, out);
        }
        Stmt::FieldSet { root, value, .. } => {
            out.insert(*root);
            collect_locals(value, out);
        }
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
        | ExprKind::DropArray { root: id, .. }
        | ExprKind::DropStruct { root: id, .. }
        | ExprKind::MoveField { root: id, .. } => { out.insert(*id); }
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
            fields: HashMap::new(),
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

#[cfg(test)]
mod loan_join_tests {
    use super::*;

    fn checker() -> (Types, hir::Function) {
        let types = Types::new();
        let function = hir::Function {
            name: "join_test".into(),
            params: vec![],
            locals: vec![
                hir::Local { name: "left".into(), ty: types.unit(), mutable: true },
                hir::Local { name: "right".into(), ty: types.unit(), mutable: true },
                hir::Local { name: "reference".into(), ty: types.unit(), mutable: true },
            ],
            result: types.unit(),
            body: vec![],
            kind: hir::FunctionKind::Defined,
            c_variadic: false,
        };
        (types, function)
    }

    fn state_with_loan(borrower: LocalId, source: LocalId) -> State {
        let mut state = State {
            available: vec![Availability::Available; 3],
            loans: HashMap::new(),
            protected: HashSet::new(),
            slots: HashMap::new(),
            fields: HashMap::new(),
        };
        state.loans.insert(borrower, Loan::single(source, true, None));
        state
    }

    #[test]
    fn nested_field_borrows_overlap_when_one_projection_is_a_prefix() {
        let parent = Loan::single(0, true, Some(vec![1]));
        let child = Loan::single(0, false, Some(vec![1, 2]));
        assert!(Checker::loans_overlap(&parent, &child));
    }

    #[test]
    fn sibling_nested_field_borrows_are_disjoint() {
        let left = Loan::single(0, true, Some(vec![1, 2]));
        let right = Loan::single(0, false, Some(vec![1, 3]));
        assert!(!Checker::loans_overlap(&left, &right));
    }

    #[test]
    fn dynamic_or_dereferenced_borrows_overlap_every_projection() {
        let whole = Loan::single(0, true, None);
        let nested = Loan::single(0, false, Some(vec![1, 2]));
        assert!(Checker::loans_overlap(&whole, &nested));
    }

    #[test]
    fn mutation_of_sibling_field_is_allowed_while_other_field_is_borrowed() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let mut state = state_with_loan(2, 0);
        state.loans.get_mut(&2).unwrap().origins[0].fields = Some(vec![0]);
        assert!(checker.ensure_projection_not_borrowed(&state, 0, vec![1], "mutate").is_ok());
    }

    #[test]
    fn mutation_of_borrowed_field_is_rejected() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let mut state = state_with_loan(2, 0);
        state.loans.get_mut(&2).unwrap().origins[0].fields = Some(vec![0, 1]);
        let error = checker.ensure_projection_not_borrowed(&state, 0, vec![0], "mutate").unwrap_err();
        assert!(error.contains("overlapping place"), "{error}");
    }

    #[test]
    fn whole_object_borrow_conflicts_with_every_field_mutation() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let state = state_with_loan(2, 0);
        let error = checker.ensure_projection_not_borrowed(&state, 0, vec![1], "mutate").unwrap_err();
        assert!(error.contains("overlapping place"), "{error}");
    }

    #[test]
    fn branch_join_preserves_identical_loan_provenance() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let left = state_with_loan(2, 0);
        let right = state_with_loan(2, 0);
        let mut joined = left.clone();
        checker.merge_loans(&mut joined, &left, &right).unwrap();
        assert_eq!(joined.loans.get(&2), Some(&Loan::single(0, true, None)));
    }

    #[test]
    fn branch_join_preserves_all_possible_loan_origins() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let left = state_with_loan(2, 0);
        let right = state_with_loan(2, 1);
        let mut joined = left.clone();
        checker.merge_loans(&mut joined, &left, &right).unwrap();
        let origins = &joined.loans.get(&2).unwrap().origins;
        assert_eq!(origins.len(), 2);
        assert!(origins.iter().any(|origin| origin.source == 0));
        assert!(origins.iter().any(|origin| origin.source == 1));
    }

    #[test]
    fn joined_loan_conflicts_with_every_possible_origin() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let left = state_with_loan(2, 0);
        let right = state_with_loan(2, 1);
        let mut joined = left.clone();
        checker.merge_loans(&mut joined, &left, &right).unwrap();
        let borrow_first = Loan::single(0, true, None);
        let borrow_second = Loan::single(1, true, None);
        assert!(Checker::loans_overlap(joined.loans.get(&2).unwrap(), &borrow_first));
        assert!(Checker::loans_overlap(joined.loans.get(&2).unwrap(), &borrow_second));
    }

    #[test]
    fn mutation_is_blocked_for_each_origin_after_a_branch_join() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let left = state_with_loan(2, 0);
        let right = state_with_loan(2, 1);
        let mut joined = left.clone();
        checker.merge_loans(&mut joined, &left, &right).unwrap();
        assert!(checker.ensure_not_borrowed(&joined, 0, "mutate").is_err());
        assert!(checker.ensure_not_borrowed(&joined, 1, "mutate").is_err());
    }

    #[test]
    fn shared_origin_does_not_inherit_exclusivity_from_an_unrelated_origin() {
        let mut mixed = Loan::single(0, false, None);
        mixed.insert_origin(LoanOrigin { source: 1, exclusive: true, fields: None });
        let shared_on_first = Loan::single(0, false, None);
        assert!(!Checker::loans_conflict(&mixed, &shared_on_first));
        let exclusive_on_second = Loan::single(1, false, None);
        assert!(Checker::loans_conflict(&mixed, &exclusive_on_second));
    }

    #[test]
    fn dead_borrower_present_on_only_one_branch_is_pruned_at_join() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker {
            types: &types,
            function: &function,
            signatures: &[],
            explicit_drop: &explicit_drop,
        };
        let left = state_with_loan(2, 0);
        let right = State {
            available: vec![Availability::Available; 3],
            loans: HashMap::new(),
            protected: HashSet::new(),
            slots: HashMap::new(),
            fields: HashMap::new(),
        };
        let mut joined = left.clone();
        let live = HashSet::new();
        checker
            .merge_loans_for_live_borrowers(&mut joined, &left, &right, &live)
            .unwrap();
        assert!(!joined.loans.contains_key(&2));
    }

    #[test]
    fn live_borrower_present_on_only_one_branch_is_rejected() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker {
            types: &types,
            function: &function,
            signatures: &[],
            explicit_drop: &explicit_drop,
        };
        let left = state_with_loan(2, 0);
        let right = State {
            available: vec![Availability::Available; 3],
            loans: HashMap::new(),
            protected: HashSet::new(),
            slots: HashMap::new(),
            fields: HashMap::new(),
        };
        let mut joined = left.clone();
        let live = HashSet::from([2]);
        let error = checker
            .merge_loans_for_live_borrowers(&mut joined, &left, &right, &live)
            .unwrap_err();
        assert!(error.contains("only one control-flow path"), "{error}");
    }

    #[test]
    fn branch_join_rejects_loan_present_on_only_one_path() {
        let (types, function) = checker();
        let explicit_drop = HashSet::new();
        let checker = Checker { types: &types, function: &function, signatures: &[], explicit_drop: &explicit_drop };
        let left = state_with_loan(2, 0);
        let right = State {
            available: vec![Availability::Available; 3],
            loans: HashMap::new(),
            protected: HashSet::new(),
            slots: HashMap::new(),
            fields: HashMap::new(),
        };
        let mut joined = left.clone();
        let error = checker.merge_loans(&mut joined, &left, &right).unwrap_err();
        assert!(error.contains("only one control-flow path"), "{error}");
    }

    #[test]
    fn liveness_tracks_reference_used_as_deref_assignment_target() {
        let mut locals = HashSet::new();
        let stmt = Stmt::DerefSet {
            local: 2,
            value: Expr {
                kind: ExprKind::Int(7),
                ty: 0,
            },
        };
        collect_stmt_locals(&stmt, &mut locals);
        assert!(locals.contains(&2));
    }

    #[test]
    fn liveness_tracks_root_used_by_field_assignment() {
        let mut locals = HashSet::new();
        let stmt = Stmt::FieldSet {
            root: 1,
            pointer: false,
            index: 0,
            value: Expr {
                kind: ExprKind::Int(7),
                ty: 0,
            },
            // The exact source span is irrelevant to liveness collection.
        };
        collect_stmt_locals(&stmt, &mut locals);
        assert!(locals.contains(&1));
    }

    #[test]
    fn cfg_liveness_kills_old_value_at_assignment() {
        let body = vec![
            Stmt::Set {
                local: 2,
                value: Expr { kind: ExprKind::Int(9), ty: 0 },
            },
            Stmt::Expr(Expr { kind: ExprKind::Local(2), ty: 0 }),
        ];
        let cfg = OwnershipCfg::from_block(&body);
        let first = cfg.root_nodes[0];
        assert!(cfg.nodes[first].live_out.contains(&2));
        assert!(!cfg.nodes[first].live_in.contains(&2));
    }

    #[test]
    fn cfg_liveness_models_reference_reassignment_as_a_definition() {
        let body = vec![
            Stmt::Set {
                local: 2,
                value: Expr { kind: ExprKind::Local(1), ty: 0 },
            },
            Stmt::Expr(Expr { kind: ExprKind::Local(2), ty: 0 }),
        ];
        let cfg = OwnershipCfg::from_block(&body);
        let first = cfg.root_nodes[0];
        let live_after = cfg.root_live_after();
        assert!(cfg.nodes[first].live_out.contains(&2));
        assert!(!cfg.nodes[first].live_in.contains(&2));
        assert!(live_after[0].contains(&2));
    }

    #[test]
    fn cfg_liveness_unions_branch_uses_but_expires_them_after_the_join() {
        let body = vec![
            Stmt::If {
                condition: Expr { kind: ExprKind::Bool(true), ty: 0 },
                then_body: vec![Stmt::Expr(Expr { kind: ExprKind::Local(1), ty: 0 })],
                else_body: Some(vec![Stmt::Expr(Expr { kind: ExprKind::Local(2), ty: 0 })]),
            },
        ];
        let cfg = OwnershipCfg::from_block(&body);
        let live_after = cfg.root_live_after();
        assert!(!live_after[0].contains(&1));
        assert!(!live_after[0].contains(&2));
    }

    #[test]
    fn cfg_liveness_keeps_value_used_after_branch_join() {
        let body = vec![
            Stmt::If {
                condition: Expr { kind: ExprKind::Bool(true), ty: 0 },
                then_body: vec![],
                else_body: Some(vec![]),
            },
            Stmt::Expr(Expr { kind: ExprKind::Local(2), ty: 0 }),
        ];
        let cfg = OwnershipCfg::from_block(&body);
        let live_after = cfg.root_live_after();
        assert!(live_after[0].contains(&2));
    }

    #[test]
    fn cfg_liveness_keeps_deferred_references_alive_until_scope_exit() {
        let body = vec![
            Stmt::Defer {
                expr: Expr { kind: ExprKind::Local(2), ty: 0 },
                owner: None,
            },
            Stmt::Expr(Expr { kind: ExprKind::Int(0), ty: 0 }),
        ];
        let cfg = OwnershipCfg::from_block(&body);
        let live_after = cfg.root_live_after();
        assert!(live_after[0].contains(&2));
    }

    #[test]
    fn cfg_liveness_reaches_fixed_point_through_loop_backedge() {
        let body = vec![
            Stmt::While {
                condition: Expr { kind: ExprKind::Bool(true), ty: 0 },
                body: vec![Stmt::Expr(Expr { kind: ExprKind::Local(1), ty: 0 })],
            },
        ];
        let cfg = OwnershipCfg::from_block(&body);
        let live = &cfg.nodes[cfg.root_nodes[0]].live_out;
        assert!(live.contains(&1));
    }

}
