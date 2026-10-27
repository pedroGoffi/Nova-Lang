//! Interned semantic type system. TypeId is stable for the duration of a compilation.
use crate::ast::TypeRef;
use std::collections::HashMap;

pub type TypeId = usize;
pub type StructId = usize;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    Unit,
    Bool,
    Signed(u8),
    Unsigned(u8),
    Float(u8),
    Str,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeKind {
    Primitive(Primitive),
    Struct(StructId),
    Array { element: TypeId, len: usize },
    Pointer { element: TypeId, mutable: bool },
    Reference { element: TypeId, mutable: bool },
    Function { params: Vec<TypeId>, result: TypeId },
}
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: TypeId,
}
#[derive(Debug, Clone)]
pub struct StructInfo {
    pub name: String,
    pub fields: Vec<Field>,
    pub serialize: bool,
    pub deserialize: bool,
    pub is_class: bool,
}
#[derive(Debug, Clone)]
pub struct Types {
    kinds: Vec<TypeKind>,
    interned: HashMap<TypeKind, TypeId>,
    names: HashMap<String, TypeId>,
    pub structs: Vec<StructInfo>,
}
impl Types {
    pub fn new() -> Self {
        let mut s = Self {
            kinds: Vec::new(),
            interned: HashMap::new(),
            names: HashMap::new(),
            structs: Vec::new(),
        };
        for (name, prim) in [
            ("()", Primitive::Unit),
            ("bool", Primitive::Bool),
            ("i8", Primitive::Signed(8)),
            ("i16", Primitive::Signed(16)),
            ("i32", Primitive::Signed(32)),
            ("i64", Primitive::Signed(64)),
            ("u8", Primitive::Unsigned(8)),
            ("u16", Primitive::Unsigned(16)),
            ("u32", Primitive::Unsigned(32)),
            ("u64", Primitive::Unsigned(64)),
            ("isize", Primitive::Signed(64)),
            ("usize", Primitive::Unsigned(64)),
            ("f32", Primitive::Float(32)),
            ("f64", Primitive::Float(64)),
            ("str", Primitive::Str),
        ] {
            let id = s.intern(TypeKind::Primitive(prim));
            s.names.insert(name.into(), id);
        }
        s
    }
    pub fn intern(&mut self, kind: TypeKind) -> TypeId {
        if let Some(id) = self.interned.get(&kind) {
            return *id;
        }
        let id = self.kinds.len();
        // TypeKind can own heap-allocated data (e.g. Function params).
        // Keep an owned copy in the vector and move the original into the map.
        self.kinds.push(kind.clone());
        self.interned.insert(kind, id);
        id
    }
    pub fn kind(&self, id: TypeId) -> TypeKind {
        self.kinds[id].clone()
    }
    pub fn named(&self, name: &str) -> Option<TypeId> {
        self.names.get(name).copied()
    }
    pub fn primitive(&self, primitive: Primitive) -> TypeId {
        self.interned[&TypeKind::Primitive(primitive)]
    }
    pub fn boolean(&self) -> TypeId {
        self.primitive(Primitive::Bool)
    }
    pub fn unit(&self) -> TypeId {
        self.primitive(Primitive::Unit)
    }
    pub fn i32(&self) -> TypeId {
        self.primitive(Primitive::Signed(32))
    }
    pub fn i64(&self) -> TypeId {
        self.primitive(Primitive::Signed(64))
    }
    pub fn f64(&self) -> TypeId {
        self.primitive(Primitive::Float(64))
    }
    pub fn string(&self) -> TypeId {
        self.primitive(Primitive::Str)
    }
    pub fn register_struct(
        &mut self,
        name: &str,
        serialize: bool,
        deserialize: bool,
        is_class: bool,
    ) -> Result<StructId, String> {
        if self.names.contains_key(name) {
            return Err(format!("duplicate type '{name}'"));
        }
        let id = self.structs.len();
        self.structs.push(StructInfo {
            name: name.into(),
            fields: Vec::new(),
            serialize,
            deserialize,
            is_class,
        });
        let ty = self.intern(TypeKind::Struct(id));
        self.names.insert(name.into(), ty);
        Ok(id)
    }
    pub fn resolve(&mut self, r: &TypeRef) -> Result<TypeId, String> {
        match r {
            TypeRef::Applied(..) => Err("compiler bug: unspecialized generic reached types".into()),
            TypeRef::Function { params, result } => {
                let params = params
                    .iter()
                    .map(|t| self.resolve(t))
                    .collect::<Result<Vec<_>, _>>()?;
                let result = self.resolve(result)?;
                Ok(self.intern(TypeKind::Function { params, result }))
            }
            TypeRef::Named(n) if n == "auto" => {
                Err("'auto' is only allowed for local declarations with an initializer".into())
            }
            TypeRef::Named(n) => self.named(n).ok_or_else(|| format!("unknown type '{n}'")),
            TypeRef::Array(inner, len) => {
                if *len > 1_000_000 {
                    return Err("array size must be 0..=1_000_000".into());
                }
                let element = self.resolve(inner)?;
                if element == self.unit() {
                    return Err("unit array elements are unsupported".into());
                }
                Ok(self.intern(TypeKind::Array { element, len: *len }))
            }
            TypeRef::Pointer { mutable, inner } => {
                let element = self.resolve(inner)?;
                Ok(self.intern(TypeKind::Pointer {
                    element,
                    mutable: *mutable,
                }))
            }
            TypeRef::Reference { mutable, inner } => {
                let element = self.resolve(inner)?;
                if element == self.unit() {
                    return Err("cannot borrow unit type".into());
                }
                Ok(self.intern(TypeKind::Reference {
                    element,
                    mutable: *mutable,
                }))
            }
        }
    }
    /// Borrowing inside stored aggregates requires lifetime parameters.
    /// Until lifetime-bearing struct definitions are implemented, disallow
    /// references as fields instead of permitting references to dangle.
    pub fn contains_borrow(&self, ty: TypeId) -> bool {
        match self.kind(ty) {
            TypeKind::Reference { .. } => true,
            TypeKind::Array { element, .. } => self.contains_borrow(element),
            _ => false,
        }
    }
    pub fn display(&self, id: TypeId) -> String {
        match self.kind(id) {
            TypeKind::Primitive(p) => match p {
                Primitive::Unit => "()".into(),
                Primitive::Bool => "bool".into(),
                Primitive::Str => "str".into(),
                Primitive::Signed(bits) => format!("i{bits}"),
                Primitive::Unsigned(bits) => format!("u{bits}"),
                Primitive::Float(bits) => format!("f{bits}"),
            },
            TypeKind::Struct(s) => self.structs[s].name.clone(),
            TypeKind::Array { element, len } => format!("[{}; {len}]", self.display(element)),
            TypeKind::Pointer { element, mutable } => format!(
                "*{} {}",
                if mutable { "mut" } else { "const" },
                self.display(element)
            ),
            TypeKind::Reference { element, mutable } => format!(
                "&{}{}",
                if mutable { "mut " } else { "" },
                self.display(element)
            ),
            TypeKind::Function { params, result } => format!(
                "fn({}) -> {}",
                params
                    .iter()
                    .map(|x| self.display(*x))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.display(result)
            ),
        }
    }
    pub fn llvm(&self, id: TypeId) -> String {
        match self.kind(id) {
            TypeKind::Primitive(p) => match p {
                Primitive::Unit => "void".into(),
                Primitive::Bool => "i1".into(),
                Primitive::Signed(n) | Primitive::Unsigned(n) => format!("i{n}"),
                Primitive::Float(32) => "float".into(),
                Primitive::Float(_) => "double".into(),
                Primitive::Str => "ptr".into(),
            },
            TypeKind::Struct(s) => format!("%struct.{}", self.structs[s].name),
            TypeKind::Array { element, len } => format!("[{len} x {}]", self.llvm(element)),
            TypeKind::Pointer { .. } | TypeKind::Reference { .. } | TypeKind::Function { .. } => {
                "ptr".into()
            }
        }
    }
    pub fn integer(&self, ty: TypeId) -> Option<(u8, bool)> {
        match self.kind(ty) {
            TypeKind::Primitive(Primitive::Signed(bits)) => Some((bits, true)),
            TypeKind::Primitive(Primitive::Unsigned(bits)) => Some((bits, false)),
            _ => None,
        }
    }
    pub fn float(&self, ty: TypeId) -> bool {
        matches!(self.kind(ty), TypeKind::Primitive(Primitive::Float(_)))
    }
    pub fn numeric(&self, ty: TypeId) -> bool {
        self.integer(ty).is_some() || self.float(ty)
    }
    pub fn can_numeric_cast(&self, from: TypeId, to: TypeId) -> bool {
        from == to
            || (self.numeric(from) && self.numeric(to))
            || (matches!(self.kind(from), TypeKind::Function { .. })
                && self.kind(to) == TypeKind::Primitive(Primitive::Unsigned(64)))
            || (self.kind(from) == TypeKind::Primitive(Primitive::Unsigned(64))
                && matches!(self.kind(to), TypeKind::Function { .. }))
            || (matches!(self.kind(from), TypeKind::Pointer { .. })
                && matches!(self.kind(to), TypeKind::Pointer { .. }))
    }
    pub fn scalar(&self, ty: TypeId) -> bool {
        matches!(
            self.kind(ty),
            TypeKind::Primitive(_)
                | TypeKind::Pointer { .. }
                | TypeKind::Reference { .. }
                | TypeKind::Function { .. }
        )
    }
    pub fn llvm_declarations(&self) -> String {
        let mut out = String::new();
        for s in &self.structs {
            let fields = s
                .fields
                .iter()
                .map(|f| self.llvm(f.ty))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("%struct.{} = type {{ {fields} }}\n", s.name));
        }
        out
    }
    pub fn check_sized(&self) -> Result<(), String> {
        fn visit(types: &Types, ty: TypeId, stack: &mut Vec<StructId>) -> Result<(), String> {
            match types.kind(ty) {
                TypeKind::Struct(s) => {
                    if stack.contains(&s) {
                        return Err(format!(
                            "recursive by-value structure '{}' requires indirection",
                            types.structs[s].name
                        ));
                    }
                    stack.push(s);
                    for field in &types.structs[s].fields {
                        visit(types, field.ty, stack)?;
                    }
                    stack.pop();
                }
                TypeKind::Array { element, .. } => visit(types, element, stack)?,
                TypeKind::Pointer { .. }
                | TypeKind::Reference { .. }
                | TypeKind::Function { .. }
                | TypeKind::Primitive(_) => {}
            }
            Ok(())
        }
        for s in 0..self.structs.len() {
            let t = self.named(&self.structs[s].name).unwrap();
            visit(self, t, &mut Vec::new())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod intern_tests {
    use super::{TypeKind, Types};

    #[test]
    fn function_type_interning_is_stable() {
        let mut types = Types::new();
        let params = vec![types.i64(), types.boolean()];
        let result = types.i32();
        let function = TypeKind::Function { params, result };
        let first = types.intern(function.clone());
        let second = types.intern(function.clone());
        assert_eq!(first, second);
        assert_eq!(types.kind(first), function);
    }
}
