//! LLVM IR text backend. The IR is verified before reaching this module.
use crate::{
    ast::{BinaryOp, UnaryOp},
    hir::FunctionKind,
    mir::{self, Instruction, Operand, PlaceProjection, Rvalue, Terminator},
    types::{Primitive, TypeId, TypeKind},
};
use std::collections::HashMap;

#[derive(Default)]
struct Strings {
    ids: HashMap<String, usize>,
    entries: Vec<String>,
}
impl Strings {
    fn intern(&mut self, text: &str) -> String {
        if let Some(id) = self.ids.get(text) {
            return format!("@.str{id}");
        }
        let id = self.entries.len();
        self.entries.push(text.into());
        self.ids.insert(text.into(), id);
        format!("@.str{id}")
    }
    fn definitions(&self) -> String {
        let mut lines = String::new();
        for (id, s) in self.entries.iter().enumerate() {
            let mut buf = s.as_bytes().to_vec();
            buf.push(0);
            let repr = buf
                .iter()
                .map(|b| match *b {
                    32..=126 if *b != b'"' && *b != b'\\' => (*b as char).to_string(),
                    _ => format!("\\{:02X}", *b),
                })
                .collect::<String>();
            lines.push_str(&format!(
                "@.str{id} = private unnamed_addr constant [{} x i8] c\"{repr}\", align 1\n",
                buf.len()
            ));
        }
        lines
    }
}
struct Emitter<'a> {
    program: &'a mir::Program,
    func: &'a mir::Function,
    pool: &'a mut Strings,
    next: usize,
    lines: Vec<String>,
    needs_trap: bool,
}
fn dyn_name_part(value: &str) -> String {
    value.replace("::", "__")
}
fn dyn_method_info(name: &str) -> Option<(String, usize, String)> {
    // Namespaced traits prefix the generated slot with their module name.
    let rest = name.split_once("__nova_dyn__")?.1;
    let parts = rest.rsplitn(3, "__").collect::<Vec<_>>();
    Some((
        parts.get(2)?.to_string(),
        parts.get(1)?.parse().ok()?,
        parts.first()?.to_string(),
    ))
}
fn dyn_impl<'a>(
    program: &'a mir::Program,
    concrete: TypeId,
    method: &str,
) -> Option<&'a mir::Function> {
    program.functions.iter().find(|function| {
        if !matches!(function.kind, FunctionKind::Defined)
            || !function.name.ends_with(&format!("__{method}"))
        {
            return false;
        }
        let Some(first) = function.params.first() else {
            return false;
        };
        match program.types.kind(function.locals[*first]) {
            TypeKind::Reference { element, .. } | TypeKind::Pointer { element, .. } => {
                element == concrete
            }
            _ => false,
        }
    })
}
fn dyn_slot_count(program: &mir::Program, trait_name: &str) -> usize {
    program
        .functions
        .iter()
        .filter_map(|function| {
            let (key, slot, _) = dyn_method_info(&function.name)?;
            (key == dyn_name_part(trait_name)).then_some(slot + 1)
        })
        .max()
        .unwrap_or(1)
}
fn dyn_vtables(program: &mir::Program) -> String {
    let mut output = String::new();
    let mut seen = std::collections::HashSet::new();
    for function in &program.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let mir::Instruction::Assign {
                    value: mir::Rvalue::DynObjectRef(value, trait_name),
                    ..
                } = instruction
                else {
                    continue;
                };
                let source = mir::operand_ty(value, function, &program.types).ok();
                let Some(
                    TypeKind::Reference {
                        element: concrete, ..
                    }
                    | TypeKind::Pointer {
                        element: concrete, ..
                    },
                ) = source.map(|ty| program.types.kind(ty))
                else {
                    continue;
                };
                let key = format!("{}::{}", trait_name, concrete);
                if !seen.insert(key) {
                    continue;
                }
                let methods = program
                    .functions
                    .iter()
                    .filter_map(|candidate| {
                        let (trait_key, slot, method) = dyn_method_info(&candidate.name)?;
                        if trait_key == dyn_name_part(trait_name) {
                            Some((slot, method))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let Some(max_slot) = methods.iter().map(|(slot, _)| *slot).max() else {
                    continue;
                };
                let global = format!(
                    "@__nova_vtable_{}_{}",
                    dyn_name_part(trait_name),
                    dyn_name_part(&program.types.display(concrete))
                );
                let mut entries = vec!["ptr null".to_owned(); max_slot + 1];
                for (slot, method) in methods {
                    if let Some(implementation) = dyn_impl(program, concrete, &method) {
                        entries[slot] =
                            format!("ptr @{}", function_link_symbol(program, implementation));
                    }
                }
                output.push_str(&format!(
                    "{global} = private constant [{} x ptr] [{}]\n",
                    entries.len(),
                    entries.join(", ")
                ));
            }
        }
    }
    output
}
impl<'a> Emitter<'a> {
    fn temp(&mut self) -> String {
        let v = format!("%t{}", self.next);
        self.next += 1;
        v
    }
    fn ty(&self, id: TypeId) -> String {
        self.program.types.llvm(id)
    }
    fn operand_ty(&self, op: &Operand) -> TypeId {
        mir::operand_ty(op, self.func, &self.program.types).expect("verified operand")
    }
    fn operand(&mut self, op: &Operand) -> String {
        match op {
            Operand::Local(id) => {
                let dst = self.temp();
                let ty = self.ty(self.func.locals[*id]);
                self.lines
                    .push(format!("  {dst} = load {ty}, ptr %slot{id}"));
                dst
            }
            Operand::Int(value, _) => value.to_string(),
            Operand::Float(value, ty) => {
                let v = if matches!(
                    self.program.types.kind(*ty),
                    TypeKind::Primitive(Primitive::Float(32))
                ) {
                    (*value as f32 as f64).to_bits()
                } else {
                    value.to_bits()
                };
                format!("0x{v:016X}")
            }
            Operand::Bool(b) => (if *b { "1" } else { "0" }).into(),
            Operand::Str(s, _) => self.pool.intern(s),
            Operand::Unit => "".into(),
            Operand::Zero(_) => "zeroinitializer".into(),
        }
    }
    fn value(&mut self, rv: &Rvalue, target: Option<TypeId>) -> Option<String> {
        match rv {
            Rvalue::Use(op) => Some(self.operand(op)),
            Rvalue::DynObjectRef(value, trait_name) => {
                let source = self.operand_ty(value);
                let concrete = match self.program.types.kind(source) {
                    TypeKind::Reference { element, .. } | TypeKind::Pointer { element, .. } => {
                        element
                    }
                    _ => unreachable!("verified dyn object source"),
                };
                let global = format!(
                    "@__nova_vtable_{}_{}",
                    dyn_name_part(trait_name),
                    dyn_name_part(&self.program.types.display(concrete))
                );
                let slots = dyn_slot_count(self.program, trait_name);
                let object = self.temp();
                self.lines
                    .push(format!("  {object} = alloca {{ ptr, ptr }}"));
                let data = self.operand(value);
                let data_slot = self.temp();
                self.lines.push(format!(
                    "  {data_slot} = getelementptr {{ ptr, ptr }}, ptr {object}, i32 0, i32 0"
                ));
                self.lines
                    .push(format!("  store ptr {data}, ptr {data_slot}"));
                let table = self.temp();
                self.lines.push(format!(
                    "  {table} = getelementptr [{slots} x ptr], ptr {global}, i32 0, i32 0"
                ));
                let table_slot = self.temp();
                self.lines.push(format!(
                    "  {table_slot} = getelementptr {{ ptr, ptr }}, ptr {object}, i32 0, i32 1"
                ));
                self.lines
                    .push(format!("  store ptr {table}, ptr {table_slot}"));
                Some(object)
            }
            Rvalue::Cast(src, dest) => {
                let source = self.operand_ty(src);
                let from = self.ty(source);
                let to = self.ty(*dest);
                let value = self.operand(src);
                if source == *dest {
                    return Some(value);
                }
                let sk = self.program.types.kind(source);
                let dk = self.program.types.kind(*dest);
                let op = match (sk.clone(), dk) {
                    (
                        TypeKind::Primitive(Primitive::Char),
                        TypeKind::Primitive(Primitive::Signed(db) | Primitive::Unsigned(db)),
                    ) => {
                        if db == 32 {
                            return Some(value);
                        }
                        if db < 32 {
                            "trunc"
                        } else {
                            "zext"
                        }
                    }
                    (
                        TypeKind::Primitive(Primitive::Signed(sb) | Primitive::Unsigned(sb)),
                        TypeKind::Primitive(Primitive::Char),
                    ) => {
                        if sb == 32 {
                            return Some(value);
                        }
                        if sb > 32 {
                            "trunc"
                        } else if matches!(sk, TypeKind::Primitive(Primitive::Signed(_))) {
                            "sext"
                        } else {
                            "zext"
                        }
                    }
                    (
                        TypeKind::Primitive(Primitive::Char),
                        TypeKind::Primitive(Primitive::Char),
                    ) => return Some(value),
                    (
                        TypeKind::Primitive(Primitive::Signed(sb) | Primitive::Unsigned(sb)),
                        TypeKind::Primitive(Primitive::Signed(db) | Primitive::Unsigned(db)),
                    ) => {
                        if sb == db {
                            return Some(value);
                        }
                        if sb > db {
                            "trunc"
                        } else if matches!(sk, TypeKind::Primitive(Primitive::Signed(_))) {
                            "sext"
                        } else {
                            "zext"
                        }
                    }
                    (
                        TypeKind::Primitive(Primitive::Float(sb)),
                        TypeKind::Primitive(Primitive::Float(db)),
                    ) => {
                        if sb == db {
                            return Some(value);
                        }
                        if sb < db {
                            "fpext"
                        } else {
                            "fptrunc"
                        }
                    }
                    (
                        TypeKind::Primitive(Primitive::Signed(_)),
                        TypeKind::Primitive(Primitive::Float(_)),
                    ) => "sitofp",
                    (
                        TypeKind::Primitive(Primitive::Unsigned(_)),
                        TypeKind::Primitive(Primitive::Float(_)),
                    ) => "uitofp",
                    (
                        TypeKind::Primitive(Primitive::Float(_)),
                        TypeKind::Primitive(Primitive::Signed(_)),
                    ) => "fptosi",
                    (
                        TypeKind::Primitive(Primitive::Float(_)),
                        TypeKind::Primitive(Primitive::Unsigned(_)),
                    ) => "fptoui",
                    (TypeKind::Pointer { .. }, TypeKind::Pointer { .. }) => return Some(value),
                    (TypeKind::Reference { .. }, TypeKind::Pointer { .. }) => return Some(value),
                    (
                        TypeKind::Reference {
                            element: a,
                            mutable: true,
                        },
                        TypeKind::Reference {
                            element: b,
                            mutable: false,
                        },
                    ) if a == b => {
                        return Some(value);
                    }
                    (TypeKind::Pointer { .. }, TypeKind::Primitive(Primitive::Unsigned(64))) => {
                        "ptrtoint"
                    }
                    (TypeKind::Primitive(Primitive::Unsigned(64)), TypeKind::Pointer { .. }) => {
                        "inttoptr"
                    }
                    (TypeKind::Function { .. }, TypeKind::Primitive(Primitive::Unsigned(64))) => {
                        "ptrtoint"
                    }
                    (TypeKind::Primitive(Primitive::Unsigned(64)), TypeKind::Function { .. }) => {
                        "inttoptr"
                    }
                    _ => unreachable!(
                        "only numeric or explicit function-handle casts are legal in typed MIR"
                    ),
                };
                let dst = self.temp();
                self.lines
                    .push(format!("  {dst} = {op} {from} {value} to {to}"));
                Some(dst)
            }
            Rvalue::Unary(op, arg) => {
                let ty = self.ty(self.operand_ty(arg));
                let value = self.operand(arg);
                let dst = self.temp();
                let float = self.program.types.float(self.operand_ty(arg));
                let opstr = match op {
                    UnaryOp::Neg if float => format!("fsub {ty} -0.0, {value}"),
                    UnaryOp::Neg => format!("sub {ty} 0, {value}"),
                    UnaryOp::Not => format!("xor i1 {value}, true"),
                    UnaryOp::BitNot => format!("xor {ty} {value}, -1"),
                };
                self.lines.push(format!("  {dst} = {opstr}"));
                Some(dst)
            }
            Rvalue::Binary(op, left, right) => {
                let id = self.operand_ty(left);
                let ty = self.ty(id);
                let signed = self
                    .program
                    .types
                    .integer(id)
                    .map(|(_, s)| s)
                    .unwrap_or(true);
                let float = self.program.types.float(id);
                let a = self.operand(left);
                let b = self.operand(right);
                let dst = self.temp();
                let instr = match op {
                    BinaryOp::Add => {
                        format!("{} {ty} {a}, {b}", if float { "fadd" } else { "add" })
                    }
                    BinaryOp::Sub => {
                        format!("{} {ty} {a}, {b}", if float { "fsub" } else { "sub" })
                    }
                    BinaryOp::Mul => {
                        format!("{} {ty} {a}, {b}", if float { "fmul" } else { "mul" })
                    }
                    BinaryOp::Div => format!(
                        "{} {ty} {a}, {b}",
                        if float {
                            "fdiv"
                        } else if signed {
                            "sdiv"
                        } else {
                            "udiv"
                        }
                    ),
                    BinaryOp::Rem => format!(
                        "{} {ty} {a}, {b}",
                        if float {
                            "frem"
                        } else if signed {
                            "srem"
                        } else {
                            "urem"
                        }
                    ),
                    BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge => {
                        let cmp = match op {
                            BinaryOp::Eq => "eq",
                            BinaryOp::Ne => "ne",
                            BinaryOp::Lt => "lt",
                            BinaryOp::Le => "le",
                            BinaryOp::Gt => "gt",
                            _ => "ge",
                        };
                        if float {
                            let pred = match op {
                                BinaryOp::Eq => "oeq",
                                BinaryOp::Ne => "une",
                                BinaryOp::Lt => "olt",
                                BinaryOp::Le => "ole",
                                BinaryOp::Gt => "ogt",
                                _ => "oge",
                            };
                            format!("fcmp {pred} {ty} {a}, {b}")
                        } else {
                            let pred = if cmp == "eq" || cmp == "ne" {
                                cmp.into()
                            } else {
                                format!("{}{cmp}", if signed { "s" } else { "u" })
                            };
                            format!("icmp {pred} {ty} {a}, {b}")
                        }
                    }
                    BinaryOp::BitAnd => format!("and {ty} {a}, {b}"),
                    BinaryOp::BitOr => format!("or {ty} {a}, {b}"),
                    BinaryOp::BitXor => format!("xor {ty} {a}, {b}"),
                    BinaryOp::Shl => format!("shl {ty} {a}, {b}"),
                    BinaryOp::Shr => {
                        format!("{} {ty} {a}, {b}", if signed { "ashr" } else { "lshr" })
                    }
                    BinaryOp::And | BinaryOp::Or => {
                        unreachable!("boolean short circuit must be lowered to MIR CFG")
                    }
                };
                self.lines.push(format!("  {dst} = {instr}"));
                Some(dst)
            }
            Rvalue::FnRef(id, _) => Some(format!(
                "@{}",
                function_link_symbol(self.program, &self.program.functions[*id])
            )),
            Rvalue::CallIndirect(callee, args) => {
                let TypeKind::Function { result, .. } =
                    self.program.types.kind(self.operand_ty(callee))
                else {
                    unreachable!()
                };
                let target = self.operand(callee);
                let mut llvm_args = Vec::new();
                for arg in args {
                    let ty = self.ty(self.operand_ty(arg));
                    let value = self.operand(arg);
                    llvm_args.push(format!("{ty} {value}"));
                }
                let ret = self.ty(result);
                let call = format!("call {ret} {target}({})", llvm_args.join(", "));
                if result == self.program.types.unit() {
                    self.lines.push(format!("  {call}"));
                    None
                } else {
                    let dst = self.temp();
                    self.lines.push(format!("  {dst} = {call}"));
                    Some(dst)
                }
            }
            Rvalue::Call(id, args) => {
                let called = &self.program.functions[*id];
                if let FunctionKind::DynMethod { slot } = called.kind {
                    let object = self.operand(&args[0]);
                    let object_value = self.temp();
                    self.lines.push(format!(
                        "  {object_value} = load {{ ptr, ptr }}, ptr {object}"
                    ));
                    let data = self.temp();
                    let table = self.temp();
                    self.lines.push(format!(
                        "  {data} = extractvalue {{ ptr, ptr }} {object_value}, 0"
                    ));
                    self.lines.push(format!(
                        "  {table} = extractvalue {{ ptr, ptr }} {object_value}, 1"
                    ));
                    let entry = self.temp();
                    self.lines.push(format!(
                        "  {entry} = getelementptr ptr, ptr {table}, i64 {slot}"
                    ));
                    let target_fn = self.temp();
                    self.lines
                        .push(format!("  {target_fn} = load ptr, ptr {entry}"));
                    let ret = self.ty(called.result);
                    let mut values = vec![format!("ptr {data}")];
                    for (arg, param) in args.iter().skip(1).zip(called.params.iter().skip(1)) {
                        values.push(format!(
                            "{} {}",
                            self.ty(called.locals[*param]),
                            self.operand(arg)
                        ));
                    }
                    let call = format!("call {ret} {target_fn}({})", values.join(", "));
                    if called.result == self.program.types.unit() {
                        self.lines.push(format!("  {call}"));
                        return None;
                    }
                    let result = self.temp();
                    self.lines.push(format!("  {result} = {call}"));
                    return Some(result);
                }
                // Pointer offset is a compiler intrinsic. Its hidden declaration
                // exists only so the typed pipeline can carry the call; emit LLVM
                // directly and never expose a STD or C symbol.
                // Check the same invariants as the former C implementation.
                if matches!(
                    called.kind,
                    FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::ByteOffset)
                ) {
                    assert_eq!(args.len(), 2, "verified raw offset ABI");
                    // The typed compiler ABI carries addresses as u64 so the
                    // intrinsic remains representable in Nova's scalar HIR.
                    // LLVM GEP, however, requires an actual pointer value.
                    let raw_address = self.operand(&args[0]);
                    let address = self.temp();
                    self.lines
                        .push(format!("  {address} = inttoptr i64 {raw_address} to ptr"));
                    let distance = self.operand(&args[1]);
                    let missing = self.temp();
                    let too_far = self.temp();
                    let invalid = self.temp();
                    self.lines
                        .push("  ; compiler intrinsic: byte_offset".into());
                    self.lines
                        .push(format!("  {missing} = icmp eq ptr {address}, null"));
                    self.lines.push(format!(
                        "  {too_far} = icmp ugt i64 {distance}, 9223372036854775807"
                    ));
                    self.lines
                        .push(format!("  {invalid} = or i1 {missing}, {too_far}"));
                    let ordinal = self.next;
                    self.next += 1;
                    let rejected = format!("raw_pointer_invalid_{ordinal}");
                    let valid = format!("raw_pointer_valid_{ordinal}");
                    self.lines.push(format!(
                        "  br i1 {invalid}, label %{rejected}, label %{valid}"
                    ));
                    self.lines.push(format!("{rejected}:"));
                    self.lines.push("  call void @llvm.trap()".into());
                    self.lines.push("  unreachable".into());
                    self.lines.push(format!("{valid}:"));
                    let result_pointer = self.temp();
                    self.lines.push(format!(
                        "  {result_pointer} = getelementptr i8, ptr {address}, i64 {distance}"
                    ));
                    let result = self.temp();
                    self.lines
                        .push(format!("  {result} = ptrtoint ptr {result_pointer} to i64"));
                    self.needs_trap = true;
                    return Some(result);
                }
                if let FunctionKind::CompilerIntrinsic(intrinsic) = called.kind {
                    let llvm_name = match intrinsic {
                        crate::intrinsics::Intrinsic::CopyBytes => "llvm.memcpy.p0.p0.i64",
                        crate::intrinsics::Intrinsic::MoveBytes => "llvm.memmove.p0.p0.i64",
                        crate::intrinsics::Intrinsic::SetBytes => "llvm.memset.p0.i64",
                        crate::intrinsics::Intrinsic::LoadByte => "load_byte",
                        crate::intrinsics::Intrinsic::HeapAllocate => "aligned_alloc",
                        crate::intrinsics::Intrinsic::HeapFree => "free",
                        crate::intrinsics::Intrinsic::TextLength => "__nova_internal_text_length",
                        crate::intrinsics::Intrinsic::TextAddress => "__nova_internal_text_address",
                        crate::intrinsics::Intrinsic::BorrowText => "__nova_internal_borrow_text",
                        _ => "",
                    };
                    if !llvm_name.is_empty() {
                        let values = args.iter().map(|arg| self.operand(arg)).collect::<Vec<_>>();
                        let result = self.temp();
                        match intrinsic {
                            crate::intrinsics::Intrinsic::CopyBytes => {
                                let destination = self.temp();
                                let source = self.temp();
                                self.lines.push(format!(
                                    "  {destination} = inttoptr i64 {} to ptr",
                                    values[0]
                                ));
                                self.lines.push(format!(
                                    "  {source} = inttoptr i64 {} to ptr",
                                    values[1]
                                ));
                                self.lines.push(format!("  call void @llvm.memcpy.p0.p0.i64(ptr {destination}, ptr {source}, i64 {}, i1 false)", values[2]));
                                return None;
                            }
                            crate::intrinsics::Intrinsic::MoveBytes => {
                                let destination = self.temp();
                                let source = self.temp();
                                self.lines.push(format!(
                                    "  {destination} = inttoptr i64 {} to ptr",
                                    values[0]
                                ));
                                self.lines.push(format!(
                                    "  {source} = inttoptr i64 {} to ptr",
                                    values[1]
                                ));
                                self.lines.push(format!("  call void @llvm.memmove.p0.p0.i64(ptr {destination}, ptr {source}, i64 {}, i1 false)", values[2]));
                                return None;
                            }
                            crate::intrinsics::Intrinsic::SetBytes => {
                                let destination = self.temp();
                                self.lines.push(format!(
                                    "  {destination} = inttoptr i64 {} to ptr",
                                    values[0]
                                ));
                                self.lines.push(format!("  call void @llvm.memset.p0.i64(ptr {destination}, i8 {}, i64 {}, i1 false)", values[1], values[2]));
                                return None;
                            }
                            crate::intrinsics::Intrinsic::LoadByte => {
                                let pointer = self.temp();
                                self.lines.push(format!(
                                    "  {pointer} = inttoptr i64 {} to ptr",
                                    values[0]
                                ));
                                self.lines
                                    .push(format!("  {result} = load i8, ptr {pointer}"));
                                return Some(result);
                            }
                            crate::intrinsics::Intrinsic::HeapAllocate => {
                                self.lines.push(format!(
                                    "  {result} = call ptr @aligned_alloc(i64 {}, i64 {})",
                                    values[0], values[1]
                                ));
                                let address = self.temp();
                                self.lines
                                    .push(format!("  {address} = ptrtoint ptr {result} to i64"));
                                return Some(address);
                            }
                            crate::intrinsics::Intrinsic::HeapFree => {
                                let pointer = self.temp();
                                self.lines.push(format!(
                                    "  {pointer} = inttoptr i64 {} to ptr",
                                    values[0]
                                ));
                                self.lines.push(format!("  call void @free(ptr {pointer})"));
                                return None;
                            }
                            crate::intrinsics::Intrinsic::TextLength => {
                                self.lines.push(format!(
                                    "  {result} = call i64 @__nova_internal_text_length(ptr {})",
                                    values[0]
                                ));
                                return Some(result);
                            }
                            crate::intrinsics::Intrinsic::TextAddress => {
                                self.lines.push(format!(
                                    "  {result} = ptrtoint ptr {} to i64",
                                    values[0]
                                ));
                                return Some(result);
                            }
                            crate::intrinsics::Intrinsic::BorrowText => {
                                self.lines.push(format!(
                                    "  {result} = inttoptr i64 {} to ptr",
                                    values[0]
                                ));
                                return Some(result);
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                if let FunctionKind::CompilerIntrinsic(intrinsic) = called.kind {
                    let llvm_name = match intrinsic {
                        crate::intrinsics::Intrinsic::SqrtF64 => "llvm.sqrt.f64",
                        crate::intrinsics::Intrinsic::AbsF64 => "llvm.fabs.f64",
                        crate::intrinsics::Intrinsic::FloorF64 => "llvm.floor.f64",
                        crate::intrinsics::Intrinsic::CeilF64 => "llvm.ceil.f64",
                        _ => "",
                    };
                    if !llvm_name.is_empty() {
                        assert_eq!(args.len(), 1, "verified unary f64 intrinsic ABI");
                        let value = self.operand(&args[0]);
                        let result = self.temp();
                        self.lines.push(format!(
                            "  {result} = call double @{llvm_name}(double {value})"
                        ));
                        return Some(result);
                    }
                }
                let c_variadic = called.c_variadic;
                let fixed_count = called.params.len();
                let result_ty = called.result;
                let fixed_types = called
                    .params
                    .iter()
                    .map(|p| self.ty(called.locals[*p]))
                    .collect::<Vec<_>>();
                let mut llvm_args = Vec::with_capacity(args.len());
                for (index, arg) in args.iter().enumerate() {
                    let arg_ty = self.operand_ty(arg);
                    let ty = self.ty(arg_ty);
                    let mut value = self.operand(arg);
                    let mut llvm_ty = ty.clone();
                    // C default argument promotions (ISO C 6.5.2.2):
                    // f32 -> f64, integer narrower than int -> i32.
                    if c_variadic && index >= fixed_count {
                        match self.program.types.kind(arg_ty) {
                            TypeKind::Primitive(Primitive::Float(32)) => {
                                let dst = self.temp();
                                self.lines
                                    .push(format!("  {dst} = fpext float {value} to double"));
                                value = dst;
                                llvm_ty = "double".into();
                            }
                            TypeKind::Primitive(Primitive::Signed(bits)) if bits < 32 => {
                                let dst = self.temp();
                                self.lines
                                    .push(format!("  {dst} = sext {ty} {value} to i32"));
                                value = dst;
                                llvm_ty = "i32".into();
                            }
                            TypeKind::Primitive(Primitive::Unsigned(bits)) if bits < 32 => {
                                let dst = self.temp();
                                self.lines
                                    .push(format!("  {dst} = zext {ty} {value} to i32"));
                                value = dst;
                                llvm_ty = "i32".into();
                            }
                            TypeKind::Primitive(Primitive::Bool) => {
                                let dst = self.temp();
                                self.lines.push(format!("  {dst} = zext i1 {value} to i32"));
                                value = dst;
                                llvm_ty = "i32".into();
                            }
                            _ => {}
                        }
                    }
                    llvm_args.push(format!("{llvm_ty} {value}"));
                }
                let args = llvm_args.join(", ");
                let result = self.ty(result_ty);
                let symbol = function_link_symbol(self.program, called);
                let callee = if c_variadic {
                    let mut fixed = fixed_types;
                    fixed.push("...".into());
                    format!("({}) @{symbol}", fixed.join(", "))
                } else {
                    format!("@{symbol}")
                };
                if result_ty == self.program.types.unit() {
                    self.lines.push(format!("  call void {callee}({args})"));
                    None
                } else {
                    let dst = self.temp();
                    self.lines
                        .push(format!("  {dst} = call {result} {callee}({args})"));
                    Some(dst)
                }
            }
            Rvalue::StructInit(id, values) => {
                let ty = self.ty(target.expect("struct assign destination"));
                let mut aggregate = "poison".to_string();
                for (index, operand) in values.iter().enumerate() {
                    let field_id = self.program.types.structs[*id].fields[index].ty;
                    let field_ty = self.program.types.llvm_field(field_id);
                    // A unit payload has no runtime value; LLVM still needs
                    // a typed zero-sized initializer for its aggregate slot.
                    let val = if field_id == self.program.types.unit() {
                        "zeroinitializer".into()
                    } else {
                        self.operand(operand)
                    };
                    let dst = self.temp();
                    self.lines.push(format!(
                        "  {dst} = insertvalue {ty} {aggregate}, {field_ty} {val}, {index}"
                    ));
                    aggregate = dst;
                }
                if values.is_empty() {
                    Some("zeroinitializer".into())
                } else {
                    Some(aggregate)
                }
            }
            Rvalue::Field(base, index) => {
                let ty = self.ty(self.operand_ty(base));
                let val = self.operand(base);
                let dst = self.temp();
                self.lines
                    .push(format!("  {dst} = extractvalue {ty} {val}, {index}"));
                Some(dst)
            }
            Rvalue::Array(values) => {
                let ty_id = target.expect("array destination");
                let ty = self.ty(ty_id);
                let TypeKind::Array { element, .. } = self.program.types.kind(ty_id) else {
                    unreachable!()
                };
                let elem_ty = self.ty(element);
                let mut aggregate = "poison".to_string();
                for (i, v) in values.iter().enumerate() {
                    let val = self.operand(v);
                    let dst = self.temp();
                    self.lines.push(format!(
                        "  {dst} = insertvalue {ty} {aggregate}, {elem_ty} {val}, {i}"
                    ));
                    aggregate = dst;
                }
                Some(aggregate)
            }
            Rvalue::Index(array, index) => {
                let array_ty = self.operand_ty(array);
                let (element, length) = match self.program.types.kind(array_ty) {
                    TypeKind::Array { len, element } => (element, Some(len)),
                    TypeKind::Pointer { element, .. } => (element, None),
                    _ => unreachable!("HIR validated indexed operand"),
                };
                let val = self.operand(array);
                let index_val = self.operand(index);
                if let Some(len) = length {
                    let valid = self.temp();
                    self.lines.push(format!("  {valid} = icmp ult i32 {index_val}, {len}"));
                    let unique = self.next;
                    self.next += 1;
                    self.lines.push(format!("  br i1 {valid}, label %index.ok{unique}, label %index.trap{unique}"));
                    self.lines.push(format!("index.trap{unique}:"));
                    self.lines.push("  call void @llvm.trap()".into());
                    self.lines.push("  unreachable".into());
                    self.lines.push(format!("index.ok{unique}:"));
                    self.needs_trap = true;
                }
                let base = if length.is_some() {
                    let aggregate_ty = self.ty(array_ty);
                    let stack = self.temp();
                    self.lines.push(format!("  {stack} = alloca {aggregate_ty}"));
                    self.lines.push(format!("  store {aggregate_ty} {val}, ptr {stack}"));
                    let ptr = self.temp();
                    self.lines.push(format!("  {ptr} = getelementptr inbounds {aggregate_ty}, ptr {stack}, i32 0, i32 0"));
                    ptr
                } else { val };
                let gep = self.temp();
                self.lines.push(format!("  {gep} = getelementptr inbounds {}, ptr {base}, i32 {index_val}", self.ty(element)));
                let dst = self.temp();
                self.lines.push(format!("  {dst} = load {}, ptr {gep}", self.ty(element)));
                Some(dst)
            }
            Rvalue::AddressOf(place) => {
                // Address the original slot, not the by-value Index rvalue's
                // temporary. GEP follows field/index projections in place.
                let mut addr = format!("%slot{}", place.root);
                let mut current_ty = self.func.locals[place.root];
                for projection in &place.projections {
                    let aggregate_ty = self.ty(current_ty);
                    match projection {
                        PlaceProjection::Deref => {
                            let new_addr = self.temp();
                            self.lines
                                .push(format!("  {new_addr} = load ptr, ptr {addr}"));
                            addr = new_addr;
                            current_ty = match self.program.types.kind(current_ty) {
                                TypeKind::Pointer { element, .. }
                                | TypeKind::Reference { element, .. } => element,
                                _ => unreachable!("verified dereference place"),
                            };
                        }
                        PlaceProjection::Field(field) => {
                            let TypeKind::Struct(sid) = self.program.types.kind(current_ty) else {
                                unreachable!("verified struct place")
                            };
                            let new_addr = self.temp();
                            self.lines.push(format!(
                                "  {new_addr} = getelementptr inbounds {aggregate_ty}, ptr {addr}, i32 0, i32 {field}"
                            ));
                            addr = new_addr;
                            current_ty = self.program.types.structs[sid].fields[*field].ty;
                        }
                        PlaceProjection::Index(index) => {
                            let (element, is_array) = match self.program.types.kind(current_ty) {
                                TypeKind::Array { element, .. } => (element, true),
                                TypeKind::Pointer { element, .. } => (element, false),
                                _ => unreachable!("verified array/pointer place"),
                            };
                            let index_val = self.operand(index);
                            if is_array {
                                let valid = self.temp();
                                let len = match self.program.types.kind(current_ty) {
                                    TypeKind::Array { len, .. } => len,
                                    _ => unreachable!(),
                                };
                                self.lines.push(format!("  {valid} = icmp ult i32 {index_val}, {len}"));
                                let unique = self.next;
                                self.next += 1;
                                self.lines.push(format!("  br i1 {valid}, label %borrow.index.ok{unique}, label %borrow.index.trap{unique}"));
                                self.lines.push(format!("borrow.index.trap{unique}:"));
                                self.lines.push("  call void @llvm.trap()".into());
                                self.lines.push("  unreachable".into());
                                self.lines.push(format!("borrow.index.ok{unique}:"));
                            } else {
                                let loaded = self.temp();
                                self.lines.push(format!("  {loaded} = load ptr, ptr {addr}"));
                                addr = loaded;
                            }
                            let new_addr = self.temp();
                            if is_array {
                                self.lines.push(format!("  {new_addr} = getelementptr inbounds {aggregate_ty}, ptr {addr}, i32 0, i32 {index_val}"));
                            } else {
                                self.lines.push(format!("  {new_addr} = getelementptr inbounds {}, ptr {addr}, i32 {index_val}", self.ty(element)));
                            }
                            if is_array { self.needs_trap = true; }
                            addr = new_addr;
                            current_ty = element;
                        }
                    }
                }
                Some(addr)
            }
            Rvalue::Deref(ptr) => {
                let v = self.operand(ptr);
                let dst = self.temp();
                let ty = self.ty(target.expect("deref destination"));
                self.lines.push(format!("  {dst} = load {ty}, ptr {v}"));
                Some(dst)
            }
        }
    }
    fn block(&mut self, bb: &mir::BasicBlock) {
        for ins in &bb.instructions {
            match ins {
                Instruction::Assign { dst, value } => {
                    let ty = self.ty(self.func.locals[*dst]);
                    if let Some(v) = self.value(value, Some(self.func.locals[*dst])) {
                        self.lines.push(format!("  store {ty} {v}, ptr %slot{dst}"));
                    }
                }
                Instruction::StoreDeref { pointer, value } => {
                    let pointer_type = self.func.locals[*pointer];
                    let element = match self.program.types.kind(pointer_type) {
                        TypeKind::Pointer { element, .. } | TypeKind::Reference { element, .. } => {
                            element
                        }
                        _ => unreachable!("verified pointer store"),
                    };
                    let destination = self.temp();
                    self.lines
                        .push(format!("  {destination} = load ptr, ptr %slot{pointer}"));
                    let content = self.operand(value);
                    self.lines.push(format!(
                        "  store {} {content}, ptr {destination}",
                        self.ty(element)
                    ));
                }
                Instruction::StoreField {
                    root,
                    pointer,
                    index,
                    value,
                } => {
                    let root_ty = self.func.locals[*root];
                    let struct_ty = if *pointer {
                        match self.program.types.kind(root_ty) {
                            TypeKind::Pointer { element, .. }
                            | TypeKind::Reference { element, .. } => element,
                            _ => unreachable!("verified MIR field pointer"),
                        }
                    } else {
                        root_ty
                    };
                    let TypeKind::Struct(sid) = self.program.types.kind(struct_ty) else {
                        unreachable!("verified MIR field base")
                    };
                    let base = if *pointer {
                        let ptr = self.temp();
                        self.lines
                            .push(format!("  {ptr} = load ptr, ptr %slot{root}"));
                        ptr
                    } else {
                        format!("%slot{root}")
                    };
                    let gep = self.temp();
                    let struct_llvm = self.ty(struct_ty);
                    self.lines.push(format!("  {gep} = getelementptr inbounds {struct_llvm}, ptr {base}, i32 0, i32 {index}"));
                    let field_id = self.program.types.structs[sid].fields[*index].ty;
                    let value_ty = self.program.types.llvm_field(field_id);
                    let val = if field_id == self.program.types.unit() {
                        "zeroinitializer".into()
                    } else {
                        self.operand(value)
                    };
                    self.lines
                        .push(format!("  store {value_ty} {val}, ptr {gep}"));
                }
                Instruction::Eval(rv) => {
                    self.value(rv, None);
                }
            }
        }
        match bb
            .terminator
            .as_ref()
            .expect("verified MIR has terminators")
        {
            Terminator::Goto(b) => self.lines.push(format!("  br label %bb{b}")),
            Terminator::Branch {
                condition,
                then_bb,
                else_bb,
            } => {
                let v = self.operand(condition);
                self.lines.push(format!(
                    "  br i1 {v}, label %bb{then_bb}, label %bb{else_bb}"
                ));
            }
            Terminator::Return(value) => match value {
                Some(value) => {
                    let val = self.operand(value);
                    self.lines
                        .push(format!("  ret {} {val}", self.ty(self.func.result)));
                }
                None => self.lines.push("  ret void".into()),
            },
            Terminator::Unreachable => self.lines.push("  unreachable".into()),
        }
    }
    fn emit(mut self) -> (String, bool) {
        let f = self.func;
        let params = f
            .params
            .iter()
            .enumerate()
            .map(|(i, id)| format!("{} %arg{i}", self.ty(f.locals[*id])))
            .collect::<Vec<_>>()
            .join(", ");
        let mut output = format!(
            "define {} @{}({params}) {{\nentry:\n",
            self.ty(f.result),
            f.name
        );
        for (id, ty) in f.locals.iter().enumerate() {
            output.push_str(&format!("  %slot{id} = alloca {}\n", self.ty(*ty)));
        }
        for (i, id) in f.params.iter().enumerate() {
            output.push_str(&format!(
                "  store {} %arg{i}, ptr %slot{id}\n",
                self.ty(f.locals[*id])
            ));
        }
        output.push_str("  br label %bb0\n");
        for (i, b) in f.blocks.iter().enumerate() {
            self.lines.push(format!("bb{i}:"));
            self.block(b);
        }
        for line in self.lines {
            output.push_str(&line);
            output.push('\n');
        }
        output.push_str("}\n\n");
        (output, self.needs_trap)
    }
}
fn json_escape_helper(pool: &mut Strings) -> String {
    let fmt = pool.intern("\\u%04x");
    format!(
        r#"
; Escape UTF-8 bytes, JSON quotes and control bytes while streaming to stdout.
define private void @__nova_json_str(ptr %text) {{
entry:
  %q = call i32 @putchar(i32 34)
  br label %loop
loop:
  %idx = phi i64 [ 0, %entry ], [ %next, %cont ]
  %at = getelementptr i8, ptr %text, i64 %idx
  %char = load i8, ptr %at
  %end = icmp eq i8 %char, 0
  br i1 %end, label %finish, label %dispatch
dispatch:
  %next = add i64 %idx, 1
  %quote = icmp eq i8 %char, 34
  %slash = icmp eq i8 %char, 92
  %special = or i1 %quote, %slash
  br i1 %special, label %escaped, label %control
escaped:
  %p = call i32 @putchar(i32 92)
  %extended = zext i8 %char to i32
  %p2 = call i32 @putchar(i32 %extended)
  br label %cont
control:
  %is_n = icmp eq i8 %char, 10
  %is_r = icmp eq i8 %char, 13
  %is_t = icmp eq i8 %char, 9
  %is_low = icmp ult i8 %char, 32
  %is_named = or i1 %is_n, %is_r
  %is_named2 = or i1 %is_named, %is_t
  br i1 %is_named2, label %named, label %other
named:
  %escape = select i1 %is_n, i32 110, i32 114
  %escape2 = select i1 %is_t, i32 116, i32 %escape
  %b = call i32 @putchar(i32 92)
  %c = call i32 @putchar(i32 %escape2)
  br label %cont
other:
  br i1 %is_low, label %unicode, label %plain
unicode:
  %control_v = zext i8 %char to i32
  %u = call i32 (ptr, ...) @printf(ptr {fmt}, i32 %control_v)
  br label %cont
plain:
  %plain_v = zext i8 %char to i32
  %n = call i32 @putchar(i32 %plain_v)
  br label %cont
cont:
  br label %loop
finish:
  %close = call i32 @putchar(i32 34)
  ret void
}}
"#
    )
}
fn serializer(program: &mir::Program, sid: usize, pool: &mut Strings) -> String {
    let s = &program.types.structs[sid];
    let ty = format!("%struct.{}", s.name);
    let mut out = format!(
        "define i32 @{}_write_json({ty} %value) {{\nentry:\n",
        s.name
    );
    let open = pool.intern("{");
    out.push_str(&format!("  %o = call i32 (ptr, ...) @printf(ptr {open})\n"));
    for (i, field) in s.fields.iter().enumerate() {
        let prefix = pool.intern(&format!(
            "{}\"{}\":",
            if i == 0 { "" } else { "," },
            field.name
        ));
        out.push_str(&format!(
            "  %p{i} = call i32 (ptr, ...) @printf(ptr {prefix})\n"
        ));
        let field_ty = program.types.llvm(field.ty);
        out.push_str(&format!("  %f{i} = extractvalue {ty} %value, {i}\n"));
        match program.types.kind(field.ty) {
            TypeKind::Primitive(Primitive::Signed(bits)) => {
                let fmt = pool.intern(if bits == 64 { "%lld" } else { "%d" });
                let var = if bits < 32 {
                    out.push_str(&format!("  %ext{i} = sext {field_ty} %f{i} to i32\n"));
                    format!("i32 %ext{i}")
                } else {
                    format!("{field_ty} %f{i}")
                };
                out.push_str(&format!(
                    "  %s{i} = call i32 (ptr, ...) @printf(ptr {fmt}, {var})\n"
                ));
            }
            TypeKind::Primitive(Primitive::Unsigned(bits)) => {
                let fmt = pool.intern(if bits == 64 { "%llu" } else { "%u" });
                let var = if bits < 32 {
                    out.push_str(&format!("  %ext{i} = zext {field_ty} %f{i} to i32\n"));
                    format!("i32 %ext{i}")
                } else {
                    format!("{field_ty} %f{i}")
                };
                out.push_str(&format!(
                    "  %s{i} = call i32 (ptr, ...) @printf(ptr {fmt}, {var})\n"
                ));
            }
            TypeKind::Primitive(Primitive::Bool) => {
                let yes = pool.intern("true");
                let no = pool.intern("false");
                out.push_str(&format!(
                    "  %choose{i} = select i1 %f{i}, ptr {yes}, ptr {no}\n"
                ));
                out.push_str(&format!(
                    "  %s{i} = call i32 (ptr, ...) @printf(ptr %choose{i})\n"
                ));
            }
            TypeKind::Primitive(Primitive::Str) => {
                out.push_str(&format!("  call void @__nova_json_str(ptr %f{i})\n"));
            }
            TypeKind::Struct(nested) => {
                out.push_str(&format!(
                    "  %s{i} = call i32 @{}_write_json({field_ty} %f{i})\n",
                    program.types.structs[nested].name
                ));
            }
            _ => unreachable!("sema validates serializer fields"),
        }
    }
    let close = pool.intern("}");
    out.push_str(&format!(
        "  %end = call i32 (ptr, ...) @printf(ptr {close})\n  ret i32 0\n}}\n\n"
    ));
    out
}
// Derived `toJson` builds a string, instead of redirecting stdout. A private
// recursive appender is emitted for every serializable struct, so nested values
// are encoded into the SAME buffer (no intermediate process-lifetime strings).
fn json_string_serializer(program: &mir::Program, sid: usize, pool: &mut Strings) -> String {
    let s = &program.types.structs[sid];
    let ty = format!("%struct.{}", s.name);
    let append = format!("__nova_json_append_{}", s.name);
    let mut out = format!("define private void @{append}(i64 %builder, {ty} %value) {{\nentry:\n");
    let open = pool.intern("{");
    out.push_str(&format!(
        "  call void @__nova_json_builder_append(i64 %builder, ptr {open})\n"
    ));
    for (i, field) in s.fields.iter().enumerate() {
        let prefix = pool.intern(&format!(
            "{}\"{}\":",
            if i == 0 { "" } else { "," },
            field.name
        ));
        let fty = program.types.llvm(field.ty);
        out.push_str(&format!(
            "  call void @__nova_json_builder_append(i64 %builder, ptr {prefix})\n"
        ));
        out.push_str(&format!("  %f{i} = extractvalue {ty} %value, {i}\n"));
        match program.types.kind(field.ty) {
            TypeKind::Primitive(Primitive::Signed(bits)) => {
                let value = if bits < 64 {
                    out.push_str(&format!("  %w{i} = sext {fty} %f{i} to i64\n"));
                    format!("%w{i}")
                } else {
                    format!("%f{i}")
                };
                out.push_str(&format!(
                    "  call void @__nova_json_builder_i64(i64 %builder, i64 {value})\n"
                ));
            }
            TypeKind::Primitive(Primitive::Unsigned(bits)) => {
                let value = if bits < 64 {
                    out.push_str(&format!("  %w{i} = zext {fty} %f{i} to i64\n"));
                    format!("%w{i}")
                } else {
                    format!("%f{i}")
                };
                out.push_str(&format!(
                    "  call void @__nova_json_builder_u64(i64 %builder, i64 {value})\n"
                ));
            }
            TypeKind::Primitive(Primitive::Bool) => {
                out.push_str(&format!(
                    "  call void @__nova_json_builder_bool(i64 %builder, i1 %f{i})\n"
                ));
            }
            TypeKind::Primitive(Primitive::Str) => {
                out.push_str(&format!(
                    "  call void @__nova_json_builder_string(i64 %builder, ptr %f{i})\n"
                ));
            }
            TypeKind::Struct(nested) => {
                out.push_str(&format!(
                    "  call void @__nova_json_append_{}(i64 %builder, {fty} %f{i})\n",
                    program.types.structs[nested].name
                ));
            }
            _ => unreachable!("HIR validated JSON struct fields"),
        }
    }
    let close = pool.intern("}");
    out.push_str(&format!(
        "  call void @__nova_json_builder_append(i64 %builder, ptr {close})\n  ret void\n}}\n\n"
    ));
    out.push_str(&format!(
        "define ptr @{}_to_json({ty} %value) {{\nentry:\n",
        s.name
    ));
    out.push_str("  %builder = call i64 @__nova_json_builder_new()\n");
    out.push_str(&format!(
        "  call void @{append}(i64 %builder, {ty} %value)\n"
    ));
    out.push_str(
        "  %result = call ptr @__nova_json_builder_finish(i64 %builder)\n  ret ptr %result\n}\n\n",
    );
    out
}
// The public T_from_json(str)->T wrapper owns the temporary JSON DOM.
// A private decoder is reused recursively by nested structs. `str` fields are
// copied into runtime-managed, process-lifetime storage before the DOM is freed.
fn deserializer(program: &mir::Program, id: usize, pool: &mut Strings) -> String {
    let s = &program.types.structs[id];
    let ty = format!("%struct.{}", s.name);
    let decoder = format!("__nova_json_decode_{}", s.name);
    let mut out=format!("define private {ty} @{decoder}(ptr %node) {{\nentry:\n  call void @__nova_json_require_object(ptr %node)\n");
    for (i, field) in s.fields.iter().enumerate() {
        let label = pool.intern(&field.name);
        let fty = program.types.llvm(field.ty);
        out.push_str(&format!(
            "  %field{i} = call ptr @__nova_json_field(ptr %node, ptr {label})\n"
        ));
        let value = match program.types.kind(field.ty) {
            TypeKind::Primitive(Primitive::Signed(bits)) => {
                out.push_str(&format!(
                    "  %num{i} = call i64 @__nova_json_signed(ptr %field{i}, i32 {bits})\n"
                ));
                if bits == 64 {
                    format!("%num{i}")
                } else {
                    out.push_str(&format!("  %value{i} = trunc i64 %num{i} to {fty}\n"));
                    format!("%value{i}")
                }
            }
            TypeKind::Primitive(Primitive::Unsigned(bits)) => {
                out.push_str(&format!(
                    "  %num{i} = call i64 @__nova_json_unsigned(ptr %field{i}, i32 {bits})\n"
                ));
                if bits == 64 {
                    format!("%num{i}")
                } else {
                    out.push_str(&format!("  %value{i} = trunc i64 %num{i} to {fty}\n"));
                    format!("%value{i}")
                }
            }
            TypeKind::Primitive(Primitive::Bool) => {
                out.push_str(&format!(
                    "  %value{i} = call i1 @__nova_json_boolean(ptr %field{i})\n"
                ));
                format!("%value{i}")
            }
            TypeKind::Primitive(Primitive::Str) => {
                out.push_str(&format!(
                    "  %value{i} = call ptr @__nova_json_string(ptr %field{i})\n"
                ));
                format!("%value{i}")
            }
            TypeKind::Struct(nested) => {
                out.push_str(&format!(
                    "  %value{i} = call {fty} @__nova_json_decode_{}(ptr %field{i})\n",
                    program.types.structs[nested].name
                ));
                format!("%value{i}")
            }
            _ => unreachable!("HIR validates JSON fields"),
        };
        let previous = if i == 0 {
            "zeroinitializer".to_string()
        } else {
            format!("%aggregate{}", i - 1)
        };
        out.push_str(&format!(
            "  %aggregate{i} = insertvalue {ty} {previous}, {fty} {value}, {i}\n"
        ));
    }
    let value = if s.fields.is_empty() {
        "zeroinitializer".into()
    } else {
        format!("%aggregate{}", s.fields.len() - 1)
    };
    out.push_str(&format!("  ret {ty} {value}\n}}\n\n"));
    out.push_str(&format!(
        "define {ty} @{}_from_json(ptr %text) {{\nentry:\n",
        s.name
    ));
    out.push_str(&format!(
        "  %root = call ptr @__nova_json_parse(ptr %text)\n"
    ));
    out.push_str(&format!("  %value = call {ty} @{decoder}(ptr %root)\n"));
    out.push_str("  call void @__nova_json_free(ptr %root)\n");
    out.push_str(&format!("  ret {ty} %value\n}}\n\n"));
    out
}

// LLVM definitions emitted by Nova use their original surface-lowered name.
// Only genuine external calls cross the libc / private-STD ABI boundary.
// A coroutine factory is represented as External in MIR, but its adapter is
// actually defined in this module, so it also keeps its Nova symbol.
fn function_link_symbol<'a>(
    program: &mir::Program,
    function: &'a mir::Function,
) -> std::borrow::Cow<'a, str> {
    if matches!(function.kind, FunctionKind::External)
        && !crate::coroutine_codegen::is_factory(program, &function.name)
    {
        c_link_symbol(&function.name)
    } else {
        std::borrow::Cow::Borrowed(&function.name)
    }
}
// `std::abi::native::libc::<header>::name` resolves directly to libc's ELF symbol `name`.
// Standard-library external entrypoints use a distinct private ABI namespace.
// Native C symbols are implementation details, not names exposed by Nova APIs.
// Ordinary Nova functions and libc imports retain their normal link symbols.
fn c_link_symbol(name: &str) -> std::borrow::Cow<'_, str> {
    // The built-in STD has one global C ABI, regardless of which Nova package
    // loaded it. For example, the dependency loader can emit the same extern
    // as `std__abi__native__net__native_tcp_close` or as
    // `nexo__std__abi__native__net__native_tcp_close`. Both must use the same
    // private runtime symbol after dependency alias normalization.
    //
    // Only normalize complete namespace components; an unrelated name like
    // `my_std__old__vec__native_*` is not a reference to the built-in STD.
    let std_name = if name.starts_with("std__") {
        Some(name)
    } else {
        name.find("__std__").map(|start| &name[start + 2..])
    };
    if let Some(std_name) = std_name {
        // ABI modules are physically under std/abi/. Do not change the
        // existing C symbol names: __nova_std__old__raw____..., libc printf, etc.
        // Nested package aliases are canonicalized before this point.
        let std_name = if let Some(rest) = std_name.strip_prefix("std__abi__native__") {
            std::borrow::Cow::Owned(format!("std__{rest}"))
        } else {
            std::borrow::Cow::Borrowed(std_name)
        };
        let std_name = std_name.as_ref();
        // `std::abi::native::libc::<header>::symbol` always uses the system C ABI,
        // including when imported through a package's nested STD namespace.
        if let Some(rest) = std_name.strip_prefix("std__libc__") {
            if let Some((header, symbol)) = rest.split_once("__") {
                if header == "errno" && symbol == "errno_location" {
                    return std::borrow::Cow::Borrowed("__errno_location");
                }
                return std::borrow::Cow::Owned(symbol.to_owned());
            }
        }
        // Atomic entrypoints are an explicitly reviewed native C ABI. Keep
        // their declared leaf symbols so runtime/nova_atomics.c can export
        // the same names and the build's lazy-runtime detector can see them.
        if let Some(symbol) = std_name.strip_prefix("std__concurrency__atomic__") {
            if symbol.starts_with("__nova_atomic_") {
                // `symbol` is borrowed from the locally normalized `std_name`
                // Cow above. Return an owned value so this branch does not
                // leak a borrow tied to that local temporary.
                return std::borrow::Cow::Owned(symbol.to_owned());
            }
        }
        if let Some(rest) = std_name.strip_prefix("std__sdl2__") {
            return std::borrow::Cow::Owned(rest.replace("__", "_"));
        }
        if let Some(rest) = std_name.strip_prefix("std__opengl__") {
            return std::borrow::Cow::Owned(rest.to_owned());
        }
        // Runtime symbols are not package-local. Do not prepend `nexo__`.
        if let Some((module, operation)) = std_name.rsplit_once("__") {
            if let Some(operation) = operation.strip_prefix("native_") {
                return std::borrow::Cow::Owned(format!("__nova_{module}____{operation}"));
            }
        }
        // Keep the pre-existing mapping for other `extern fn` in the STD.
        return std::borrow::Cow::Owned(format!("__nova_{std_name}"));
    }
    // User-defined externs keep their declared linkage name.
    std::borrow::Cow::Borrowed(name)
}

