//! LLVM IR text backend. The IR is verified before reaching this module.
use crate::{
    ast::{BinaryOp, UnaryOp},
    hir::FunctionKind,
    mir::{self, Instruction, Operand, Rvalue, Terminator},
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
            Operand::Str(s) => self.pool.intern(s),
            Operand::Unit => "".into(),
            Operand::Zero(_) => "zeroinitializer".into(),
        }
    }
    fn value(&mut self, rv: &Rvalue, target: Option<TypeId>) -> Option<String> {
        match rv {
            Rvalue::Use(op) => Some(self.operand(op)),
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
                    let val = self.operand(operand);
                    let field_ty = self.ty(self.program.types.structs[*id].fields[index].ty);
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
                let ty = self.ty(array_ty);
                let TypeKind::Array { len, element } = self.program.types.kind(array_ty) else {
                    unreachable!()
                };
                let val = self.operand(array);
                let index_val = self.operand(index);
                let valid = self.temp();
                self.lines
                    .push(format!("  {valid} = icmp ult i32 {index_val}, {len}"));
                let unique = self.next;
                self.next += 1;
                self.lines.push(format!(
                    "  br i1 {valid}, label %index.ok{unique}, label %index.trap{unique}"
                ));
                self.lines.push(format!("index.trap{unique}:"));
                self.lines.push("  call void @llvm.trap()".into());
                self.lines.push("  unreachable".into());
                self.lines.push(format!("index.ok{unique}:"));
                let stack = self.temp();
                self.lines.push(format!("  {stack} = alloca {ty}"));
                self.lines.push(format!("  store {ty} {val}, ptr {stack}"));
                let gep = self.temp();
                self.lines.push(format!(
                    "  {gep} = getelementptr inbounds {ty}, ptr {stack}, i32 0, i32 {index_val}"
                ));
                let dst = self.temp();
                self.lines
                    .push(format!("  {dst} = load {}, ptr {gep}", self.ty(element)));
                self.needs_trap = true;
                Some(dst)
            }
            Rvalue::AddressOf(local) => Some(format!("%slot{local}")),
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
                    let value_ty = self.ty(self.program.types.structs[sid].fields[*index].ty);
                    let val = self.operand(value);
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

// `std::libc::<header>::name` resolves directly to libc's ELF symbol `name`.
// Standard-library external entrypoints use a distinct private ABI namespace.
// Native C symbols are implementation details, not names exposed by Nova APIs.
// Ordinary Nova functions and libc imports retain their normal link symbols.
fn c_link_symbol(name: &str) -> std::borrow::Cow<'_, str> {
    // The built-in STD has one global C ABI, regardless of which Nova package
    // loaded it. For example, the dependency loader can emit the same extern
    // as `std__vec__native_byte_vector_append` or as
    // `nexo__std__vec__native_byte_vector_append`. Both must link to
    // `__nova_std__vec____byte_vector_append` in runtime/nova_rt.c.
    //
    // Only normalize complete namespace components; an unrelated name like
    // `my_std__vec__native_*` is not a reference to the built-in STD.
    let std_name = if name.starts_with("std__") {
        Some(name)
    } else {
        name.find("__std__").map(|start| &name[start + 2..])
    };
    if let Some(std_name) = std_name {
        // `std::libc::<header>::symbol` always uses the system C ABI,
        // including when imported through a package's nested STD namespace.
        if let Some(rest) = std_name.strip_prefix("std__libc__") {
            if let Some((header, symbol)) = rest.split_once("__") {
                if header == "errno" && symbol == "errno_location" {
                    return std::borrow::Cow::Borrowed("__errno_location");
                }
                return std::borrow::Cow::Borrowed(symbol);
            }
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

pub fn generate(program: &mir::Program) -> String {
    let mut pool = Strings::default();
    let mut functions = String::new();
    let mut need_trap = false;
    // Dependencies may import the same built-in STD under distinct Nova names
    // (e.g. std__string and nexo__std__string). After ABI mapping, both
    // resolve to one C symbol. LLVM allows only one extern declaration per
    // symbol, so deduplicate by *link symbol*, not by Nova function name.
    // Keep the ABI signatures to reject incompatible collisions explicitly.
    let mut external_abi: HashMap<String, (String, Vec<String>)> = HashMap::new();
    let json = program.types.structs.iter().any(|s| s.serialize);
    for f in &program.functions {
        match f.kind {
            FunctionKind::External if crate::coroutine_codegen::is_factory(program, &f.name) => {
                functions.push_str(
                    &crate::coroutine_codegen::adapter(program, f)
                        .expect("verified coroutine trampoline"),
                );
            }
            FunctionKind::External => {
                let symbol = function_link_symbol(program, f).into_owned();
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
    let mut out =
        String::from("; Nova compiler: checked MIR -> LLVM IR\nsource_filename = \"nova\"\n\n");
    out.push_str(&program.types.llvm_declarations());
    out.push_str(&pool.definitions());
    let has_c_symbol = |symbol: &str| {
        program
            .functions
            .iter()
            .any(|f| matches!(f.kind, FunctionKind::External) && c_link_symbol(&f.name) == symbol)
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
        .any(|f| crate::coroutine_codegen::is_factory(program, &f.name))
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
    fn nested_builtin_std_external_abi_is_global() {
        // Calls AND extern declarations use c_link_symbol through the same
        // function_link_symbol/codegen path. Nested dependency namespaces may
        // not leak into the C symbol name.
        assert_eq!(
            super::c_link_symbol("std__vec__native_byte_vector_append"),
            "__nova_std__vec____byte_vector_append"
        );
        assert_eq!(
            super::c_link_symbol("nexo__std__vec__native_byte_vector_append"),
            "__nova_std__vec____byte_vector_append"
        );
        assert_eq!(
            super::c_link_symbol("vendor__nexo__std__net__native_tcp_close"),
            "__nova_std__net____tcp_close"
        );
        assert_eq!(
            super::c_link_symbol("nexo__std__libc__stdio__printf"),
            "printf"
        );
        assert_eq!(
            super::c_link_symbol("nexo__std__libc__errno__errno_location"),
            "__errno_location"
        );
        assert_eq!(
            super::c_link_symbol("some__native_user_entry"),
            "some__native_user_entry"
        );
        assert_eq!(
            super::c_link_symbol("my_std__vec__native_unrelated"),
            "my_std__vec__native_unrelated"
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
            llvm.contains("@__nova_std__vec____byte_vector_append("),
            "{llvm}"
        );
        assert!(llvm.contains("@__nova_std__net____tcp_close("), "{llvm}");
        assert!(
            !llvm.contains("@nexo__std__vec__native_byte_vector_append("),
            "{llvm}"
        );
        assert!(
            !llvm.contains("@nexo__std__net__native_tcp_close("),
            "{llvm}"
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
            llvm.matches("declare i64 @__nova_std__string____string_create_from_text(ptr)")
                .count(),
            1
        );
        // A wait() on a task inside Nexo's async server would attempt a
        // blocking scheduler operation while a coroutine is active.
        assert!(
            llvm.contains("call i64 @__nova_std__task____await_value("),
            "Nexo must cooperatively await pending socket retries"
        );
        // Defined Nova functions, including RAII drops, must retain their names.
        assert!(
            llvm.contains("define void @nexo__std__vec__ByteVector__drop("),
            "{llvm}"
        );
    }

    #[test]
    fn std_defined_raii_destructors_keep_their_llvm_definition_names() {
        // This package owns String, StringBuilder, SymbolTable and ByteBuffer;
        // a use-after-scope cleanup must call each Nova-defined drop method.
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/packages/stdlib_contracts_017");
        let ast = crate::package::load(&root).expect("load stdlib owner fixture");
        let surface = crate::surface::lower(&ast).expect("surface specialization");
        let hir = crate::hir::lower(&surface).expect("RAII lowering");
        let mir = crate::mir::lower(&hir).expect("verified MIR");
        let llvm = super::generate(&mir);

        // The generated definition and every call must use the same symbol.
        assert!(
            llvm.contains("define void @std__builder__StringBuilder__drop("),
            "{llvm}"
        );
        assert!(
            llvm.contains("call void @std__builder__StringBuilder__drop("),
            "{llvm}"
        );
        assert!(
            !llvm.contains("@__nova_std__builder__StringBuilder__drop("),
            "{llvm}"
        );
        assert!(
            llvm.contains("define %struct.std__string__String @std__string__from_text("),
            "{llvm}"
        );
        assert!(
            llvm.contains("call %struct.std__string__String @std__string__from_text("),
            "{llvm}"
        );

        // The native C runtime is a different ABI and must remain prefixed.
        assert!(
            llvm.contains("@__nova_std__string____string_destroy("),
            "{llvm}"
        );
    }
}
