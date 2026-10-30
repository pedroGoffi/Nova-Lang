//! Typed high-level IR: resolved function/local/field IDs and well-typed expressions.
use crate::{
    ast::{self, BinaryOp, UnaryOp},
    types::{Primitive, StructId, TypeId, TypeKind, Types},
};
use std::collections::{HashMap, HashSet};

pub type FunctionId = usize;
pub type LocalId = usize;
#[derive(Debug, Clone)]
pub struct Program {
    pub types: Types,
    pub functions: Vec<Function>,
}
#[derive(Debug, Clone, Copy)]
pub enum FunctionKind {
    Defined,
    External,
    JsonWriter(StructId),
    JsonReader(StructId),
    JsonToString(StructId),
}
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub result: TypeId,
    pub body: Vec<Stmt>,
    pub kind: FunctionKind,
    pub c_variadic: bool,
}
#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: TypeId,
    pub mutable: bool,
}
#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        local: LocalId,
        init: Expr,
    },
    Defer {
        expr: Expr,
        owner: Option<LocalId>,
    },
    Set {
        local: LocalId,
        value: Expr,
    },
    DerefSet {
        local: LocalId,
        value: Expr,
    },
    FieldSet {
        root: LocalId,
        pointer: bool,
        index: usize,
        value: Expr,
    },
    Return(Option<Expr>),
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },
    Loop(Vec<Stmt>),
    For {
        local: LocalId,
        start: Expr,
        end: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Unsafe(Vec<Stmt>),
    Expr(Expr),
}
#[derive(Debug, Clone)]
pub struct Expr {
    pub ty: TypeId,
    pub kind: ExprKind,
}
/// An addressable location, not a value: projections never move the owner.
/// An index is evaluated once when the borrow is established.
#[derive(Debug, Clone)]
pub struct Place {
    pub root: LocalId,
    pub projections: Vec<PlaceProjection>,
}
#[derive(Debug, Clone)]
pub enum PlaceProjection {
    Field(usize),
    Index(Expr),
}
#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    Unit,
    Zero,
    /// Branches assign the same synthetic local without ever moving fields.
    If { condition: Box<Expr>, then_body: Vec<Stmt>, else_body: Vec<Stmt>, result: Option<LocalId> },
    /// A scoped expression; its final value is moved to an outer result local.
    Block { body: Vec<Stmt>, result: Option<LocalId> },
    Local(LocalId),
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinaryOp, Box<Expr>),
    Call(FunctionId, Vec<Expr>),
    FunctionRef(FunctionId),
    CallIndirect(Box<Expr>, Vec<Expr>),
    StructInit(StructId, Vec<Expr>),
    Field(Box<Expr>, usize),
    /// Compiler-generated enum match: consume its sole live owning payload.
    /// Unlike ordinary field projections, this transfers the whole owner and
    /// invalidates the enum temporary's Drop flag.
    TakeMatchField(Box<Expr>, usize),
    Array(Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    /// Compiler-generated cleanup for a fixed array of owned structs.
    /// Consumes each element exactly once when its root is still live.
    DropArray { root: LocalId, element: TypeId, len: usize, drop_fn: FunctionId },
    AddressOf {
        place: Place,
        mutable: bool,
    },
    Deref(Box<Expr>),
    /// Consumes an owned local. MIR clears its drop flag after producing the value.
    MoveLocal(LocalId),
    Cast(Box<Expr>),
}
#[derive(Clone)]
struct Signature {
    id: FunctionId,
    params: Vec<TypeId>,
    result: TypeId,
    c_variadic: bool,
    is_unsafe: bool,
}
struct Resolver<'a> {
    signatures: &'a HashMap<String, Signature>,
    types: &'a mut Types,
    scopes: Vec<HashMap<String, LocalId>>,
    locals: Vec<Local>,
    result: TypeId,
    unsafe_depth: usize,
    loop_depth: usize,
}
impl<'a> Resolver<'a> {
    fn new(
        signatures: &'a HashMap<String, Signature>,
        types: &'a mut Types,
        result: TypeId,
    ) -> Self {
        Self {
            signatures,
            types,
            scopes: vec![HashMap::new()],
            locals: Vec::new(),
            result,
            unsafe_depth: 0,
            loop_depth: 0,
        }
    }
    fn find_local(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }
    fn declare(&mut self, name: &str, ty: TypeId, mutable: bool) -> Result<LocalId, String> {
        if self.scopes.last().unwrap().contains_key(name) {
            return Err(format!("duplicate local '{name}' in the same scope"));
        }
        if ty == self.types.unit() {
            return Err(format!("local '{name}' cannot have unit type"));
        }
        let id = self.locals.len();
        self.locals.push(Local {
            name: name.into(),
            ty,
            mutable,
        });
        self.scopes.last_mut().unwrap().insert(name.into(), id);
        Ok(id)
    }
    fn scoped(&mut self, body: &[ast::Stmt], unsafe_scope: bool) -> Result<Vec<Stmt>, String> {
        self.scopes.push(HashMap::new());
        if unsafe_scope {
            self.unsafe_depth += 1;
        }
        let resolved = self.block(body);
        if unsafe_scope {
            self.unsafe_depth -= 1;
        }
        self.scopes.pop();
        resolved
    }
    fn loop_scoped(&mut self, body: &[ast::Stmt]) -> Result<Vec<Stmt>, String> {
        self.loop_depth += 1;
        let result = self.scoped(body, false);
        self.loop_depth -= 1;
        result
    }
    /// Resolve an lvalue without evaluating its owning aggregate by value.
    /// At this stage, projections are rooted in a named local. Reborrowing
    /// through *references* is intentionally a separate lifetime feature.
    fn place(&mut self, value: &ast::Expr) -> Result<(Place, TypeId), String> {
        use ast::Expr as A;
        match value {
            A::Var(name) => {
                let root = self.find_local(name)
                    .ok_or_else(|| format!("unknown local '{name}'"))?;
                Ok((Place { root, projections: Vec::new() }, self.locals[root].ty))
            }
            A::Field(parent, name) => {
                let (mut place, parent_ty) = self.place(parent)?;
                let TypeKind::Struct(sid) = self.types.kind(parent_ty) else {
                    return Err(format!("cannot borrow field '{name}' of non-struct"));
                };
                let (index, field) = self.types.structs[sid].fields.iter().enumerate()
                    .find(|(_, field)| field.name == *name)
                    .ok_or_else(|| format!("unknown field '{name}'"))?;
                let ty = field.ty;
                place.projections.push(PlaceProjection::Field(index));
                Ok((place, ty))
            }
            A::Index(parent, index) => {
                let (mut place, parent_ty) = self.place(parent)?;
                let TypeKind::Array { element, len } = self.types.kind(parent_ty) else {
                    return Err("indexed borrow requires a fixed-size array".into());
                };
                let i32_ty = self.types.i32();
                let index = self.expect(index, i32_ty)?;
                // Avoid emitting an unreachable GEP for an obvious bad literal.
                if let ExprKind::Int(n) = &index.kind {
                    if *n < 0 || *n >= len as i128 {
                        return Err(format!("array borrow index {n} out of bounds (length {len})"));
                    }
                }
                place.projections.push(PlaceProjection::Index(index));
                Ok((place, element))
            }
            _ => Err("borrow requires an addressable local, field or indexed array element".into()),
        }
    }
    /// Lower a value block in a fresh lexical scope. The last expression is
    /// deliberately lowered *after* preceding lets so it sees their bindings.
    fn value_branch(
        &mut self, body: &[ast::Stmt], expected: Option<TypeId>, required: bool,
    ) -> Result<(Vec<Stmt>, Expr), String> {
        self.scopes.push(HashMap::new());
        let output = (|| {
            let (prefix, last) = match body.split_last() {
                Some((ast::Stmt::Tail(value), prefix)) => (prefix, Some(value)),
                _ => (body, None),
            };
            let statements = self.block(prefix)?;
            let tail = match last {
                Some(expr) => self.expr(expr, expected)?,
                None if !required => Expr { ty:self.types.unit(), kind:ExprKind::Unit },
                None => return Err("if expression branch must yield a value".into()),
            };
            Ok((statements, tail))
        })();
        self.scopes.pop();
        output
    }
    fn fresh_expression_result(&mut self, ty: TypeId) -> Result<LocalId, String> {
        if matches!(self.types.kind(ty), TypeKind::Reference { .. }) {
            return Err("returning a borrow from a value block requires lifetime analysis".into());
        }
        let name = format!("__nova_expression_result_{}", self.locals.len());
        self.declare(&name, ty, true)
    }
    fn expect(&mut self, e: &ast::Expr, expected: TypeId) -> Result<Expr, String> {
        self.expr(e, Some(expected))
    }
    fn expr(&mut self, e: &ast::Expr, expected: Option<TypeId>) -> Result<Expr, String> {
        use ast::Expr as A;
        let (ty, kind) = match e {
            A::Yield(..) | A::Await(..) | A::Spread(..) => {
                return Err("compiler bug: suspension syntax reached typed HIR".into())
            }
            A::Lambda { .. } => return Err("compiler bug: lambda was not lifted".into()),
            A::MethodCall(..) | A::GenericCall(..) | A::GenericStructInit(..) => {
                return Err("compiler bug: surface expression reached typed HIR".into())
            }
            A::If { condition, then_body, else_body } => {
                let bool_ty = self.types.boolean();
                let condition = self.expect(condition, bool_ty)?;
                let (mut then_stmts, then_value) = self.value_branch(then_body, expected, true)?;
                let (mut else_stmts, else_value) = self.value_branch(else_body, Some(then_value.ty), true)?;
                if then_value.ty != else_value.ty {
                    return Err(format!("if branches disagree: expected {}, got {}",
                        self.types.display(then_value.ty), self.types.display(else_value.ty)));
                }
                let ty = then_value.ty;
                let result = if ty == self.types.unit() { None } else {
                    let local = self.fresh_expression_result(ty)?;
                    then_stmts.push(Stmt::Set { local, value: then_value });
                    else_stmts.push(Stmt::Set { local, value: else_value });
                    Some(local)
                };
                (ty, ExprKind::If {
                    condition: Box::new(condition), then_body:then_stmts,
                    else_body:else_stmts, result,
                })
            }
            A::Block(body) => {
                let (mut statements, tail) = self.value_branch(body, expected, false)?;
                let ty = tail.ty;
                let result = if ty == self.types.unit() { None } else {
                    let local = self.fresh_expression_result(ty)?;
                    statements.push(Stmt::Set { local, value:tail });
                    Some(local)
                };
                (ty, ExprKind::Block { body:statements, result })
            }
            A::MacroCall(..) => return Err("compiler bug: macro call reached typed HIR".into()),
            A::Cast(inner, target) => {
                let target = self.types.resolve(target)?;
                let inner = self.expr(inner, None)?;
                if matches!(self.types.kind(target), TypeKind::Function { .. })
                    && self.types.kind(inner.ty) == TypeKind::Primitive(Primitive::Unsigned(64))
                    && self.unsafe_depth == 0
                {
                    return Err(
                        "converting an integer into a function pointer requires unsafe".into(),
                    );
                }
                if matches!(self.types.kind(inner.ty), TypeKind::Pointer { .. })
                    && matches!(self.types.kind(target), TypeKind::Pointer { .. })
                    && self.unsafe_depth == 0
                {
                    return Err("casting between distinct pointer types requires unsafe".into());
                }
                if !self.types.can_numeric_cast(inner.ty, target) {
                    return Err(format!(
                        "cannot cast {} to {}",
                        self.types.display(inner.ty),
                        self.types.display(target)
                    ));
                }
                (target, ExprKind::Cast(Box::new(inner)))
            }
            A::FunctionRef(name) => {
                let sig = self
                    .signatures
                    .get(name)
                    .ok_or_else(|| format!("unknown function reference '{name}'"))?;
                if sig.c_variadic {
                    return Err("variadic functions cannot be referenced as values".into());
                }
                if sig.is_unsafe {
                    return Err(format!("unsafe function '{name}' cannot be stored in a safe function pointer; effect-qualified function pointers are not supported yet"));
                }
                (
                    self.types.intern(TypeKind::Function {
                        params: sig.params.clone(),
                        result: sig.result,
                    }),
                    ExprKind::FunctionRef(sig.id),
                )
            }
            A::CallIndirect(callee, values) => {
                let callee = self.expr(callee, None)?;
                let TypeKind::Function { params, result } = self.types.kind(callee.ty) else {
                    return Err("attempt to call a non-function value".into());
                };
                if params.len() != values.len() {
                    return Err(format!(
                        "indirect call expects {} args, got {}",
                        params.len(),
                        values.len()
                    ));
                }
                let args = values
                    .iter()
                    .zip(params)
                    .map(|(e, ty)| self.expect(e, ty))
                    .collect::<Result<Vec<_>, _>>()?;
                (result, ExprKind::CallIndirect(Box::new(callee), args))
            }
            A::Zero(ty) => {
                let ty = self.types.resolve(ty)?;
                (ty, ExprKind::Zero)
            }
            A::Int(raw) => {
                let value: i128 = raw
                    .parse()
                    .map_err(|_| format!("integer literal '{raw}' is too large"))?;
                let ty = if let Some(hint) = expected.filter(|t| self.types.integer(*t).is_some()) {
                    hint
                } else if value <= i32::MAX as i128 {
                    self.types.i32()
                } else if value <= i64::MAX as i128 {
                    self.types.i64()
                } else {
                    return Err(format!(
                        "integer literal {value} exceeds supported default signed range"
                    ));
                };
                let (bits, signed) = self.types.integer(ty).unwrap();
                let max = if signed {
                    (1_i128 << (bits - 1)) - 1
                } else {
                    (1_i128 << bits) - 1
                };
                if value > max {
                    return Err(format!(
                        "integer literal {value} does not fit in {}",
                        self.types.display(ty)
                    ));
                }
                (ty, ExprKind::Int(value))
            }
            A::Float(raw) => {
                let ty = if let Some(hint) = expected.filter(|t| self.types.float(*t)) {
                    hint
                } else {
                    self.types.f64()
                };
                let value: f64 = raw
                    .parse()
                    .map_err(|_| format!("invalid floating point literal '{raw}'"))?;
                if !value.is_finite()
                    || matches!(
                        self.types.kind(ty),
                        TypeKind::Primitive(Primitive::Float(32))
                    ) && !(value as f32).is_finite()
                {
                    return Err(format!("floating literal '{raw}' is not representable"));
                }
                (ty, ExprKind::Float(value))
            }
            A::Bool(b) => (self.types.boolean(), ExprKind::Bool(*b)),
            A::Unit => (self.types.unit(), ExprKind::Unit),
            A::Str(raw) => (self.types.string(), ExprKind::Str(unescape_string(raw)?)),
            A::Var(name) => {
                let id = self
                    .find_local(name)
                    .ok_or_else(|| format!("unknown local '{name}'"))?;
                (self.locals[id].ty, ExprKind::Local(id))
            }
            A::Unary(op, inner) => {
                let hint = if *op == UnaryOp::Not {
                    Some(self.types.boolean())
                } else {
                    expected
                };
                let inner = self.expr(inner, hint)?;
                match op {
                    UnaryOp::Neg if !self.types.numeric(inner.ty) => {
                        return Err("unary '-' requires numeric type".into())
                    }
                    UnaryOp::Not if inner.ty != self.types.boolean() => {
                        return Err("unary '!' requires bool".into())
                    }
                    UnaryOp::BitNot if self.types.integer(inner.ty).is_none() => {
                        return Err("unary '~' requires integer".into())
                    }
                    _ => {}
                }
                (inner.ty, ExprKind::Unary(*op, Box::new(inner)))
            }
            A::Binary(lhs, op, rhs) => {
                let arith = matches!(
                    op,
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem
                );
                let logical = matches!(op, BinaryOp::And | BinaryOp::Or);
                let bitwise = matches!(
                    op,
                    BinaryOp::BitAnd
                        | BinaryOp::BitOr
                        | BinaryOp::BitXor
                        | BinaryOp::Shl
                        | BinaryOp::Shr
                );
                let ordering = matches!(
                    op,
                    BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
                );
                let hint = if logical {
                    Some(self.types.boolean())
                } else if arith || bitwise {
                    expected.filter(|t| self.types.numeric(*t))
                } else {
                    None
                };
                let left = self.expr(lhs, hint)?;
                let right = self.expr(rhs, Some(left.ty))?;
                if left.ty != right.ty {
                    return Err(format!("operator '{op:?}' requires equal types"));
                }
                if arith && !self.types.numeric(left.ty) {
                    return Err(format!("operator '{op:?}' requires numeric operands"));
                }
                if bitwise && self.types.integer(left.ty).is_none() {
                    return Err(format!("operator '{op:?}' requires integer operands"));
                }
                if ordering && !self.types.numeric(left.ty) && left.ty != self.types.character() {
                    return Err("comparison requires numeric operands".into());
                }
                if logical && left.ty != self.types.boolean() {
                    return Err("logical operators require bool".into());
                }
                if !arith
                    && !bitwise
                    && !logical
                    && !ordering
                    && matches!(
                        self.types.kind(left.ty),
                        TypeKind::Struct(_)
                            | TypeKind::Array { .. }
                            | TypeKind::Primitive(Primitive::Str)
                    )
                {
                    // Lowering must not silently compare aggregates (or str
                    // pointers) by their storage representation. Report the
                    // *source expression* so an unsupported comparison can be
                    // distinguished from an incorrect type inferred upstream.
                    return Err(format!(
                        "operator '{op:?}' on '{}' requires an explicit equality implementation; left={lhs:?}, right={rhs:?}",
                        self.types.display(left.ty)
                    ));
                }
                let ty = if arith || bitwise {
                    left.ty
                } else {
                    self.types.boolean()
                };
                (ty, ExprKind::Binary(Box::new(left), *op, Box::new(right)))
            }
            A::Call(name, args) => {
                let sig = self
                    .signatures
                    .get(name)
                    .ok_or_else(|| format!("unknown function '{name}'"))?
                    .clone();
                if sig.is_unsafe && self.unsafe_depth == 0 {
                    return Err(format!(
                        "call to unsafe function '{name}' requires an unsafe block"
                    ));
                }
                if (sig.c_variadic && args.len() < sig.params.len())
                    || (!sig.c_variadic && args.len() != sig.params.len())
                {
                    return Err(format!(
                        "function '{name}' expects {}{} arguments, got {}",
                        sig.params.len(),
                        if sig.c_variadic { " or more" } else { "" },
                        args.len()
                    ));
                }
                let mut resolved = Vec::with_capacity(args.len());
                for (arg, param) in args.iter().zip(&sig.params) {
                    resolved.push(self.expect(arg, *param)?);
                }
                for arg in args.iter().skip(sig.params.len()) {
                    let v = self.expr(arg, None)?;
                    if !matches!(
                        self.types.kind(v.ty),
                        TypeKind::Primitive(Primitive::Signed(_)|Primitive::Unsigned(_)|Primitive::Float(_)|Primitive::Bool|Primitive::Str)
                        // A temporary &T / &mut T passed inside `unsafe` is a
                        // native C pointer at the LLVM ABI boundary. Its
                        // borrow remains checked by the ownership pass.
                        | TypeKind::Pointer{..} | TypeKind::Reference{..}
                    ) {
                        return Err(format!(
                            "C vararg for '{name}' must be an ABI scalar or pointer; got {}",
                            self.types.display(v.ty)
                        ));
                    }
                    resolved.push(v);
                }
                (sig.result, ExprKind::Call(sig.id, resolved))
            }
            A::StructInit(name, values) => {
                let id = self
                    .types
                    .named(name)
                    .ok_or_else(|| format!("unknown struct '{name}'"))?;
                let struct_id = match self.types.kind(id) {
                    TypeKind::Struct(s) => s,
                    _ => return Err(format!("'{name}' is not a struct")),
                };
                let fields = self.types.structs[struct_id].fields.clone();
                if values.len() != fields.len() {
                    return Err(format!(
                        "struct '{name}' requires {} fields, got {}",
                        fields.len(),
                        values.len()
                    ));
                }
                let mut seen = HashMap::<&str, &ast::Expr>::new();
                for (name, value) in values {
                    if seen.insert(name, value).is_some() {
                        return Err(format!("duplicate field '{name}' in struct initializer"));
                    }
                }
                let mut resolved = Vec::new();
                for field in fields {
                    let value = seen
                        .get(field.name.as_str())
                        .ok_or_else(|| format!("missing field '{}' in '{name}'", field.name))?;
                    let member = self.expect(value, field.ty)?;
                    resolved.push(member);
                }
                for (key, _) in values {
                    if !self.types.structs[struct_id]
                        .fields
                        .iter()
                        .any(|f| &f.name == key)
                    {
                        return Err(format!("unknown field '{key}' in '{name}'"));
                    }
                }
                (id, ExprKind::StructInit(struct_id, resolved))
            }
            A::Field(value, name) => {
                // Surface's match desugaring creates a private named enum
                // temporary. Only its bindings may transfer a payload owner.
                let match_payload = matches!(value.as_ref(), A::Var(local)
                    if local.starts_with("__nova_match_") && name != "__nova_tag");
                let value = self.expr(value, None)?;
                let sid = match self.types.kind(value.ty) {
                    TypeKind::Struct(s) => s,
                    _ => return Err(format!("cannot access field '{name}' on non-struct")),
                };
                let index = self.types.structs[sid]
                    .fields
                    .iter()
                    .position(|f| f.name == *name)
                    .ok_or_else(|| {
                        format!(
                            "unknown field '{name}' on '{}'",
                            self.types.structs[sid].name
                        )
                    })?;
                let payload_ty = self.types.structs[sid].fields[index].ty;
                if match_payload && field_has_drop(self.types, payload_ty, self.signatures) {
                    // The enum layout stores inactive variants as zeroed
                    // fields. This move is safe only if the active variant
                    // contains ONE owning payload; other fields must be Copy.
                    let variant_tag = name.strip_prefix("__nova_variant_")
                        .and_then(|rest| rest.split_once('_'))
                        .and_then(|(tag, _)| tag.parse::<usize>().ok())
                        .ok_or("invalid compiler-generated enum payload name")?;
                    let owning_in_variant = self.types.structs[sid].fields.iter()
                        .filter(|f| f.name.strip_prefix("__nova_variant_")
                            .and_then(|rest| rest.split_once('_'))
                            .and_then(|(tag, _)| tag.parse::<usize>().ok()) == Some(variant_tag))
                        .filter(|f| field_has_drop(self.types, f.ty, self.signatures)).count();
                    if owning_in_variant != 1 {
                        return Err(format!("matching variant tag {variant_tag} with multiple owning fields is not yet supported"));
                    }
                    (payload_ty, ExprKind::TakeMatchField(Box::new(value), index))
                } else {
                    (payload_ty, ExprKind::Field(Box::new(value), index))
                }
            }
            A::Array(items) => {
                if items.is_empty() {
                    return Err(
                        "empty array needs an explicit element type (not yet supported)".into(),
                    );
                }
                let (element, len) = if let Some(hint) = expected {
                    match self.types.kind(hint) {
                        TypeKind::Array { element, len } => (element, len),
                        _ => return Err("array literal requires array type".into()),
                    }
                } else {
                    let first = self.expr(&items[0], None)?;
                    (first.ty, items.len())
                };
                if len != items.len() {
                    return Err(format!(
                        "array requires {len} elements, got {}",
                        items.len()
                    ));
                }
                let elems = items
                    .iter()
                    .map(|i| self.expect(i, element))
                    .collect::<Result<Vec<_>, _>>()?;
                let ty = self.types.intern(TypeKind::Array { element, len });
                (ty, ExprKind::Array(elems))
            }
            A::Index(value, index) => {
                let value = self.expr(value, None)?;
                let element = match self.types.kind(value.ty) {
                    TypeKind::Array { element, .. } => element,
                    _ => return Err("indexing requires a fixed-size array".into()),
                };
                let i32ty = self.types.i32();
                let index = self.expect(index, i32ty)?;
                (element, ExprKind::Index(Box::new(value), Box::new(index)))
            }
            A::AddressOf { mutable, value } => {
                let (place, element_ty) = self.place(value)?;
                if *mutable && !self.locals[place.root].mutable {
                    return Err(format!("mutable borrow requires 'let mut {}'",
                        self.locals[place.root].name));
                }
                let ty = if let Some(expected) = expected {
                    match self.types.kind(expected) {
                        TypeKind::Pointer { element, mutable: want }
                            if element == element_ty && want == *mutable => {
                            if self.unsafe_depth == 0 {
                                return Err("raw address-of requires unsafe; use a typed borrow &T instead".into());
                            }
                            expected
                        }
                        _ => self.types.intern(TypeKind::Reference {
                            element: element_ty, mutable: *mutable,
                        }),
                    }
                } else {
                    self.types.intern(TypeKind::Reference {
                        element: element_ty, mutable: *mutable,
                    })
                };
                (ty, ExprKind::AddressOf { place, mutable: *mutable })
            }
            A::Deref(inner) => {
                let inner = self.expr(inner, None)?;
                let ty = match self.types.kind(inner.ty) {
                    TypeKind::Pointer { element, .. } => {
                        if self.unsafe_depth == 0 {
                            return Err("raw pointer dereference requires unsafe".into());
                        }
                        element
                    }
                    TypeKind::Reference { element, .. } => element,
                    _ => return Err("cannot dereference non-pointer".into()),
                };
                (ty, ExprKind::Deref(Box::new(inner)))
            }
        };
        if let Some(want) = expected {
            if ty != want {
                return Err(format!(
                    "expected {}, found {}",
                    self.types.display(want),
                    self.types.display(ty)
                ));
            }
        }
        Ok(Expr { ty, kind })
    }
    fn automatic_cleanup(&self, local: LocalId) -> Result<Option<Stmt>, String> {
        let owner_ty = self.locals[local].ty;
        // A fixed array owns every element. Emit one root-guarded cleanup,
        // otherwise marking the root as moved could skip some elements.
        if let TypeKind::Array { element, len } = self.types.kind(owner_ty) {
            if let TypeKind::Struct(sid) = self.types.kind(element) {
                let info = &self.types.structs[sid];
                let method = format!("{}__drop", info.name);
                if let Some(sig) = self.signatures.get(&method) {
                    if sig.params != vec![element] || sig.result != self.types.unit() {
                        return Err(format!("'{}::drop' must consume {} and return ()",info.name,info.name));
                    }
                    return Ok(Some(Stmt::Defer {
                        expr: Expr {
                            ty: self.types.unit(),
                            kind: ExprKind::DropArray { root: local, element, len, drop_fn: sig.id },
                        },
                        owner: Some(local),
                    }));
                }
            }
            return Ok(None);
        }
        let TypeKind::Struct(struct_id) = self.types.kind(owner_ty) else {
            return Ok(None);
        };
        let info = &self.types.structs[struct_id];
        let symbol = format!("{}__drop", info.name);
        let Some(signature) = self.signatures.get(&symbol) else {
            return Ok(None);
        };
        if signature.params != vec![self.locals[local].ty] || signature.result != self.types.unit()
        {
            return Err(format!(
                "'{}::drop' must have signature fn drop(self: {}) -> ()",
                info.name, info.name
            ));
        }
        Ok(Some(Stmt::Defer {
            expr: Expr {
                ty: self.types.unit(),
                kind: ExprKind::Call(
                    signature.id,
                    vec![Expr {
                        ty: self.locals[local].ty,
                        kind: ExprKind::Local(local),
                    }],
                ),
            },
            owner: Some(local),
        }))
    }
    fn block(&mut self, body: &[ast::Stmt]) -> Result<Vec<Stmt>, String> {
        let mut out = Vec::new();
        for s in body {
            if stops_block(&out) {
                return Err("unreachable statement after unconditional transfer".into());
            }
            let stmt = match s {
                ast::Stmt::Match { .. } => return Err("compiler bug: match was not lowered".into()),
                ast::Stmt::ForEach { .. } => {
                    return Err(
                        "compiler bug: for-in iterator was not desugared before typed HIR".into(),
                    )
                }
                ast::Stmt::Let {
                    name,
                    mutable,
                    ty,
                    init,
                    defer,
                } => {
                    let ty = ty.as_ref().map(|t| self.types.resolve(t)).transpose()?;
                    let init = self.expr(init, ty)?;
                    let local = self.declare(name, init.ty, *mutable)?;
                    out.push(Stmt::Let { local, init });
                    // A declaration-level defer replaces the automatic class destructor.
                    if let Some(deferred) = defer {
                        let cleanup = self.expr(deferred, None)?;
                        if cleanup.ty != self.types.unit() {
                            return Err("defer must call a function returning ()".into());
                        }
                        out.push(Stmt::Defer {
                            expr: cleanup,
                            owner: None,
                        });
                    } else if let Some(cleanup) = self.automatic_cleanup(local)? {
                        out.push(cleanup);
                    }
                    continue;
                }
                ast::Stmt::Defer(e) => {
                    let cleanup = self.expr(e, None)?;
                    if cleanup.ty != self.types.unit() {
                        return Err("defer must call a function returning ()".into());
                    }
                    Stmt::Defer {
                        expr: cleanup,
                        owner: None,
                    }
                }
                ast::Stmt::DerefSet { name, value } => {
                    let local = self
                        .find_local(name)
                        .ok_or_else(|| format!("unknown pointer or borrow '{name}'"))?;
                    let target = match self.types.kind(self.locals[local].ty) {
                        TypeKind::Pointer {
                            element,
                            mutable: true,
                        } => {
                            if self.unsafe_depth == 0 {
                                return Err("writing through a raw pointer requires unsafe".into());
                            }
                            element
                        }
                        TypeKind::Reference {
                            element,
                            mutable: true,
                        } => element,
                        _ => {
                            return Err(
                                "dereference assignment needs a mutable pointer or &mut borrow"
                                    .into(),
                            )
                        }
                    };
                    if let TypeKind::Struct(struct_id) = self.types.kind(target) {
                        let name = &self.types.structs[struct_id].name;
                        if self.signatures.contains_key(&format!("{name}__drop")) {
                            return Err("overwriting an owned object through a borrow requires explicit replacement semantics".into());
                        }
                    }
                    Stmt::DerefSet {
                        local,
                        value: self.expect(value, target)?,
                    }
                }
                ast::Stmt::Set { name, value } => {
                    let local = self
                        .find_local(name)
                        .ok_or_else(|| format!("unknown local '{name}'"))?;
                    if !self.locals[local].mutable {
                        return Err(format!(
                            "cannot assign to immutable local '{name}'; use 'let mut'"
                        ));
                    }
                    let ty = self.locals[local].ty;
                    // Ownership pass checks whether the former value was moved;
                    // only then is reinitialization of an owned local legal.
                    Stmt::Set {
                        local,
                        value: self.expect(value, ty)?,
                    }
                }
                ast::Stmt::FieldSet {
                    root,
                    field,
                    op,
                    value,
                } => {
                    let local = self
                        .find_local(root)
                        .ok_or_else(|| format!("unknown local '{root}'"))?;
                    let root_ty = self.locals[local].ty;
                    let (struct_ty, pointer) = match self.types.kind(root_ty) {
                        TypeKind::Struct(_) => {
                            if !self.locals[local].mutable {
                                return Err(format!("cannot assign to field of immutable local '{root}'; declare it with 'let mut'"));
                            }
                            (root_ty, false)
                        }
                        TypeKind::Pointer {
                            element,
                            mutable: true,
                        } => {
                            if self.unsafe_depth == 0 {
                                return Err(format!(
                                    "writing through '*mut' '{root}' requires an unsafe block"
                                ));
                            }
                            (element, true)
                        }
                        TypeKind::Reference {
                            element,
                            mutable: true,
                        } => (element, true),
                        TypeKind::Reference { mutable: false, .. } => {
                            return Err(format!("cannot assign through immutable borrow '{root}'"))
                        }
                        TypeKind::Pointer { mutable: false, .. } => {
                            return Err(format!("cannot write through '*const' '{root}'"))
                        }
                        _ => {
                            return Err(format!(
                                "'{root}' is not a struct or mutable pointer to struct"
                            ))
                        }
                    };
                    let sid = match self.types.kind(struct_ty) {
                        TypeKind::Struct(sid) => sid,
                        _ => return Err(format!("'{root}' does not refer to a struct")),
                    };
                    let info = &self.types.structs[sid];
                    let index = info
                        .fields
                        .iter()
                        .position(|f| f.name == *field)
                        .ok_or_else(|| format!("unknown field '{field}' on '{}'", info.name))?;
                    let field_ty = info.fields[index].ty;
                    if let TypeKind::Struct(own) = self.types.kind(field_ty) {
                        let inner = &self.types.structs[own];
                        if inner.is_class
                            || self
                                .signatures
                                .contains_key(&format!("{}__drop", inner.name))
                        {
                            return Err(format!("overwriting owning field '{field}' requires explicit move/destruction semantics"));
                        }
                    }
                    if op.is_some() {
                        return Err(
                            "compiler bug: compound field assignment was not lowered".into()
                        );
                    }
                    Stmt::FieldSet {
                        root: local,
                        pointer,
                        index,
                        value: self.expect(value, field_ty)?,
                    }
                }
                ast::Stmt::CompoundSet { name, op, value } => {
                    let local = self
                        .find_local(name)
                        .ok_or_else(|| format!("unknown local '{name}'"))?;
                    if !self.locals[local].mutable {
                        return Err(format!(
                            "cannot modify immutable local '{name}'; use 'let mut'"
                        ));
                    }
                    let lhs = ast::Expr::Var(name.clone());
                    let combined = ast::Expr::Binary(Box::new(lhs), *op, Box::new(value.clone()));
                    let ty = self.locals[local].ty;
                    Stmt::Set {
                        local,
                        value: self.expect(&combined, ty)?,
                    }
                }
                ast::Stmt::Return(v) => {
                    let v = match v {
                        Some(v) if self.result != self.types.unit() => {
                            Some(self.expect(v, self.result)?)
                        }
                        Some(_) => {
                            return Err("unit function uses 'return;' without a value".into())
                        }
                        None if self.result == self.types.unit() => None,
                        None => return Err("non-unit function must return a value".into()),
                    };
                    Stmt::Return(v)
                }
                ast::Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let condition = self.expect(condition, self.types.boolean())?;
                    let then_body = self.scoped(then_body, false)?;
                    let else_body = else_body
                        .as_ref()
                        .map(|b| self.scoped(b, false))
                        .transpose()?;
                    Stmt::If {
                        condition,
                        then_body,
                        else_body,
                    }
                }
                ast::Stmt::While { condition, body } => {
                    let condition = self.expect(condition, self.types.boolean())?;
                    let body = self.loop_scoped(body)?;
                    Stmt::While { condition, body }
                }
                ast::Stmt::Loop(body) => Stmt::Loop(self.loop_scoped(body)?),
                ast::Stmt::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    let start = self.expr(start, None)?;
                    if self.types.integer(start.ty).is_none() {
                        return Err("for range requires an integer start".into());
                    }
                    let end = self.expect(end, start.ty)?;
                    self.scopes.push(HashMap::new());
                    let loop_result: Result<Stmt, String> = (|| -> Result<Stmt, String> {
                        let local = self.declare(name, start.ty, false)?;
                        let body = self.loop_scoped(body)?;
                        Ok(Stmt::For {
                            local,
                            start,
                            end,
                            body,
                        })
                    })();
                    self.scopes.pop();
                    loop_result?
                }
                ast::Stmt::Break => {
                    if self.loop_depth == 0 {
                        return Err("break is only allowed inside a loop".into());
                    }
                    Stmt::Break
                }
                ast::Stmt::Continue => {
                    if self.loop_depth == 0 {
                        return Err("continue is only allowed inside a loop".into());
                    }
                    Stmt::Continue
                }
                ast::Stmt::Unsafe(body) => Stmt::Unsafe(self.scoped(body, true)?),
                // Only a function's root tail becomes Return (in surface).
                // In an ordinary statement block, the value is discarded.
                ast::Stmt::Tail(e) => Stmt::Expr(self.expr(e, None)?),
                ast::Stmt::Expr(e) => {
                    let value = self.expr(e, None)?;
                    let own_type = value.ty;
                    if let TypeKind::Struct(struct_id) = self.types.kind(own_type) {
                        let symbol = format!("{}__drop", self.types.structs[struct_id].name);
                        if self.signatures.contains_key(&symbol) {
                            let temp_name = format!("__nova_owned_temporary_{}", self.locals.len());
                            let temporary = self.declare(&temp_name, own_type, false)?;
                            out.push(Stmt::Let {
                                local: temporary,
                                init: value,
                            });
                            if let Some(cleanup) = self.automatic_cleanup(temporary)? {
                                out.push(cleanup);
                            }
                            continue;
                        }
                    }
                    Stmt::Expr(value)
                }
            };
            out.push(stmt);
        }
        Ok(out)
    }
}
fn stops_block(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Break | Stmt::Continue | Stmt::Return(_)) => true,
        Some(Stmt::If {
            then_body,
            else_body: Some(other),
            ..
        }) => stops_block(then_body) && stops_block(other),
        Some(Stmt::Unsafe(body)) => stops_block(body),
        _ => false,
    }
}
fn terminates(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return(_)) => true,
        Some(Stmt::If {
            then_body,
            else_body: Some(else_body),
            ..
        }) => terminates(then_body) && terminates(else_body),
        Some(Stmt::Unsafe(body)) => terminates(body),
        _ => false,
    }
}
fn unescape_string(raw: &str) -> Result<String, String> {
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or("malformed string literal")?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let e = chars.next().ok_or("unfinished escape sequence")?;
        out.push(match e {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            _ => return Err(format!("unknown string escape \\{e}")),
        });
    }
    if out.contains('\0') {
        return Err("embedded NUL bytes are not supported in str literals".into());
    }
    Ok(out)
}
fn field_has_drop(types: &Types, ty: TypeId, signatures: &HashMap<String, Signature>) -> bool {
    match types.kind(ty) {
        TypeKind::Struct(id) => {
            signatures.contains_key(&format!("{}__drop", types.structs[id].name))
        }
        TypeKind::Array { element, .. } => field_has_drop(types, element, signatures),
        _ => false,
    }
}
/// Build calls that dispose owned fields from last to first. The destructor's
/// `self` is already consumed, so moving its fields is safe here.
fn drop_field_actions(
    types: &Types,
    ty: TypeId,
    root: Expr,
    signatures: &HashMap<String, Signature>,
) -> Vec<Stmt> {
    match types.kind(ty) {
        TypeKind::Struct(id) => {
            let symbol = format!("{}__drop", types.structs[id].name);
            let Some(signature) = signatures.get(&symbol) else {
                return Vec::new();
            };
            vec![Stmt::Expr(Expr {
                ty: types.unit(),
                kind: ExprKind::Call(signature.id, vec![root]),
            })]
        }
        TypeKind::Array { element, len } => {
            let mut actions = Vec::new();
            for index in (0..len).rev() {
                let indexed = Expr {
                    ty: element,
                    kind: ExprKind::Index(
                        Box::new(root.clone()),
                        Box::new(Expr {
                            ty: types.i32(),
                            kind: ExprKind::Int(index as i128),
                        }),
                    ),
                };
                actions.extend(drop_field_actions(types, element, indexed, signatures));
            }
            actions
        }
        _ => Vec::new(),
    }
}
pub fn lower(program: &ast::Program) -> Result<Program, String> {
    let mut types = Types::new();
    let mut struct_ids = HashMap::new();
    for item in &program.items {
        if let ast::Item::Struct(s) = item {
            let mut derives = HashSet::new();
            for attr in &s.attributes {
                if attr.args.is_empty() {
                    return Err(format!(
                        "attribute '{}' needs at least one argument",
                        attr.name
                    ));
                }
                if attr.name != "derive" {
                    return Err(format!("unsupported attribute '#[{}(...)]'", attr.name));
                }
                for name in &attr.args {
                    if !derives.insert(name.clone()) {
                        return Err(format!("duplicate derive '{name}'"));
                    }
                    if name != "Serialize" && name != "Deserialize" {
                        return Err(format!(
                            "unknown derive '{name}'; currently supported: Serialize, Deserialize"
                        ));
                    }
                }
            }
            let id = types.register_struct(
                &s.name,
                derives.contains("Serialize"),
                derives.contains("Deserialize"),
                s.is_class,
            )?;
            struct_ids.insert(s.name.clone(), id);
        }
    }
    for item in &program.items {
        if let ast::Item::Struct(s) = item {
            let id = struct_ids[&s.name];
            let mut fields = Vec::new();
            let mut seen = HashSet::new();
            for field in &s.fields {
                if !seen.insert(field.name.clone()) {
                    return Err(format!("duplicate field '{}' on '{}'", field.name, s.name));
                }
                fields.push(crate::types::Field {
                    name: field.name.clone(),
                    ty: types.resolve(&field.ty)?,
                });
            }
            types.structs[id].fields = fields;
        }
    }
    types.check_sized()?;
    for info in &types.structs {
        for field in &info.fields {
            if types.contains_borrow(field.ty) {
                return Err(format!("borrowed field '{}.{}' requires explicit lifetime parameters; cannot store &T in an unbounded struct",info.name,field.name));
            }
        }
    }
    for s in &types.structs {
        if s.serialize {
            for field in &s.fields {
                let supported = match types.kind(field.ty) {
                    TypeKind::Primitive(
                        Primitive::Signed(_)
                        | Primitive::Unsigned(_)
                        | Primitive::Bool
                        | Primitive::Str,
                    ) => true,
                    TypeKind::Struct(nested) => types.structs[nested].serialize,
                    _ => false,
                };
                if !supported {
                    return Err(format!(
                        "#[derive(Serialize)] on '{}': unsupported field '{}' of type {}",
                        s.name,
                        field.name,
                        types.display(field.ty)
                    ));
                }
            }
        }
    }
    // Deserialization is opt-in, independently of stdout serialization.
    // No pointer, float, array or unmanaged resource fields in the first version.
    for s in &types.structs {
        if !s.deserialize {
            continue;
        }
        for field in &s.fields {
            let supported = match types.kind(field.ty) {
                TypeKind::Primitive(
                    Primitive::Signed(_)
                    | Primitive::Unsigned(_)
                    | Primitive::Bool
                    | Primitive::Str,
                ) => true,
                TypeKind::Struct(nested) => types.structs[nested].deserialize,
                _ => false,
            };
            if !supported {
                return Err(format!("#[derive(Deserialize)] on '{}': field '{}' of type {} cannot be deserialized; nested structs also need #[derive(Deserialize)]",s.name,field.name,types.display(field.ty)));
            }
        }
    }
    let mut signatures = HashMap::<String, Signature>::new();
    for item in &program.items {
        let (name, params, result) = match item {
            ast::Item::Function(f) => (&f.name, &f.params, &f.result),
            ast::Item::Extern(f) => (&f.name, &f.params, &f.result),
            _ => continue,
        };
        let p = params
            .iter()
            .map(|param| types.resolve(&param.ty))
            .collect::<Result<Vec<_>, _>>()?;
        let r = types.resolve(result)?;
        if matches!(item, ast::Item::Extern(_)) {
            // Function values lower to LLVM `ptr`, like C function pointers.
            // Only their signature needs additional ABI validation: Nova
            // aggregate parameters/results cannot cross this C boundary.
            fn ffi_safe(types: &Types, ty: TypeId, parameter: bool) -> bool {
                match types.kind(ty) {
                    TypeKind::Primitive(Primitive::Unit) => !parameter,
                    TypeKind::Primitive(_) | TypeKind::Pointer { .. } => true,
                    TypeKind::Function { params, result } => {
                        params.iter().all(|arg| ffi_safe(types, *arg, true))
                            && ffi_safe(types, result, false)
                    }
                    TypeKind::Reference { .. } | TypeKind::Struct(_) | TypeKind::Array { .. } => {
                        false
                    }
                }
            }
            if p.iter().any(|ty| !ffi_safe(&types, *ty, true)) || !ffi_safe(&types, r, false) {
                return Err(format!("extern function '{name}' uses an unsupported value across C ABI; use scalars, pointers, or an ABI-safe function pointer (no aggregate parameters or return values)"));
            }
        }
        let c_variadic = match item {
            ast::Item::Extern(f) => f.c_variadic,
            _ => false,
        };
        let is_unsafe = match item {
            ast::Item::Function(f) => {
                ast::function_is_unsafe(&f.attributes, &format!("function '{name}'"))?
            }
            ast::Item::Extern(f) => {
                ast::function_is_unsafe(&f.attributes, &format!("extern '{name}'"))?
            }
            _ => false,
        };
        if c_variadic && p.is_empty() {
            return Err(format!(
                "extern C variadic '{name}' requires a fixed parameter"
            ));
        }
        let id = signatures.len();
        if signatures
            .insert(
                name.clone(),
                Signature {
                    id,
                    params: p,
                    result: r,
                    c_variadic,
                    is_unsafe,
                },
            )
            .is_some()
        {
            return Err(format!("duplicate function '{name}'"));
        }
    }
    // Synthesize destructors for aggregates containing owned fields.
    // Members are destroyed in reverse declaration order. Named user Drop
    // implementations take precedence over generated field-wise destructors.
    let mut generated_drops: Vec<(usize, String)> = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for (struct_id, info) in types.structs.iter().enumerate() {
            let symbol = format!("{}__drop", info.name);
            if signatures.contains_key(&symbol) {
                continue;
            }
            let has_owned_member = info
                .fields
                .iter()
                .any(|field| field_has_drop(&types, field.ty, &signatures));
            if !has_owned_member {
                continue;
            }
            let id = signatures.len();
            let ty = types
                .named(&info.name)
                .expect("registered struct must be named");
            signatures.insert(
                symbol.clone(),
                Signature {
                    id,
                    params: vec![ty],
                    result: types.unit(),
                    c_variadic: false,
                    is_unsafe: false,
                },
            );
            generated_drops.push((struct_id, symbol));
            changed = true;
        }
    }
    for (sid, s) in types.structs.iter().enumerate() {
        if s.serialize {
            let generated = format!("{}_write_json", s.name);
            if signatures.contains_key(&generated) {
                return Err(format!(
                    "generated serializer function '{generated}' conflicts with user declaration"
                ));
            }
            let id = signatures.len();
            let struct_ty = types.named(&s.name).unwrap();
            signatures.insert(
                generated,
                Signature {
                    id,
                    params: vec![struct_ty],
                    result: types.i32(),
                    c_variadic: false,
                    is_unsafe: false,
                },
            );
            let _ = sid;
            let to_json = format!("{}_to_json", s.name);
            if signatures.contains_key(&to_json) {
                return Err(format!(
                    "generated serializer function '{to_json}' conflicts with user declaration"
                ));
            }
            let id = signatures.len();
            signatures.insert(
                to_json,
                Signature {
                    id,
                    params: vec![struct_ty],
                    result: types.string(),
                    c_variadic: false,
                    is_unsafe: false,
                },
            );
        }
    }
    for s in &types.structs {
        if !s.deserialize {
            continue;
        }
        let generated = format!("{}_from_json", s.name);
        let private_decoder = format!("__nova_json_decode_{}", s.name);
        if signatures.contains_key(&private_decoder) {
            return Err(format!(
                "reserved JSON decoder symbol '{private_decoder}' conflicts with user declaration"
            ));
        }
        if signatures.contains_key(&generated) {
            return Err(format!(
                "generated deserializer function '{generated}' conflicts with user declaration"
            ));
        }
        let id = signatures.len();
        let struct_ty = types.named(&s.name).unwrap();
        signatures.insert(
            generated,
            Signature {
                id,
                params: vec![types.string()],
                result: struct_ty,
                c_variadic: false,
                is_unsafe: false,
            },
        );
    }
    if types.structs.iter().any(|s| s.serialize)
        && ["printf", "putchar"]
            .iter()
            .any(|n| signatures.contains_key(*n))
    {
        return Err("derived JSON writers reserve C ABI symbols 'printf' and 'putchar'; declare neither explicitly".into());
    }
    let mut functions = vec![None; signatures.len()];
    for item in &program.items {
        let (name, params, body, kind) = match item {
            ast::Item::Function(f) => (
                &f.name,
                &f.params,
                Some(f.body.as_slice()),
                FunctionKind::Defined,
            ),
            ast::Item::Extern(f) => (&f.name, &f.params, None, FunctionKind::External),
            _ => continue,
        };
        let signature = &signatures[name];
        let mut resolver = Resolver::new(&signatures, &mut types, signature.result);
        // An unsafe function grants unsafe operations within its own body,
        // while callers still need an explicit unsafe context.
        resolver.unsafe_depth = usize::from(signature.is_unsafe);
        let mut params_ids = Vec::new();
        for (p, ty) in params.iter().zip(&signature.params) {
            params_ids.push(resolver.declare(&p.name, *ty, false)?);
        }
        let mut body = match body {
            Some(body) => resolver
                .block(body)
                .map_err(|error| format!("{error}\n  = typed HIR: in function '{name}'"))?,
            None => vec![],
        };
        if matches!(kind, FunctionKind::Defined) && !name.ends_with("__drop") {
            let mut parameter_cleanups = Vec::new();
            for parameter in &params_ids {
                if let Some(cleanup) = resolver.automatic_cleanup(*parameter)? {
                    parameter_cleanups.push(cleanup);
                }
            }
            parameter_cleanups.append(&mut body);
            body = parameter_cleanups;
        }
        if matches!(kind, FunctionKind::Defined) && !terminates(&body) {
            return Err(format!("function '{name}' does not return on every path"));
        }
        functions[signature.id] = Some(Function {
            name: name.clone(),
            params: params_ids,
            locals: resolver.locals,
            result: signature.result,
            body,
            kind,
            c_variadic: signature.c_variadic,
        });
    }
    for (struct_id, symbol) in generated_drops {
        let signature = &signatures[&symbol];
        let info = &types.structs[struct_id];
        let owner_ty = signature.params[0];
        let mut body = Vec::new();
        let tagged_enum = info.fields.first().map(|field| field.name == "__nova_tag").unwrap_or(false);
        if tagged_enum {
            // Generic enums are represented by a tag plus flattened variant
            // fields. Inactive variants are zero-initialized and MUST NOT be
            // destroyed. Build one guarded cleanup per discriminant.
            let mut variant_tags: Vec<usize> = Vec::new();
            for field in info.fields.iter().skip(1) {
                if let Some(tag) = field.name.strip_prefix("__nova_variant_")
                    .and_then(|rest| rest.split_once('_'))
                    .and_then(|(tag, _)| tag.parse::<usize>().ok()) {
                    if !variant_tags.contains(&tag) { variant_tags.push(tag); }
                }
            }
            // Empty variants produce no payload fields and need no cleanup.
            for tag in variant_tags {
                let mut actions = Vec::new();
                for (index, field) in info.fields.iter().enumerate().rev() {
                    let belongs = field.name.strip_prefix("__nova_variant_")
                        .and_then(|rest| rest.split_once('_'))
                        .and_then(|(field_tag, _)| field_tag.parse::<usize>().ok()) == Some(tag);
                    if !belongs { continue; }
                    let member = Expr {
                        ty: field.ty,
                        kind: ExprKind::Field(Box::new(Expr { ty: owner_ty, kind: ExprKind::Local(0) }), index),
                    };
                    actions.extend(drop_field_actions(&types, field.ty, member, &signatures));
                }
                if !actions.is_empty() {
                    let condition = Expr {
                        ty: types.boolean(),
                        kind: ExprKind::Binary(
                            Box::new(Expr { ty: types.i32(), kind: ExprKind::Field(
                                Box::new(Expr { ty: owner_ty, kind: ExprKind::Local(0) }), 0) }),
                            ast::BinaryOp::Eq,
                            Box::new(Expr { ty: types.i32(), kind: ExprKind::Int(tag as i128) }),
                        ),
                    };
                    body.push(Stmt::If { condition, then_body: actions, else_body: None });
                }
            }
        } else {
            for (field_index, field) in info.fields.iter().enumerate().rev() {
                let receiver = Expr {
                    ty: owner_ty,
                    kind: ExprKind::Local(0),
                };
                let member = Expr {
                    ty: field.ty,
                    kind: ExprKind::Field(Box::new(receiver), field_index),
                };
                body.extend(drop_field_actions(&types, field.ty, member, &signatures));
            }
        }
        body.push(Stmt::Return(None));
        functions[signature.id] = Some(Function {
            name: symbol,
            params: vec![0],
            locals: vec![Local {
                name: "self".into(),
                ty: owner_ty,
                mutable: false,
            }],
            result: types.unit(),
            body,
            kind: FunctionKind::Defined,
            c_variadic: false,
        });
    }
    for (sid, s) in types.structs.iter().enumerate() {
        if !s.serialize {
            continue;
        }
        let name = format!("{}_write_json", s.name);
        let sig = &signatures[&name];
        functions[sig.id] = Some(Function {
            name,
            params: vec![0],
            locals: vec![Local {
                name: "value".into(),
                ty: sig.params[0],
                mutable: false,
            }],
            result: sig.result,
            body: vec![],
            kind: FunctionKind::JsonWriter(sid),
            c_variadic: false,
        });
    }
    for (sid, s) in types.structs.iter().enumerate() {
        if !s.serialize {
            continue;
        }
        let name = format!("{}_to_json", s.name);
        let sig = &signatures[&name];
        functions[sig.id] = Some(Function {
            name,
            params: vec![0],
            locals: vec![Local {
                name: "value".into(),
                ty: sig.params[0],
                mutable: false,
            }],
            result: sig.result,
            body: vec![],
            kind: FunctionKind::JsonToString(sid),
            c_variadic: false,
        });
    }
    for (sid, s) in types.structs.iter().enumerate() {
        if !s.deserialize {
            continue;
        }
        let name = format!("{}_from_json", s.name);
        let sig = &signatures[&name];
        functions[sig.id] = Some(Function {
            name,
            params: vec![0],
            locals: vec![Local {
                name: "text".into(),
                ty: types.string(),
                mutable: false,
            }],
            result: sig.result,
            body: vec![],
            kind: FunctionKind::JsonReader(sid),
            c_variadic: false,
        });
    }
    let functions = functions
        .into_iter()
        .map(|f| f.expect("signature must produce function"))
        .collect();
    let mut result = Program { types, functions };
    crate::ownership::check(&mut result)?;
    Ok(result)
}