// Atomic operations cross a small, compiler-reviewed native C ABI. The
// standard library owns the typed API and memory-ordering policy; the C
// adapter owns target-specific atomic lowering and lock-free queries.
// Delimit each built-in so unused operations never enter the object file.
/// Return the MIR function ids that can be reached from the executable entry
/// point. Calls and function references are the only edges needed here because
/// MIR has already resolved overloads, methods and generic specializations.
///
/// If there is no `main` (for example, a compiler-only codegen test), all
/// functions are retained so the standalone backend API keeps its historical
/// behavior.
fn reachable_functions(program: &mir::Program) -> std::collections::HashSet<usize> {
    use std::collections::{HashMap, HashSet};
    let Some(root) = program.functions.iter().position(|function| {
        function.name == "main" && matches!(function.kind, FunctionKind::Defined)
    }) else {
        return (0..program.functions.len()).collect();
    };
    let by_name: HashMap<&str, usize> = program
        .functions
        .iter()
        .enumerate()
        .map(|(id, function)| (function.name.as_str(), id))
        .collect();
    let mut live = HashSet::from([root]);
    let mut work = vec![root];
    while let Some(id) = work.pop() {
        let function = &program.functions[id];
        for block in &function.blocks {
            for instruction in &block.instructions {
                let rvalue = match instruction {
                    mir::Instruction::Assign { value, .. } | mir::Instruction::Eval(value) => value,
                    mir::Instruction::StoreField { .. } | mir::Instruction::StoreDeref { .. } => {
                        continue
                    }
                };
                if let mir::Rvalue::DynObjectRef(value, trait_name) = rvalue {
                    let Some(source) = mir::operand_ty(value, function, &program.types).ok() else {
                        continue;
                    };
                    let Some(concrete) = (match program.types.kind(source) {
                        TypeKind::Reference { element, .. } | TypeKind::Pointer { element, .. } => {
                            Some(element)
                        }
                        _ => None,
                    }) else {
                        continue;
                    };
                    for candidate in &program.functions {
                        let Some((trait_key, _, method)) = dyn_method_info(&candidate.name) else {
                            continue;
                        };
                        if trait_key != dyn_name_part(trait_name) {
                            continue;
                        }
                        if let Some(implementation) = dyn_impl(program, concrete, &method) {
                            if let Some(target) = by_name.get(implementation.name.as_str()).copied()
                            {
                                if live.insert(target) {
                                    work.push(target);
                                }
                            }
                        }
                    }
                }
                let dependency = match rvalue {
                    mir::Rvalue::Call(target, ..) | mir::Rvalue::FnRef(target, ..) => Some(*target),
                    mir::Rvalue::CallIndirect(_, ..)
                    | mir::Rvalue::Use(_)
                    | mir::Rvalue::Unary(_, _)
                    | mir::Rvalue::Binary(_, _, _)
                    | mir::Rvalue::StructInit(_, _)
                    | mir::Rvalue::Field(_, _)
                    | mir::Rvalue::Array(_)
                    | mir::Rvalue::Index(_, _)
                    | mir::Rvalue::AddressOf(_)
                    | mir::Rvalue::Deref(_)
                    | mir::Rvalue::DynObjectRef(_, _)
                    | mir::Rvalue::Cast(_, _) => None,
                };
                if let Some(target) = dependency {
                    if live.insert(target) {
                        work.push(target);
                    }
                }
            }
        }
        // Coroutine factories are declared as external in MIR, but their
        // adapter calls the generated body by symbol name rather than through
        // an MIR Call edge.
        if crate::coroutine_codegen::is_factory(program, &function.name) {
            let body = function
                .name
                .strip_prefix("__nova_make_")
                .map(|name| format!("__nova_body_{name}"));
            if let Some(body) = body.and_then(|name| by_name.get(name.as_str()).copied()) {
                if live.insert(body) {
                    work.push(body);
                }
            }
        }
    }
    // Generated JSON entrypoints contain direct symbol calls that are emitted
    // by the serializer backend rather than represented as MIR instructions.
    // Once one generated JSON function is live, retain the generated JSON
    // family so nested serializers/readers remain linkable.
    if live.iter().any(|id| {
        matches!(
            program.functions[*id].kind,
            FunctionKind::JsonWriter(_)
                | FunctionKind::JsonReader(_)
                | FunctionKind::JsonToString(_)
        )
    }) {
        for (id, function) in program.functions.iter().enumerate() {
            if matches!(
                function.kind,
                FunctionKind::JsonWriter(_)
                    | FunctionKind::JsonReader(_)
                    | FunctionKind::JsonToString(_)
            ) {
                live.insert(id);
            }
        }
    }
    live
}

