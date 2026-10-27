//! Typed lower-level IR with explicit basic blocks, places and terminators.
use crate::{
    ast::{BinaryOp, UnaryOp},
    hir::{self, ExprKind, FunctionKind, Stmt},
    types::{TypeId, TypeKind, Types},
};
use std::collections::{HashSet, VecDeque};

pub type BlockId = usize;
pub type LocalId = usize;
#[derive(Debug, Clone)]
pub struct Program {
    pub types: Types,
    pub functions: Vec<Function>,
}
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<LocalId>,
    pub locals: Vec<TypeId>,
    pub result: TypeId,
    pub blocks: Vec<BasicBlock>,
    pub kind: FunctionKind,
    pub c_variadic: bool,
}
#[derive(Debug, Clone, Default)]
pub struct BasicBlock {
    pub instructions: Vec<Instruction>,
    pub terminator: Option<Terminator>,
}
#[derive(Debug, Clone)]
pub enum Instruction {
    Assign {
        dst: LocalId,
        value: Rvalue,
    },
    StoreField {
        root: LocalId,
        pointer: bool,
        index: usize,
        value: Operand,
    },
    StoreDeref {
        pointer: LocalId,
        value: Operand,
    },
    Eval(Rvalue),
}
#[derive(Debug, Clone)]
pub enum Rvalue {
    Use(Operand),
    Unary(UnaryOp, Operand),
    Binary(BinaryOp, Operand, Operand),
    Call(usize, Vec<Operand>),
    FnRef(usize, TypeId),
    CallIndirect(Operand, Vec<Operand>),
    StructInit(usize, Vec<Operand>),
    Field(Operand, usize),
    Array(Vec<Operand>),
    Index(Operand, Operand),
    AddressOf(LocalId),
    Deref(Operand),
    Cast(Operand, TypeId),
}
#[derive(Debug, Clone)]
pub enum Operand {
    Local(LocalId),
    Int(i128, TypeId),
    Float(f64, TypeId),
    Bool(bool),
    Str(String),
    Unit,
    Zero(TypeId),
}
#[derive(Debug, Clone)]
pub enum Terminator {
    Goto(BlockId),
    Branch {
        condition: Operand,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    Return(Option<Operand>),
    Unreachable,
}
#[derive(Clone)]
struct Cleanup {
    expr: hir::Expr,
    owner: Option<LocalId>,
}
struct Builder {
    function: Function,
    current: BlockId,
    types: Types,
    loops: Vec<(BlockId, BlockId, usize)>,
    // One cleanup frame per active lexical scope. Entries are installed when
    // their `defer` declaration is reached, and executed in LIFO order.
    cleanups: Vec<Vec<Cleanup>>,
    /// Owner local -> bool local holding its initialized/not-moved state.
    drop_flags: std::collections::HashMap<LocalId, LocalId>,
}
impl Builder {
    fn new(f: &hir::Function, types: &Types) -> Self {
        Self {
            function: Function {
                name: f.name.clone(),
                params: f.params.clone(),
                locals: f.locals.iter().map(|l| l.ty).collect(),
                result: f.result,
                blocks: vec![BasicBlock::default()],
                kind: f.kind,
                c_variadic: f.c_variadic,
            },
            current: 0,
            types: types.clone(),
            loops: Vec::new(),
            cleanups: Vec::new(),
            drop_flags: std::collections::HashMap::new(),
        }
    }
    fn new_block(&mut self) -> BlockId {
        let id = self.function.blocks.len();
        self.function.blocks.push(BasicBlock::default());
        id
    }
    fn temp(&mut self, ty: TypeId) -> LocalId {
        let id = self.function.locals.len();
        self.function.locals.push(ty);
        id
    }
    fn assign(&mut self, dst: LocalId, value: Rvalue) {
        self.function.blocks[self.current]
            .instructions
            .push(Instruction::Assign { dst, value });
    }
    fn end(&mut self, term: Terminator) {
        debug_assert!(self.function.blocks[self.current].terminator.is_none());
        self.function.blocks[self.current].terminator = Some(term);
    }
    fn is_terminated(&self) -> bool {
        self.function.blocks[self.current].terminator.is_some()
    }
    fn expression(&mut self, e: &hir::Expr) -> Operand {
        match &e.kind {
            ExprKind::Cast(inner) => {
                let val = self.expression(inner);
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Cast(val, e.ty));
                Operand::Local(dst)
            }
            ExprKind::Int(n) => Operand::Int(*n, e.ty),
            ExprKind::Float(n) => Operand::Float(*n, e.ty),
            ExprKind::Bool(b) => Operand::Bool(*b),
            ExprKind::Str(s) => Operand::Str(s.clone()),
            ExprKind::Unit => Operand::Unit,
            ExprKind::Zero => Operand::Zero(e.ty),
            ExprKind::Local(id) => Operand::Local(*id),
            ExprKind::MoveLocal(owner) => {
                // First snapshot the value, then invalidate its old owner.
                let snapshot = self.temp(e.ty);
                self.assign(snapshot, Rvalue::Use(Operand::Local(*owner)));
                if let Some(flag) = self.drop_flags.get(owner).copied() {
                    self.assign(flag, Rvalue::Use(Operand::Bool(false)));
                }
                Operand::Local(snapshot)
            }
            ExprKind::Unary(op, e) => {
                let inner = self.expression(e);
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Unary(*op, inner));
                Operand::Local(dst)
            }
            ExprKind::Binary(left, op @ (BinaryOp::And | BinaryOp::Or), right) => {
                let lhs = self.expression(left);
                let rhs_bb = self.new_block();
                let skip_bb = self.new_block();
                let join_bb = self.new_block();
                let result = self.temp(self.types.boolean());
                let (yes, no, skip) = if *op == BinaryOp::And {
                    (rhs_bb, skip_bb, false)
                } else {
                    (skip_bb, rhs_bb, true)
                };
                self.end(Terminator::Branch {
                    condition: lhs,
                    then_bb: yes,
                    else_bb: no,
                });
                self.current = skip_bb;
                self.assign(result, Rvalue::Use(Operand::Bool(skip)));
                self.end(Terminator::Goto(join_bb));
                self.current = rhs_bb;
                let rhs = self.expression(right);
                self.assign(result, Rvalue::Use(rhs));
                self.end(Terminator::Goto(join_bb));
                self.current = join_bb;
                Operand::Local(result)
            }
            ExprKind::Binary(left, op, right) => {
                let lhs = self.expression(left);
                let rhs = self.expression(right);
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Binary(*op, lhs, rhs));
                Operand::Local(dst)
            }
            ExprKind::FunctionRef(id) => {
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::FnRef(*id, e.ty));
                Operand::Local(dst)
            }
            ExprKind::CallIndirect(callee, args) => {
                let callee = self.expression(callee);
                let args = args.iter().map(|a| self.expression(a)).collect();
                if e.ty == self.types.unit() {
                    self.function.blocks[self.current]
                        .instructions
                        .push(Instruction::Eval(Rvalue::CallIndirect(callee, args)));
                    Operand::Unit
                } else {
                    let dst = self.temp(e.ty);
                    self.assign(dst, Rvalue::CallIndirect(callee, args));
                    Operand::Local(dst)
                }
            }
            ExprKind::Call(id, args) => {
                let args = args.iter().map(|a| self.expression(a)).collect();
                if e.ty == self.types.unit() {
                    self.function.blocks[self.current]
                        .instructions
                        .push(Instruction::Eval(Rvalue::Call(*id, args)));
                    Operand::Unit
                } else {
                    let dst = self.temp(e.ty);
                    self.assign(dst, Rvalue::Call(*id, args));
                    Operand::Local(dst)
                }
            }
            ExprKind::StructInit(id, fields) => {
                let values = fields.iter().map(|a| self.expression(a)).collect();
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::StructInit(*id, values));
                Operand::Local(dst)
            }
            ExprKind::Field(value, index) => {
                let base = self.expression(value);
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Field(base, *index));
                Operand::Local(dst)
            }
            ExprKind::Array(items) => {
                let values = items.iter().map(|a| self.expression(a)).collect();
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Array(values));
                Operand::Local(dst)
            }
            ExprKind::Index(array, index) => {
                let array = self.expression(array);
                let index = self.expression(index);
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Index(array, index));
                Operand::Local(dst)
            }
            ExprKind::AddressOf { local, .. } => {
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::AddressOf(*local));
                Operand::Local(dst)
            }
            ExprKind::Deref(ptr) => {
                let ptr = self.expression(ptr);
                let dst = self.temp(e.ty);
                self.assign(dst, Rvalue::Deref(ptr));
                Operand::Local(dst)
            }
        }
    }
    fn unwind(&mut self, target_depth: usize, returning: Option<LocalId>) {
        // Do not pop here: other CFG branches reuse the lexical compile-time
        // environment, while each emitted basic block keeps its own cleanup IR.
        let actions: Vec<Cleanup> = self.cleanups[target_depth..]
            .iter()
            .rev()
            .flat_map(|frame| frame.iter().rev().cloned())
            .collect();
        for action in actions {
            if action.owner.is_some() && action.owner == returning {
                continue;
            }
            if let Some(owner) = action.owner {
                if let Some(flag) = self.drop_flags.get(&owner).copied() {
                    // Destroy only live owners on this CFG path. A moved value
                    // keeps its payload bytes but never invokes Drop twice.
                    let drop_block = self.new_block();
                    let next_block = self.new_block();
                    self.end(Terminator::Branch {
                        condition: Operand::Local(flag),
                        then_bb: drop_block,
                        else_bb: next_block,
                    });
                    self.current = drop_block;
                    self.assign(flag, Rvalue::Use(Operand::Bool(false)));
                    self.expression(&action.expr);
                    self.end(Terminator::Goto(next_block));
                    self.current = next_block;
                    continue;
                }
            }
            self.expression(&action.expr);
        }
    }
    fn statements(&mut self, body: &[Stmt]) {
        self.cleanups.push(Vec::new());
        for stmt in body {
            if self.is_terminated() {
                break;
            }
            match stmt {
                Stmt::Let { local, init } | Stmt::Set { local, value: init } => {
                    let value = self.expression(init);
                    self.assign(*local, Rvalue::Use(value));
                    if let Some(flag) = self.drop_flags.get(local).copied() {
                        self.assign(flag, Rvalue::Use(Operand::Bool(true)));
                    }
                }
                Stmt::FieldSet {
                    root,
                    pointer,
                    index,
                    value,
                } => {
                    let value = self.expression(value);
                    self.function.blocks[self.current]
                        .instructions
                        .push(Instruction::StoreField {
                            root: *root,
                            pointer: *pointer,
                            index: *index,
                            value,
                        });
                }
                Stmt::DerefSet { local, value } => {
                    let value = self.expression(value);
                    self.function.blocks[self.current]
                        .instructions
                        .push(Instruction::StoreDeref {
                            pointer: *local,
                            value,
                        });
                }
                Stmt::Defer { expr, owner } => {
                    if let Some(owner) = owner {
                        if !self.drop_flags.contains_key(owner) {
                            let flag = self.temp(self.types.boolean());
                            self.assign(flag, Rvalue::Use(Operand::Bool(true)));
                            self.drop_flags.insert(*owner, flag);
                        }
                    }
                    self.cleanups.last_mut().unwrap().push(Cleanup {
                        expr: expr.clone(),
                        owner: *owner,
                    });
                }
                Stmt::Return(value) => {
                    // Evaluate return value before any cleanup. Snapshot it so
                    // deferred mutations cannot change the returned scalar.
                    let returned = value.as_ref().map(|v| self.expression(v));
                    let returned_owner = None;
                    let returned = returned.map(|v| {
                        let ty = operand_ty(&v, &self.function, &self.types)
                            .expect("HIR typechecked return");
                        let snapshot = self.temp(ty);
                        self.assign(snapshot, Rvalue::Use(v));
                        Operand::Local(snapshot)
                    });
                    self.unwind(0, returned_owner);
                    self.end(Terminator::Return(returned));
                }
                Stmt::Expr(e) => {
                    self.expression(e);
                }
                Stmt::Unsafe(body) => {
                    self.statements(body);
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let cond = self.expression(condition);
                    let yes = self.new_block();
                    let no = self.new_block();
                    let join = self.new_block();
                    self.end(Terminator::Branch {
                        condition: cond,
                        then_bb: yes,
                        else_bb: no,
                    });
                    self.current = yes;
                    self.statements(then_body);
                    if !self.is_terminated() {
                        self.end(Terminator::Goto(join));
                    }
                    self.current = no;
                    if let Some(body) = else_body {
                        self.statements(body);
                    }
                    if !self.is_terminated() {
                        self.end(Terminator::Goto(join));
                    }
                    self.current = join;
                }
                Stmt::Break => {
                    let (_, after, depth) = *self.loops.last().expect("HIR validates break");
                    self.unwind(depth, None);
                    self.end(Terminator::Goto(after));
                }
                Stmt::Continue => {
                    let (next, _, depth) = *self.loops.last().expect("HIR validates continue");
                    self.unwind(depth, None);
                    self.end(Terminator::Goto(next));
                }
                Stmt::Loop(body) => {
                    let head = self.new_block();
                    let after = self.new_block();
                    self.end(Terminator::Goto(head));
                    self.current = head;
                    self.loops.push((head, after, self.cleanups.len()));
                    self.statements(body);
                    self.loops.pop();
                    if !self.is_terminated() {
                        self.end(Terminator::Goto(head));
                    }
                    self.current = after;
                }
                Stmt::For {
                    local,
                    start,
                    end,
                    body,
                } => {
                    let start_val = self.expression(start);
                    self.assign(*local, Rvalue::Use(start_val));
                    let end_val = self.expression(end);
                    let bound = self.temp(end.ty);
                    self.assign(bound, Rvalue::Use(end_val));
                    let check = self.new_block();
                    let loop_body = self.new_block();
                    let step = self.new_block();
                    let after = self.new_block();
                    self.end(Terminator::Goto(check));
                    self.current = check;
                    let cmp = self.temp(self.types.boolean());
                    self.assign(
                        cmp,
                        Rvalue::Binary(BinaryOp::Lt, Operand::Local(*local), Operand::Local(bound)),
                    );
                    self.end(Terminator::Branch {
                        condition: Operand::Local(cmp),
                        then_bb: loop_body,
                        else_bb: after,
                    });
                    self.current = loop_body;
                    self.loops.push((step, after, self.cleanups.len()));
                    self.statements(body);
                    self.loops.pop();
                    if !self.is_terminated() {
                        self.end(Terminator::Goto(step));
                    }
                    self.current = step;
                    self.assign(
                        *local,
                        Rvalue::Binary(
                            BinaryOp::Add,
                            Operand::Local(*local),
                            Operand::Int(1, start.ty),
                        ),
                    );
                    self.end(Terminator::Goto(check));
                    self.current = after;
                }
                Stmt::While { condition, body } => {
                    let check = self.new_block();
                    let loop_body = self.new_block();
                    let after = self.new_block();
                    self.end(Terminator::Goto(check));
                    self.current = check;
                    let cond = self.expression(condition);
                    self.end(Terminator::Branch {
                        condition: cond,
                        then_bb: loop_body,
                        else_bb: after,
                    });
                    self.current = loop_body;
                    self.loops.push((check, after, self.cleanups.len()));
                    self.statements(body);
                    self.loops.pop();
                    if !self.is_terminated() {
                        self.end(Terminator::Goto(check));
                    }
                    self.current = after;
                }
            }
        }
        if !self.is_terminated() {
            let depth = self.cleanups.len() - 1;
            self.unwind(depth, None);
        }
        self.cleanups.pop();
    }
}
pub fn operand_ty(op: &Operand, f: &Function, types: &Types) -> Result<TypeId, String> {
    match op {
        Operand::Local(id) => f
            .locals
            .get(*id)
            .copied()
            .ok_or_else(|| format!("unknown local {id}")),
        Operand::Int(_, ty) | Operand::Float(_, ty) => Ok(*ty),
        Operand::Bool(_) => Ok(types.boolean()),
        Operand::Str(_) => Ok(types.string()),
        Operand::Unit => Ok(types.unit()),
        Operand::Zero(ty) => Ok(*ty),
    }
}
pub fn rvalue_ty(rv: &Rvalue, f: &Function, p: &Program) -> Result<TypeId, String> {
    let types = &p.types;
    match rv {
        Rvalue::Use(v) => operand_ty(v, f, types),
        Rvalue::Cast(_, to) => Ok(*to),
        Rvalue::Unary(_, v) => operand_ty(v, f, types),
        Rvalue::Binary(op, l, _) => {
            if matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::And
                    | BinaryOp::Or
            ) {
                Ok(types.boolean())
            } else {
                operand_ty(l, f, types)
            }
        }
        Rvalue::Call(id, _) => p
            .functions
            .get(*id)
            .map(|f| f.result)
            .ok_or_else(|| format!("unknown function {id}")),
        Rvalue::FnRef(_, ty) => Ok(*ty),
        Rvalue::CallIndirect(fnptr, _) => match types.kind(operand_ty(fnptr, f, types)?) {
            TypeKind::Function { result, .. } => Ok(result),
            _ => Err("indirect call of nonfunction".into()),
        },
        Rvalue::StructInit(id, _) => types
            .named(&types.structs.get(*id).ok_or("unknown struct")?.name)
            .ok_or("unregistered struct".into()),
        Rvalue::Field(base, index) => {
            let b = operand_ty(base, f, types)?;
            match types.kind(b) {
                TypeKind::Struct(id) => types.structs[id]
                    .fields
                    .get(*index)
                    .map(|f| f.ty)
                    .ok_or("unknown field".into()),
                _ => Err("field base is not struct".into()),
            }
        }
        Rvalue::Array(values) => {
            if values.is_empty() {
                return Err("empty arrays unsupported".into());
            }
            let elem = operand_ty(&values[0], f, types)?;
            // Type is checked using the destination's declared array type below.
            let _ = elem;
            Err("array rvalue requires destination context".into())
        }
        Rvalue::Index(base, _) => match types.kind(operand_ty(base, f, types)?) {
            TypeKind::Array { element, .. } => Ok(element),
            _ => Err("indexing a non-array".into()),
        },
        Rvalue::AddressOf(_) => Err("address-of requires destination pointer context".into()),
        Rvalue::Deref(ptr) => match types.kind(operand_ty(ptr, f, types)?) {
            TypeKind::Pointer { element, .. } | TypeKind::Reference { element, .. } => Ok(element),
            _ => Err("dereferencing non-pointer".into()),
        },
    }
}
fn check_operand(
    op: &Operand,
    f: &Function,
    types: &Types,
    defined: &HashSet<LocalId>,
) -> Result<(), String> {
    if let Operand::Local(id) = op {
        if *id >= f.locals.len() {
            return Err(format!("use of invalid local {id}"));
        }
        if !defined.contains(id) {
            return Err(format!("use of uninitialized local {id} in '{}'", f.name));
        }
    }
    let _ = operand_ty(op, f, types)?;
    Ok(())
}
fn read_operands(rv: &Rvalue) -> Vec<&Operand> {
    match rv {
        Rvalue::Use(a)
        | Rvalue::Unary(_, a)
        | Rvalue::Cast(a, _)
        | Rvalue::Field(a, _)
        | Rvalue::Deref(a) => vec![a],
        Rvalue::Index(a, b) => vec![a, b],
        Rvalue::Binary(_, a, b) => vec![a, b],
        Rvalue::Call(_, values) | Rvalue::StructInit(_, values) | Rvalue::Array(values) => {
            values.iter().collect()
        }
        Rvalue::CallIndirect(ptr, args) => std::iter::once(ptr).chain(args.iter()).collect(),
        Rvalue::AddressOf(_) | Rvalue::FnRef(..) => vec![],
    }
}
fn successors(bb: &BasicBlock) -> Vec<BlockId> {
    match bb.terminator.as_ref() {
        Some(Terminator::Goto(id)) => vec![*id],
        Some(Terminator::Branch {
            then_bb, else_bb, ..
        }) => vec![*then_bb, *else_bb],
        _ => vec![],
    }
}
// Verify operand contracts before LLVM emits instructions. Type equality of an
// assignment's destination alone is not enough: LLVM requires both operands
// of arithmetic/comparison instructions to agree, and calls to respect their
// callee ABI. These checks also catch regressions in surface monomorphization.
fn verify_rvalue_contract(rv: &Rvalue, f: &Function, program: &Program) -> Result<(), String> {
    let types = &program.types;
    match rv {
        Rvalue::Binary(op, l, r) => {
            let lt = operand_ty(l, f, types)?;
            let rt = operand_ty(r, f, types)?;
            if lt != rt {
                return Err(format!(
                    "'{}': mismatched binary operands {} and {}",
                    f.name,
                    types.display(lt),
                    types.display(rt)
                ));
            }
            let numeric = types.numeric(lt);
            let integer = types.integer(lt).is_some();
            let boolean = lt == types.boolean();
            let valid = match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                    numeric
                }
                BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => numeric,
                BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::Shl
                | BinaryOp::Shr => integer,
                BinaryOp::And | BinaryOp::Or => boolean,
                BinaryOp::Eq | BinaryOp::Ne => {
                    numeric || boolean || matches!(types.kind(lt), TypeKind::Pointer { .. })
                }
            };
            if !valid {
                return Err(format!(
                    "'{}': operator {op:?} is invalid for {}",
                    f.name,
                    types.display(lt)
                ));
            }
        }
        Rvalue::FnRef(id, ty) => {
            let callee = program
                .functions
                .get(*id)
                .ok_or("invalid function reference")?;
            if callee.c_variadic {
                return Err("function reference to variadic callee".into());
            }
            let wanted = TypeKind::Function {
                params: callee.params.iter().map(|i| callee.locals[*i]).collect(),
                result: callee.result,
            };
            if types.kind(*ty) != wanted {
                return Err("function reference ABI signature mismatch".into());
            }
        }
        Rvalue::CallIndirect(ptr, args) => {
            let TypeKind::Function { params, result: _ } = types.kind(operand_ty(ptr, f, types)?)
            else {
                return Err("invalid indirect call callee".into());
            };
            if params.len() != args.len() {
                return Err("indirect call arity mismatch".into());
            }
            for (got, expected) in args.iter().zip(params) {
                if operand_ty(got, f, types)? != expected {
                    return Err("indirect call argument ABI mismatch".into());
                }
            }
        }
        Rvalue::Call(id, args) => {
            let callee = program
                .functions
                .get(*id)
                .ok_or_else(|| format!("'{}': invalid callee {id}", f.name))?;
            let needed = callee.params.len();
            if args.len() < needed || (!callee.c_variadic && args.len() != needed) {
                return Err(format!(
                    "'{}': call to '{}' expects {}{} args, found {}",
                    f.name,
                    callee.name,
                    needed,
                    if callee.c_variadic { "+" } else { "" },
                    args.len()
                ));
            }
            for (pos, (actual, expected)) in args.iter().zip(callee.params.iter()).enumerate() {
                let got = operand_ty(actual, f, types)?;
                let param_ty = callee
                    .locals
                    .get(*expected)
                    .ok_or_else(|| format!("'{}': invalid parameter slot", callee.name))?;
                if got != *param_ty {
                    return Err(format!(
                        "'{}': call to '{}' arg {} expected {}, found {}",
                        f.name,
                        callee.name,
                        pos + 1,
                        types.display(*param_ty),
                        types.display(got)
                    ));
                }
            }
        }
        Rvalue::StructInit(id, values) => {
            let info = types
                .structs
                .get(*id)
                .ok_or_else(|| format!("'{}': invalid struct {id}", f.name))?;
            if info.fields.len() != values.len() {
                return Err(format!(
                    "'{}': struct '{}' initializer has {} fields; expected {}",
                    f.name,
                    info.name,
                    values.len(),
                    info.fields.len()
                ));
            }
            for (field, value) in info.fields.iter().zip(values) {
                let got = operand_ty(value, f, types)?;
                if got != field.ty {
                    return Err(format!(
                        "'{}': field '{}.{}' expected {}, found {}",
                        f.name,
                        info.name,
                        field.name,
                        types.display(field.ty),
                        types.display(got)
                    ));
                }
            }
        }
        Rvalue::Index(_, index) => {
            let ty = operand_ty(index, f, types)?;
            if ty != types.i32() {
                return Err(format!("'{}': array index must be i32", f.name));
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn verify(program: &Program) -> Result<(), String> {
    for f in &program.functions {
        if !matches!(f.kind, FunctionKind::Defined) {
            continue;
        }
        if f.blocks.is_empty() {
            return Err(format!("'{}' has no basic blocks", f.name));
        }
        let n = f.blocks.len();
        for (i, bb) in f.blocks.iter().enumerate() {
            if bb.terminator.is_none() {
                return Err(format!("'{}' bb{i} has no terminator", f.name));
            }
            for next in successors(bb) {
                if next >= n {
                    return Err(format!("'{}' bb{i} jumps outside function", f.name));
                }
            }
            for ins in &bb.instructions {
                if let Instruction::StoreField {
                    root,
                    pointer,
                    index,
                    value,
                } = ins
                {
                    let base = *f
                        .locals
                        .get(*root)
                        .ok_or_else(|| format!("'{}': invalid field base", f.name))?;
                    let struct_ty = if *pointer {
                        match program.types.kind(base) {
                            TypeKind::Pointer {
                                element,
                                mutable: true,
                            }
                            | TypeKind::Reference {
                                element,
                                mutable: true,
                            } => element,
                            _ => return Err("field store requires mutable pointer".into()),
                        }
                    } else {
                        base
                    };
                    let sid = match program.types.kind(struct_ty) {
                        TypeKind::Struct(sid) => sid,
                        _ => return Err("field store requires struct".into()),
                    };
                    let field = program.types.structs[sid]
                        .fields
                        .get(*index)
                        .ok_or("field store index out of bounds")?;
                    if operand_ty(value, f, &program.types)? != field.ty {
                        return Err(format!("'{}': field store type mismatch", f.name));
                    }
                }
                if let Instruction::StoreDeref { pointer, value } = ins {
                    let pointer_type = *f.locals.get(*pointer).ok_or("invalid pointer local")?;
                    let pointed_type = match program.types.kind(pointer_type) {
                        TypeKind::Pointer {
                            element,
                            mutable: true,
                        }
                        | TypeKind::Reference {
                            element,
                            mutable: true,
                        } => element,
                        _ => return Err("store through immutable or invalid pointer".into()),
                    };
                    if operand_ty(value, f, &program.types)? != pointed_type {
                        return Err("indirect store type mismatch".into());
                    }
                }
                if let Instruction::Assign { dst, value } = ins {
                    if *dst >= f.locals.len() {
                        return Err(format!("'{}' bb{i} assigns invalid local {dst}", f.name));
                    }
                    match value {
                        Rvalue::Cast(from, to) => {
                            let source = operand_ty(from, f, &program.types)?;
                            if *to != f.locals[*dst] {
                                return Err("cast destination type mismatch".into());
                            }
                            if !program.types.can_numeric_cast(source, *to) {
                                return Err(format!("invalid numeric cast in '{}'", f.name));
                            }
                        }
                        Rvalue::AddressOf(local) => {
                            if *local >= f.locals.len() {
                                return Err(format!("'{}' bad address local", f.name));
                            }
                            if !matches!(program.types.kind(f.locals[*dst]),TypeKind::Pointer{element,..}|TypeKind::Reference{element,..} if element==f.locals[*local])
                            {
                                return Err("invalid address-of destination type".into());
                            }
                        }
                        Rvalue::Array(v) => match program.types.kind(f.locals[*dst]) {
                            TypeKind::Array { len, element } if len == v.len() => {
                                for item in v {
                                    if operand_ty(item, f, &program.types)? != element {
                                        return Err("array element type mismatch".into());
                                    }
                                }
                            }
                            _ => return Err("array destination length/type mismatch".into()),
                        },
                        _ => {
                            let ty = rvalue_ty(value, f, program)?;
                            if ty != f.locals[*dst] {
                                return Err(format!(
                                    "'{}' bb{i}: assignment type mismatch",
                                    f.name
                                ));
                            }
                        }
                    }
                }
            }
        }
        let mut reachable = HashSet::new();
        let mut q = VecDeque::from([0]);
        while let Some(b) = q.pop_front() {
            if reachable.insert(b) {
                q.extend(successors(&f.blocks[b]));
            }
        }
        let universe = (0..f.locals.len()).collect::<HashSet<_>>();
        let initial = f.params.iter().copied().collect::<HashSet<_>>();
        let mut out = vec![universe.clone(); n];
        let mut incoming = vec![HashSet::new(); n];
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..n {
                if !reachable.contains(&i) {
                    continue;
                }
                let ins = if i == 0 {
                    initial.clone()
                } else {
                    let preds = (0..n)
                        .filter(|p| reachable.contains(p) && successors(&f.blocks[*p]).contains(&i))
                        .collect::<Vec<_>>();
                    if let Some((first, rest)) = preds.split_first() {
                        let mut value = out[*first].clone();
                        for p in rest {
                            value.retain(|id| out[*p].contains(id));
                        }
                        value
                    } else {
                        HashSet::new()
                    }
                };
                let mut new_out = ins.clone();
                for instruction in &f.blocks[i].instructions {
                    if let Instruction::Assign { dst, .. } = instruction {
                        new_out.insert(*dst);
                    }
                }
                if incoming[i] != ins || out[i] != new_out {
                    incoming[i] = ins;
                    out[i] = new_out;
                    changed = true;
                }
            }
        }
        for i in 0..n {
            if !reachable.contains(&i) {
                continue;
            }
            let bb = &f.blocks[i];
            let mut initialized = incoming[i].clone();
            for instruction in &bb.instructions {
                match instruction {
                    Instruction::StoreField { root, value, .. } => {
                        if !initialized.contains(root) {
                            return Err(format!("field base local {root} is not initialized"));
                        }
                        check_operand(value, f, &program.types, &initialized)?;
                    }
                    Instruction::StoreDeref { pointer, value } => {
                        if !initialized.contains(pointer) {
                            return Err(format!("pointer local {pointer} is not initialized"));
                        }
                        check_operand(value, f, &program.types, &initialized)?;
                    }
                    Instruction::Assign { dst, value } => {
                        verify_rvalue_contract(value, f, program)?;
                        for op in read_operands(value) {
                            check_operand(op, f, &program.types, &initialized)?;
                        }
                        if let Rvalue::Index(_, idx) = value {
                            check_operand(idx, f, &program.types, &initialized)?;
                        }
                        if let Rvalue::AddressOf(local) = value {
                            if !initialized.contains(local) {
                                return Err("address of uninitialized local".into());
                            }
                        }
                        initialized.insert(*dst);
                    }
                    Instruction::Eval(value) => {
                        verify_rvalue_contract(value, f, program)?;
                        for op in read_operands(value) {
                            check_operand(op, f, &program.types, &initialized)?;
                        }
                    }
                }
            }
            match bb.terminator.as_ref().unwrap() {
                Terminator::Return(value) => {
                    if let Some(v) = value {
                        check_operand(v, f, &program.types, &initialized)?;
                        if operand_ty(v, f, &program.types)? != f.result {
                            return Err("return type mismatch".into());
                        }
                    } else if f.result != program.types.unit() {
                        return Err("non-unit return has no value".into());
                    }
                }
                Terminator::Branch { condition, .. } => {
                    check_operand(condition, f, &program.types, &initialized)?;
                    if operand_ty(condition, f, &program.types)? != program.types.boolean() {
                        return Err("branch condition must be bool".into());
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}
pub fn lower(hir: &hir::Program) -> Result<Program, String> {
    let mut functions = Vec::new();
    for f in &hir.functions {
        let mut b = Builder::new(f, &hir.types);
        if matches!(f.kind, FunctionKind::Defined) {
            b.statements(&f.body);
            for block in &mut b.function.blocks {
                if block.terminator.is_none() {
                    block.terminator = Some(Terminator::Unreachable);
                }
            }
        } else {
            b.function.blocks.clear();
        }
        functions.push(b.function);
    }
    let program = Program {
        types: hir.types.clone(),
        functions,
    };
    verify(&program)?;
    Ok(program)
}
