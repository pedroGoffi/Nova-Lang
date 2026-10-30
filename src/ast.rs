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
    /// Declarative compiler contracts written in Nova, e.g. #[operator(bit_or)].
    pub attributes: Vec<Attribute>,
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
            _ => {} // Custom attributes were validated against .nova metadata by meta::validate.
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
    /// Final expression without semicolon; its value belongs to the enclosing block.
    Tail(Expr),
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
    /// Control flow as a typed value. The two branches must yield the same type.
    If { condition: Box<Expr>, then_body: Vec<Stmt>, else_body: Vec<Stmt> },
    /// A lexical scope whose final expression supplies the value.
    Block(Vec<Stmt>),
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

/// Returns the Nova method bound to this operator in a trait declaration.
/// With one method the author chooses any method name (e.g. `merge`).
/// For multi-operator traits each named method binds its own operator key.
pub fn operator_trait_method<'a>(definition: &'a Trait, key: &str) -> Option<&'a str> {
    if !definition.attributes.iter().any(|a|a.name=="operator" && a.args.iter().any(|v|v==key)) {
        return None;
    }
    if definition.methods.len() == 1 {
        return Some(definition.methods[0].name.as_str());
    }
    definition.methods.iter().find(|m|m.name==key).map(|m|m.name.as_str())
}

/// Operator identifier used by declarative `#[operator(...)]` in Nova code.
/// No trait or method name is fixed here; those contracts belong to `.nova`.
pub fn operator_key(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::Add => "add", BinaryOp::Sub => "sub", BinaryOp::Mul => "mul",
        BinaryOp::Div => "div", BinaryOp::Rem => "rem", BinaryOp::Eq => "eq",
        BinaryOp::Ne => "ne", BinaryOp::Lt => "lt", BinaryOp::Le => "le",
        BinaryOp::Gt => "gt", BinaryOp::Ge => "ge", BinaryOp::BitAnd => "bit_and",
        BinaryOp::BitOr => "bit_or", BinaryOp::BitXor => "bit_xor",
        BinaryOp::Shl => "shift_left", BinaryOp::Shr => "shift_right",
        BinaryOp::And | BinaryOp::Or => return None, // preserves lazy evaluation
    })
}

pub fn valid_operator_key(key: &str) -> bool {
    ["add","sub","mul","div","rem","eq","ne","lt","le","gt","ge",
     "bit_and","bit_or","bit_xor","shift_left","shift_right"].contains(&key)
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

/// A final `if` can become a value only when *every* branch yields a
/// tail expression. An exhaustive control-flow `if` whose branches `return`,
/// `break`, or finish in a semicolon remains a statement: changing it into an
/// Expr::If would incorrectly demand a value from diverging/unit branches.
/// This check runs on the parsed AST, before surface type inference.
fn value_if_chain(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::If { then_body, else_body: Some(other), .. } => {
            if !matches!(then_body.last(), Some(Stmt::Tail(_))) {
                return false;
            }
            // Else-if is represented as a single nested statement by the
            // grammar; recurse into it rather than treating it as a unit.
            if let [nested @ Stmt::If { .. }] = other.as_slice() {
                value_if_chain(nested)
            } else {
                matches!(other.last(), Some(Stmt::Tail(_)))
            }
        }
        _ => false,
    }
}

fn final_if_as_expression(stmt: Stmt) -> Expr {
    match stmt {
        Stmt::If { condition, then_body, else_body: Some(other) } => {
            let else_body = if other.len() == 1 && matches!(other.first(), Some(Stmt::If { .. })) {
                vec![Stmt::Tail(final_if_as_expression(other.into_iter().next().unwrap()))]
            } else {
                other
            };
            Expr::If { condition: Box::new(condition), then_body, else_body }
        }
        _ => unreachable!("only value-producing if chains may become expressions"),
    }
}

pub fn normalize_block_tail(mut statements: Vec<Stmt>) -> Vec<Stmt> {
    if let Some(last) = statements.pop() {
        if value_if_chain(&last) {
            statements.push(Stmt::Tail(final_if_as_expression(last)));
        } else {
            statements.push(last);
        }
    }
    statements
}

/// Decode a Nova single-quoted Unicode scalar literal. This is deliberately
/// independent of string-literal decoding: one scalar, not one UTF-8 byte.
/// The parser keeps the original token until the surface diagnostic pass.
pub fn decode_char_literal(raw: &str) -> Result<char, String> {
    let content = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\''))
        .ok_or_else(|| "error[NOVA-CHAR001]: expected single-quoted character literal".to_string())?;
    if content.is_empty() {
        return Err("error[NOVA-CHAR001]: character literal cannot be empty".into());
    }
    let result = if let Some(rest) = content.strip_prefix('\\') {
        match rest {
            "n" => '\n', "r" => '\r', "t" => '\t', "0" => '\0',
            "\\" => '\\', "'" => '\'', "\"" => '"',
            _ if rest.starts_with("u{") && rest.ends_with('}') => {
                let hex = &rest[2..rest.len()-1];
                if hex.is_empty() || hex.len()>6 || !hex.bytes().all(|b|b.is_ascii_hexdigit()) {
                    return Err("error[NOVA-CHAR002]: expected \\u{1..6 hexadecimal digits}".into());
                }
                let code = u32::from_str_radix(hex,16)
                    .map_err(|_| "error[NOVA-CHAR002]: invalid Unicode escape".to_string())?;
                char::from_u32(code).ok_or_else(||
                    "error[NOVA-CHAR002]: Unicode escape is not a valid scalar value".to_string())?
            }
            _ if rest.starts_with('x') && rest.len()==3 && rest[1..].bytes().all(|b|b.is_ascii_hexdigit()) => {
                let code = u32::from_str_radix(&rest[1..],16).unwrap();
                char::from_u32(code).unwrap()
            }
            _ => return Err(format!("error[NOVA-CHAR002]: unknown character escape \\{rest}")),
        }
    } else {
        let mut chars=content.chars();
        let c=chars.next().unwrap();
        if chars.next().is_some() || c=='\n' || c=='\r' {
            return Err("error[NOVA-CHAR001]: character literal must contain exactly one Unicode scalar value".into());
        }
        c
    };
    Ok(result)
}