/// Generate all functions. This remains the backend's compatibility API for
/// compiler tests and tools that intentionally inspect unreachable symbols.
pub fn generate(program: &mir::Program) -> String {
    generate_impl(program, None)
}

/// Generate only functions reachable from `main`, unless explicitly disabled.
pub fn generate_reachable(program: &mir::Program, keep_all_functions: bool) -> String {
    let live = (!keep_all_functions).then(|| reachable_functions(program));
    generate_impl(program, live.as_ref())
}

fn generate_impl(
    program: &mir::Program,
    live: Option<&std::collections::HashSet<usize>>,
) -> String {
    let emit = |id: usize| live.map_or(true, |set| set.contains(&id));
    let mut pool = Strings::default();
    let mut functions = String::new();
    let mut need_trap = false;
    // Dependencies may import the same built-in STD under distinct Nova names
    // (e.g. std__string and nexo__std__string). After ABI mapping, both
    // resolve to one C symbol. LLVM allows only one extern declaration per
    // symbol, so deduplicate by *link symbol*, not by Nova function name.
    // Keep the ABI signatures to reject incompatible collisions explicitly.
    let mut external_abi: HashMap<String, (String, Vec<String>)> = HashMap::new();
    let json = program.functions.iter().enumerate().any(|(id, f)| {
        emit(id)
            && matches!(
                f.kind,
                FunctionKind::JsonWriter(_)
                    | FunctionKind::JsonReader(_)
                    | FunctionKind::JsonToString(_)
            )
    });
    for (id, f) in program.functions.iter().enumerate() {
        if !emit(id) {
            continue;
        }
        match f.kind {
            FunctionKind::External if crate::coroutine_codegen::is_factory(program, &f.name) => {
                functions.push_str(
                    &crate::coroutine_codegen::adapter(program, f)
                        .expect("verified coroutine trampoline"),
                );
            }
            FunctionKind::CompilerIntrinsic(_) => {
                // Compiler intrinsics have no declaration and no body. Their
                // Rvalue::Call is emitted inline by Emitter::value.
            }
            FunctionKind::DynMethod { .. } => {
                // This is a vtable slot descriptor, not a callable body.
            }
            FunctionKind::External => {
                let symbol = function_link_symbol(program, f).into_owned();
                if f.name == "__nova_internal_byte_offset" {
                    continue; // emitted inline, never declare a C symbol
                }
                if f.name.starts_with("__nova_internal_") {
                    continue; // compiler-owned intrinsic, emitted inline
                }
                let result = program.types.llvm(f.result);
                let mut types = f
                    .params
                    .iter()
                    .map(|p| program.types.llvm(f.locals[*p]))
                    .collect::<Vec<_>>();
                if f.c_variadic {
                    types.push("...".into());
                }
                if let Some((previous_result, previous_types)) = external_abi.get(&symbol) {
                    assert_eq!(
                        (previous_result, previous_types),
                        (&result, &types),
                        "conflicting C ABI declarations for '{symbol}' (Nova alias '{}')",
                        f.name
                    );
                } else {
                    functions.push_str(&format!(
                        "declare {result} @{symbol}({})\n",
                        types.join(", ")
                    ));
                    external_abi.insert(symbol, (result, types));
                }
            }
            FunctionKind::LlvmIntrinsic(kind) => {
                if kind.is_atomic() {
                    need_trap = true;
                }
                functions.push_str(&kind.definition(&f.name));
            }
            FunctionKind::Defined => {
                let emitter = Emitter {
                    program,
                    func: f,
                    pool: &mut pool,
                    next: 0,
                    lines: Vec::new(),
                    needs_trap: false,
                };
                let (body, trap) = emitter.emit();
                functions.push_str(&body);
                need_trap |= trap;
            }
            FunctionKind::JsonWriter(id) => functions.push_str(&serializer(program, id, &mut pool)),
            FunctionKind::JsonReader(id) => {
                functions.push_str(&deserializer(program, id, &mut pool))
            }
            FunctionKind::JsonToString(id) => {
                functions.push_str(&json_string_serializer(program, id, &mut pool))
            }
        }
    }
    if json {
        functions.push_str(&json_escape_helper(&mut pool));
    }
    // LLVM declarations are not extern Nova functions or C runtime ABI.
    // Deduplicate declarations, including aliases imported by other packages.
    let mut llvm_builtin_decls = String::new();
    let mut llvm_intrinsics_seen = std::collections::HashSet::new();
    // These are also emitted by legacy compiler intrinsics used by generated
    // String/drop code. Keep them approved, but emit each ABI declaration once.
    let mut llvm_libc_declarations_seen =
        std::collections::HashSet::from(["aligned_alloc".to_owned(), "free".to_owned()]);
    for (id, f) in program.functions.iter().enumerate() {
        if !emit(id) {
            continue;
        }
        if let FunctionKind::LlvmIntrinsic(kind) = f.kind {
            if !llvm_intrinsics_seen.insert(kind.label()) {
                continue;
            }
            // Several distinct intrinsics can legitimately reference the
            // same libc symbol, e.g. typed and untyped pthread_create.
            // Deduplicate EACH declaration by ABI symbol name.
            for declaration in kind.declaration().lines() {
                let c_symbol = declaration
                    .split_once('@')
                    .and_then(|(_, tail)| tail.split_once('('))
                    .map(|(name, _)| name);
                if c_symbol.is_some_and(|name| {
                    external_abi.contains_key(name)
                        || !llvm_libc_declarations_seen.insert(name.to_owned())
                }) {
                    continue;
                }
                llvm_builtin_decls.push_str(declaration);
                llvm_builtin_decls.push('\n');
            }
        }
    }
    for function in &program.functions {
        let declaration = match function.kind {
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::CopyBytes) =>
                Some("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::MoveBytes) =>
                Some("declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::SetBytes) =>
                Some("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::HeapAllocate) =>
                Some("declare ptr @aligned_alloc(i64, i64)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::HeapFree) =>
                Some("declare void @free(ptr)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::TextLength) =>
                Some("define linkonce_odr i64 @__nova_internal_text_length(ptr %text) {\nentry:\n  br label %test\ntest:\n  %index = phi i64 [0, %entry], [%next, %step]\n  %address = getelementptr i8, ptr %text, i64 %index\n  %byte = load i8, ptr %address, align 1\n  %zero = icmp eq i8 %byte, 0\n  br i1 %zero, label %finished, label %step\nstep:\n  %next = add i64 %index, 1\n  br label %test\nfinished:\n  ret i64 %index\n}\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::SqrtF64) =>
                Some("declare double @llvm.sqrt.f64(double)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::AbsF64) =>
                Some("declare double @llvm.fabs.f64(double)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::FloorF64) =>
                Some("declare double @llvm.floor.f64(double)\n"),
            FunctionKind::CompilerIntrinsic(crate::intrinsics::Intrinsic::CeilF64) =>
                Some("declare double @llvm.ceil.f64(double)\n"),
            _ => None,
        };
        if let Some(declaration) = declaration {
            llvm_builtin_decls.push_str(declaration);
        }
    }
    let mut out =
        String::from("; Nova compiler: checked MIR -> LLVM IR\nsource_filename = \"nova\"\n\n");
    out.push_str(&program.types.llvm_declarations());
    out.push_str(&dyn_vtables(program));
    out.push_str(&pool.definitions());
    out.push_str(&llvm_builtin_decls);
    let has_c_symbol = |symbol: &str| {
        program.functions.iter().enumerate().any(|(id, f)| {
            emit(id) && matches!(f.kind, FunctionKind::External) && c_link_symbol(&f.name) == symbol
        })
    };
    if json {
        if !has_c_symbol("printf") {
            out.push_str("declare i32 @printf(ptr, ...)\n");
        }
        if !has_c_symbol("putchar") {
            out.push_str("declare i32 @putchar(i32)\n");
        }
    }
    if json {
        out.push_str("declare i64 @__nova_json_builder_new()\n");
        out.push_str("declare void @__nova_json_builder_append(i64,ptr)\n");
        out.push_str("declare void @__nova_json_builder_i64(i64,i64)\n");
        out.push_str("declare void @__nova_json_builder_u64(i64,i64)\n");
        out.push_str("declare void @__nova_json_builder_bool(i64,i1)\n");
        out.push_str("declare void @__nova_json_builder_string(i64,ptr)\n");
        out.push_str("declare ptr @__nova_json_builder_finish(i64)\n");
    }
    if program.types.structs.iter().any(|s| s.deserialize) {
        out.push_str("declare ptr @__nova_json_parse(ptr)\n");
        out.push_str("declare void @__nova_json_free(ptr)\n");
        out.push_str("declare void @__nova_json_require_object(ptr)\n");
        out.push_str("declare ptr @__nova_json_field(ptr,ptr)\n");
        out.push_str("declare i64 @__nova_json_signed(ptr,i32)\n");
        out.push_str("declare i64 @__nova_json_unsigned(ptr,i32)\n");
        out.push_str("declare i1 @__nova_json_boolean(ptr)\n");
        out.push_str("declare ptr @__nova_json_string(ptr)\n");
    }
    if need_trap {
        out.push_str("declare void @llvm.trap()\n");
    }
    if program
        .functions
        .iter()
        .enumerate()
        .any(|(id, f)| emit(id) && crate::coroutine_codegen::is_factory(program, &f.name))
    {
        if !has_c_symbol("malloc") {
            out.push_str("declare ptr @malloc(i64)\n");
        }
        out.push_str("declare i64 @__nova_coro_create(ptr, ptr)\n");
    }
    out.push('\n');
    out.push_str(&functions);
    out
}

#[cfg(test)]
mod std_linkage_regression_tests {
    use std::path::Path;

    #[test]
    fn reachable_codegen_discards_unreferenced_functions_by_default() {
        let source = "fn live()->i32{return 41;} fn dead()->i32{return 99;} fn main()->i32{return live()+1;}";
        let parsed = crate::grammar::ProgramParser::new()
            .parse(&crate::strip_comments(source))
            .expect("parse reachability fixture");
        let surface = crate::surface::lower(&parsed).expect("lower reachability fixture");
        let hir = crate::hir::lower(&surface).expect("type-check reachability fixture");
        let mir = crate::mir::lower(&hir).expect("lower reachability fixture to MIR");
        let compact = super::generate_reachable(&mir, false);
        assert!(compact.contains("define i32 @live("), "{compact}");
        assert!(compact.contains("define i32 @main("), "{compact}");
        assert!(!compact.contains("define i32 @dead("), "{compact}");
        let preserved = super::generate_reachable(&mir, true);
        assert!(preserved.contains("define i32 @dead("), "{preserved}");
    }

    #[test]
    fn nested_builtin_std_external_abi_is_global() {
        // Calls AND extern declarations use c_link_symbol through the same
        // function_link_symbol/codegen path. Nested dependency namespaces may
        // not leak into the C symbol name.
        assert_eq!(
            super::c_link_symbol("vendor__nexo__std__abi__native__net__native_tcp_close"),
            "__nova_std__net____tcp_close"
        );
        assert_eq!(
            super::c_link_symbol("nexo__std__abi__native__libc__stdio__printf"),
            "printf"
        );
        assert_eq!(
            super::c_link_symbol("nexo__std__abi__native__libc__errno__errno_location"),
            "__errno_location"
        );
        assert_eq!(
            super::c_link_symbol("std__concurrency__atomic____nova_atomic_bool_load"),
            "__nova_atomic_bool_load"
        );
        assert_eq!(
            super::c_link_symbol("std__concurrency__atomic____nova_atomic_i64_compare_exchange"),
            "__nova_atomic_i64_compare_exchange"
        );
        assert_eq!(
            super::c_link_symbol("some__native_user_entry"),
            "some__native_user_entry"
        );
        assert_eq!(
            super::c_link_symbol("my_std__old__vec__native_unrelated"),
            "my_std__old__vec__native_unrelated"
        );
    }

    #[test]
    fn nexo_async_live_emits_real_runtime_symbols() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/packages/nexo_async_live");
        let ast = crate::package::load(&root).expect("load Nexo package");
        let surface = crate::surface::lower(&ast).expect("lower Nexo surface");
        let hir = crate::hir::lower(&surface).expect("type-check Nexo");
        let mir = crate::mir::lower(&hir).expect("verify Nexo MIR");
        let llvm = super::generate(&mir);
        assert!(
            llvm.contains("define i64 @std__abi__llvm__pointer_offset("),
            "Nexo LLVM symbol or ABI contract missing"
        );
        assert!(
            llvm.contains("@std__abi__llvm__close_fd("),
            "Nexo LLVM symbol or ABI contract missing"
        );
        assert!(
            !llvm.contains("@nexo__std__abi__native__raw__native_byte_offset("),
            "Nexo LLVM symbol or ABI contract missing"
        );
        assert!(
            !llvm.contains("@nexo__std__abi__native__net__native_tcp_close("),
            "Nexo LLVM symbol or ABI contract missing"
        );
        // Both the application and the Nexo dependency import the built-in
        // STD. Their extern declarations must still be unique after ABI
        // mapping, or Clang rejects the IR as an invalid redefinition.
        let mut declarations = std::collections::HashSet::new();
        for line in llvm.lines().filter(|line| line.starts_with("declare ")) {
            let symbol = line
                .split('@')
                .nth(1)
                .and_then(|value| value.split('(').next())
                .expect("LLVM extern declaration with a symbol");
            assert!(
                declarations.insert(symbol.to_string()),
                "duplicate LLVM extern declaration for {symbol}: {line}"
            );
        }
        assert_eq!(
            llvm.matches("define %struct.std__old__string__String @std__old__string__from_text(")
                .count(),
            1
        );
        // A wait() on a task inside Nexo's async server would attempt a
        // blocking scheduler operation while a coroutine is active.
        assert!(
            llvm.contains("call i64 @__nova_std__old__task____await_value("),
            "Nexo must cooperatively await pending socket retries"
        );
        // The compiler now loads one canonical std for the entire package
        // graph. Its RAII methods are defined only once under std::, not
        // copied under a transitive `nexo::std` namespace.
        assert!(
            llvm.contains("define void @std__old__vec__ByteVector__drop("),
            "Nexo LLVM symbol or ABI contract missing"
        );
        assert!(
            !llvm.contains("define void @nexo__std__old__vec__ByteVector__drop("),
            "nested std must not define another destructor: {llvm}"
        );
    }

    #[test]
    fn std_defined_raii_destructors_keep_their_llvm_definition_names() {
        // This package owns String, StringBuilder, SymbolTable and ByteBuffer;
        // a use-after-scope cleanup must call each Nova-defined drop method.
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/packages/stdlib_contracts_017");
        let llvm = std::thread::Builder::new()
            .name("codegen-stdlib-contracts".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let ast = crate::package::load(&root).expect("load stdlib owner fixture");
                let surface = crate::surface::lower(&ast).expect("surface specialization");
                let hir = crate::hir::lower(&surface).expect("RAII lowering");
                let mir = crate::mir::lower(&hir).expect("verified MIR");
                super::generate(&mir)
            })
            .expect("spawn stdlib contract test")
            .join()
            .expect("stdlib contract test thread");

        // The generated definition and every call must use the same symbol.
        assert!(
            llvm.contains("define void @std__old__builder__StringBuilder__drop("),
            "Nova-owned destructor or String contract missing"
        );
        assert!(
            llvm.contains("call void @std__old__builder__StringBuilder__drop("),
            "Nova-owned destructor or String contract missing"
        );
        assert!(
            !llvm.contains("@__nova_std__old__builder__StringBuilder__drop("),
            "Nova-owned destructor or String contract missing"
        );
        assert!(
            llvm.contains("define %struct.std__old__string__String @std__old__string__from_text("),
            "Nova-owned destructor or String contract missing"
        );
        assert!(
            llvm.contains("call %struct.std__old__string__String @std__old__string__from_text("),
            "Nova-owned destructor or String contract missing"
        );

        // String's owning destructor is written in Nova, not a legacy C handle.
        assert!(
            llvm.contains("call void @std__old__string__String__drop("),
            "missing call to Nova String destructor"
        );
        assert!(
            llvm.contains("call void @std__old__raw__deallocate_bytes("),
            "missing Nova-owned allocation cleanup"
        );
        assert!(
            !llvm.contains("@__nova_std__old__string____string_destroy("),
            "legacy native string handle reintroduced"
        );
    }
}
