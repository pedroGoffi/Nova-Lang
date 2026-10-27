//! Source-oriented AST. No LLVM-specific types or semantics live here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    Named(String),
    Applied(String, Vec<TypeRef>),
    Array(Box<TypeRef>, usize),
    Pointer {
        mutable: bool,
        inner: Box<TypeRef>,
    },
    /// Safe, compiler-checked borrow: &T or &mut T.
    Reference {
        mutable: bool,
        inner: Box<TypeRef>,
    },
    Function {
        params: Vec<TypeRef>,
        result: Box<TypeRef>,
    },
}
#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}
#[derive(Debug, Clone)]
pub enum Item {
    Function(Function),
    Extern(ExternFunction),
    Struct(Struct),
    Enum(Enum),
    TypeAlias(TypeAlias),
    Trait(Trait),
    Concept(Concept),
    ConceptAssert(ConceptExpr),
    Namespace {
        name: String,
        items: Vec<Item>,
    },
    Impl(Impl),
    Import(String),
    /// Declarative, AST-based macro expanded before typechecking.
    Macro(MacroDef),
    /// File-backed module declaration, resolved by package::load.
    Mod {
        name: String,
        at: usize,
    },
    /// Lexical import, resolved by the module linker.
    Use {
        path: String,
        at: usize,
    },
    /// Visibility marker. Package loader enforces it before lowering.
    Public(Box<Item>),
}
#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub name: String,
    pub target: TypeRef,
}
#[derive(Debug, Clone)]
pub struct TraitRestriction {
    pub type_parameter: String,
    pub permitted_types: Vec<TypeRef>,
}
#[derive(Debug, Clone)]
pub struct Trait {
    pub name: String,
    pub restriction: Option<TraitRestriction>,
    pub parents: Vec<String>,
    pub associated_types: Vec<String>,
    pub methods: Vec<TraitMethod>,
}
#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub result: TypeRef,
    pub default_body: Option<Vec<Stmt>>,
}
/// A compile-time concept: a boolean predicate over one or more types.
#[derive(Debug, Clone)]
pub struct Concept {
    pub name: String,
    pub generics: Vec<String>,
    pub predicate: ConceptExpr,
}
#[derive(Debug, Clone)]
pub enum ConceptExpr {
    Apply {
        name: String,
        args: Vec<TypeRef>,
    },
    And(Box<ConceptExpr>, Box<ConceptExpr>),
    Or(Box<ConceptExpr>, Box<ConceptExpr>),
    Not(Box<ConceptExpr>),
    Literal(bool),
    /// Structural requirements, checked at compile time without generating code.
    Requires {
        variables: Vec<(String, TypeRef)>,
        rules: Vec<Requirement>,
    },
}
#[derive(Debug, Clone)]
pub enum Requirement {
    Field {
        variable: String,
        field: String,
        result: TypeRef,
    },
    Method {
        variable: String,
        method: String,
        result: TypeRef,
    },
    Binary {
        left: String,
        operation: BinaryOp,
        right: String,
        result: TypeRef,
    },
}
#[derive(Debug, Clone)]
pub struct ConceptBound {
    pub name: String,
    pub args: Vec<TypeRef>,
}
#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    /// Ordered arms, matched left to right like Rust `macro_rules!`.
    pub arms: Vec<MacroArm>,
}
#[derive(Debug, Clone)]
pub struct MacroArm {
    pub params: Vec<MacroParam>,
    pub body: MacroBody,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroRepetition {
    ZeroOrMore,
    OneOrMore,
    Optional,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacroFragment {
    Expr,
    Ident,
    Literal,
    Path,
}
/// A concept attached to an AST macro argument. The compiler resolves the
/// actual expression type, rather than guessing a type from its AST shape.
#[derive(Debug, Clone)]
pub struct MacroConcept {
    pub name: String,
    /// Additional type arguments after the inferred expression type.
    /// `where same_as<i64>` means `same_as<ActualType, i64>`.
    pub additional_args: Vec<TypeRef>,
}
#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: String,
    pub fragment: MacroFragment,
    pub concept: Option<MacroConcept>,
    /// Only the final capture of a matcher may repeat at this AST stage.
    pub repetition: Option<MacroRepetition>,
}
#[derive(Debug, Clone)]
pub enum MacroBody {
    Expression(Expr),
    Statements(Vec<Stmt>),
}
#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<String>,
}
/// Validate the function effect attributes in one place, after parsing.
/// Unsafe is an explicit API contract, not inferred from an FFI symbol name.
pub fn function_is_unsafe(attributes: &[Attribute], context: &str) -> Result<bool, String> {
    let mut is_unsafe = false;
    for attribute in attributes {
        match (attribute.name.as_str(), attribute.args.as_slice()) {
            ("unsafe", []) => {
                if is_unsafe {
                    return Err(format!("duplicate unsafe attribute on {context}"));
                }
                is_unsafe = true;
            }
            ("unsafe", _) => {
                return Err(format!(
                    "unsafe attribute on {context} accepts no arguments"
                ))
            }
            ("pure" | "io" | "async" | "const", []) => {}
            ("pure" | "io" | "async" | "const", _) => {
                return Err(format!(
                    "effect attribute '{}' on {context} accepts no arguments",
                    attribute.name
                ))
            }
            _ => {
                return Err(format!(
                    "unsupported function attribute '{}' on {context}",
                    attribute.name
                ))
            }
        }
    }
    Ok(is_unsafe)
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub name: String,
    pub generics: Vec<String>,
    pub fields: Vec<Field>,
    pub attributes: Vec<Attribute>,
    pub is_class: bool,
    pub parent: Option<TypeRef>,
}
#[derive(Debug, Clone)]
pub struct Enum {
    pub name: String,
    pub generics: Vec<String>,
    pub variants: Vec<Variant>,
    pub attributes: Vec<Attribute>,
}
#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub payload: Vec<TypeRef>,
}
#[derive(Debug, Clone)]
pub struct Impl {
    pub generics: Vec<String>,
    pub trait_name: Option<String>,
    pub target: TypeRef,
    pub associated_types: Vec<(String, TypeRef)>,
    pub methods: Vec<Function>,
}
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
}
#[derive(Debug, Clone)]
pub enum FunctionMode {
    Normal,
    Generator,
    Coroutine,
    Async,
}
impl Default for FunctionMode {
    fn default() -> Self {
        Self::Normal
    }
}
#[derive(Debug, Clone)]
pub struct Function {
    pub mode: FunctionMode,
    pub name: String,
    pub generics: Vec<String>,
    pub concept_bounds: Vec<ConceptBound>,
    pub params: Vec<Param>,
    pub variadic: bool,
    pub result: TypeRef,
    pub body: Vec<Stmt>,
    pub is_virtual: bool,
    pub is_override: bool,
    pub attributes: Vec<Attribute>,
}
#[derive(Debug, Clone)]
pub struct ExternFunction {
    pub name: String,
    pub params: Vec<Param>,
    pub result: TypeRef,
    pub c_variadic: bool,
    pub attributes: Vec<Attribute>,
}
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: TypeRef,
    pub default: Option<Expr>,
}
#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        mutable: bool,
        ty: Option<TypeRef>,
        init: Expr,
        defer: Option<Expr>,
    },
    /// Register an expression to execute on every exit from its lexical scope.
    Defer(Expr),
    Set {
        name: String,
        value: Expr,
    },
    DerefSet {
        name: String,
        value: Expr,
    },
    FieldSet {
        root: String,
        field: String,
        op: Option<BinaryOp>,
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
        name: String,
        start: Expr,
        end: Expr,
        body: Vec<Stmt>,
    },
    /// Struct iteration: begin/end yield cursor values with equals/value/next.
    ForEach {
        name: String,
        iterable: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    CompoundSet {
        name: String,
        op: BinaryOp,
        value: Expr,
    },
    Unsafe(Vec<Stmt>),
    Expr(Expr),
    Match {
        value: Expr,
        arms: Vec<MatchArm>,
    },
}
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
}
#[derive(Debug, Clone)]
pub enum Pattern {
    Variant { path: String, bindings: Vec<String> },
    Wildcard,
}
#[derive(Debug, Clone)]
pub enum Expr {
    Int(String),
    Float(String),
    Bool(bool),
    Str(String),
    Unit,
    Var(String),
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinaryOp, Box<Expr>),
    Call(String, Vec<Expr>),
    FunctionRef(String),
    CallIndirect(Box<Expr>, Vec<Expr>),
    Lambda {
        params: Vec<Param>,
        result: TypeRef,
        body: Vec<Stmt>,
    },
    StructInit(String, Vec<(String, Expr)>),
    Field(Box<Expr>, String),
    Array(Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    AddressOf {
        mutable: bool,
        value: Box<Expr>,
    },
    Deref(Box<Expr>),
    MethodCall(Box<Expr>, String, Vec<Expr>),
    GenericCall(String, Vec<TypeRef>, Vec<Expr>),
    GenericStructInit(String, Vec<TypeRef>, Vec<(String, Expr)>),
    Zero(TypeRef),
    Cast(Box<Expr>, TypeRef),
    MacroCall(String, Vec<Expr>),
    /// `yield e` suspends a generator or coroutine, and evaluates to the next resume input.
    Yield(Box<Expr>),
    /// `await e` suspends an async coroutine until its child completes.
    Await(Box<Expr>),
    /// Pack forwarding is expanded at the call site before typed HIR.
    Spread(Box<Expr>),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// Builds one of the existing assignment statements after parsing the common
/// `PostfixExpr` prefix. Keeping this discrimination out of LALRPOP prevents
/// `identifier.field = ...` from competing with normal field-access expressions.
///
/// For now the language supports only direct locals and one-level fields.
/// Invalid targets are preserved for a semantic diagnostic rather than being
/// silently transformed into valid statements or panicking in the parser.
pub fn assignment_stmt(target: Expr, op: Option<BinaryOp>, value: Expr) -> Stmt {
    match target {
        Expr::Var(name) => match op {
            Some(op) => Stmt::CompoundSet { name, op, value },
            None => Stmt::Set { name, value },
        },
        Expr::Field(parent, field) => match *parent {
            Expr::Var(root) => Stmt::FieldSet {
                root,
                field,
                op,
                value,
            },
            _ => Stmt::FieldSet {
                root: "__nova_invalid_assignment_target__".into(),
                field: "nested field access is not yet supported".into(),
                op,
                value,
            },
        },
        _ => Stmt::FieldSet {
            root: "__nova_invalid_assignment_target__".into(),
            field: "assignment target must be a local or local.field".into(),
            op,
            value,
        },
    }
}
