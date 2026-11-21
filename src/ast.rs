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
    /// Module-scope immutable constant. CTFE replaces references with literals;
    /// this declaration is never emitted as a runtime global or linked symbol.
    CompileTimeConst(CompileTimeConst),
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
pub struct CompileTimeConst {
    pub name: String,
    pub ty: TypeRef,
    pub value: Expr,
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
    AssociatedType { owner: String, item: String },
    Field {
        variable: String,
        field: String,
        result: TypeRef,
    },
    Method {
        variable: String,
        method: String,
        arguments: Vec<TypeRef>,
        result: TypeRef,
    },
    /// A static method's full signature, checked before monomorphization.
    StaticMethod {
        owner: String,
        method: String,
        arguments: Vec<TypeRef>,
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
            ("derive", _) => {
                return Err(format!("unsupported function attribute 'derive' on {context}; derive applies to types"));
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
    pub concept_bounds: Vec<ConceptBound>,
    pub fields: Vec<Field>,
    pub attributes: Vec<Attribute>,
    pub is_class: bool,
    pub parent: Option<TypeRef>,
}
#[derive(Debug, Clone)]
pub struct Enum {
    pub name: String,
    pub generics: Vec<String>,
    pub concept_bounds: Vec<ConceptBound>,
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
    /// Optional field initializer, checked and emitted at the construction site.
    pub default: Option<Expr>,
    /// Compile-time validation annotations (never part of runtime object layout).
    pub attributes: Vec<Attribute>,
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
    pub attributes: Vec<Attribute>,
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
    /// Receiver, method name, value arguments, explicit method type arguments.
    /// An empty type list means normal inference (e.g. `obj.map(value)`).
    MethodCall(Box<Expr>, String, Vec<Expr>, Vec<TypeRef>),
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
/// Compile-time pipeline sugar: `value |> f(a)` becomes `f(value, a)`.
/// No temporary, dynamic dispatch or runtime helper is introduced.
pub fn pipeline_call(value: Expr, target: Expr) -> Expr {
    match target {
        Expr::Call(name, mut args) => {
            args.insert(0, value);
            Expr::Call(name, args)
        }
        Expr::GenericCall(name, types, mut args) => {
            args.insert(0, value);
            Expr::GenericCall(name, types, args)
        }
        Expr::MacroCall(name, mut args) => {
            args.insert(0, value);
            Expr::MacroCall(name, args)
        }
        Expr::Var(name) => Expr::Call(name, vec![value]),
        other => Expr::CallIndirect(Box::new(other), vec![value]),
    }
}

/// Desugars `[map_expr for item in collection if predicate]` into the
/// standard collection comprehension helper. The helper is deliberately an
/// ordinary generic Nova function, so comprehension keeps the same typing,
/// ownership and generator/iterator rules as an explicit pipeline.
pub fn list_comprehension(
    item: String,
    item_ty: TypeRef,
    collection: Expr,
    mapped: Expr,
    predicate: Option<Expr>,
) -> Expr {
    let map = Expr::Lambda {
        params: vec![Param {
            name: item.clone(),
            ty: item_ty.clone(),
            default: None,
            attributes: vec![],
        }],
        result: TypeRef::Named("auto".into()),
        body: vec![Stmt::Return(Some(mapped))],
    };
    let predicate = predicate.unwrap_or(Expr::Bool(true));
    let predicate = Expr::Lambda {
        params: vec![Param {
            name: item,
            ty: item_ty,
            default: None,
            attributes: vec![],
        }],
        result: TypeRef::Named("bool".into()),
        body: vec![Stmt::Return(Some(predicate))],
    };
    Expr::Call(
        "std::old::algorithm::comprehend".into(),
        vec![collection, map, predicate],
    )
}

/// Kotlin-style trailing lambda sugar. `call(args) { it }` is represented as
/// an ordinary call whose final argument is a one-parameter lambda. Keeping
/// the rewrite in the source AST means the existing lambda lifter and ABI do
/// not need a special calling convention.
pub fn implicit_lambda(body: Vec<Stmt>) -> Expr {
    Expr::Lambda {
        params: vec![Param {
            name: "it".into(),
            ty: TypeRef::Named("auto".into()),
            default: None,
            attributes: vec![],
        }],
        result: TypeRef::Named("auto".into()),
        body,
    }
}

pub fn trailing_lambda(target: Expr, body: Vec<Stmt>) -> Expr {
    let lambda = implicit_lambda(body);
    match target {
        Expr::Call(name, mut args) => {
            args.push(lambda);
            Expr::Call(name, args)
        }
        Expr::GenericCall(name, types, mut args) => {
            args.push(lambda);
            Expr::GenericCall(name, types, args)
        }
        Expr::MethodCall(receiver, name, mut args, types) => {
            args.push(lambda);
            Expr::MethodCall(receiver, name, args, types)
        }
        other => Expr::CallIndirect(Box::new(other), vec![lambda]),
    }
}

/// Kotlin-inspired scope-function lowering. The receiver is evaluated once
/// and exposed as `it`. `apply`/`also` return the receiver, while
/// `let`/`run`/`with` return the block's final value.
pub fn scope_expr(base: Expr, kind: &str, mut body: Vec<Stmt>) -> Expr {
    // Scope blocks own their local return. Lower the direct `return` form to
    // the block tail before the normal scope-function result rule runs; this
    // prevents it from escaping to the enclosing Nova function.
    let mut local_body = Vec::with_capacity(body.len());
    for statement in body {
        match statement {
            Stmt::Return(value) => {
                local_body.push(Stmt::Tail(value.unwrap_or(Expr::Unit)));
                break;
            }
            other => local_body.push(other),
        }
    }
    body = local_body;
    let returns_receiver = matches!(kind, "apply" | "also");
    if let Some(Stmt::Tail(value)) = body.last().cloned() {
        if returns_receiver {
            body.pop();
            body.push(Stmt::Expr(value));
        }
    }
    if returns_receiver {
        body.push(Stmt::Tail(Expr::Var("it".into())));
    }
    let mut statements = vec![Stmt::Let {
        name: "it".into(),
        mutable: true,
        ty: None,
        init: base,
        defer: None,
    }];
    statements.extend(body);
    Expr::Block(statements)
}

pub fn apply_expr(base: Expr, body: Vec<Stmt>) -> Expr {
    scope_expr(base, "apply", body)
}

/// Cascade initialization sugar. `new User { id: 1 }..active = true` becomes
/// a lexical block that evaluates the base once, mutates it, and returns it.
/// A fresh-looking hidden name is scoped to the generated block, so cascades
/// can be nested without changing the runtime representation.
pub fn cascade_expr(
    base: Expr,
    assignments: Vec<(String, Option<BinaryOp>, Expr)>,
    ty: Option<TypeRef>,
) -> Expr {
    let base = match (base, ty.as_ref()) {
        // Resolve a target-typed constructor before putting it inside the
        // generated block. The surface pass can resolve a concrete static
        // constructor directly; keeping the marker nested in a block loses
        // that expected-type context in the bootstrap pipeline.
        (Expr::Call(marker, args), Some(TypeRef::Named(name)))
            if marker == "__nova_target_new" => Expr::Call(format!("{name}::new"), args),
        (Expr::Call(marker, args), Some(TypeRef::Applied(name, types)))
            if marker == "__nova_target_new" => {
            Expr::GenericCall(format!("{name}::new"), types.clone(), args)
        }
        (base, _) => base,
    };
    let root = "__nova_cascade_value".to_owned();
    let mut statements = vec![Stmt::Let {
        name: root.clone(),
        mutable: true,
        // Preserve the declared type of the destination local. This is what
        // lets a bare `new()` resolve against `let mut value: Config`.
        ty,
        init: base,
        defer: None,
    }];
    for (field, op, value) in assignments {
        statements.push(assignment_stmt(
            Expr::Field(Box::new(Expr::Var(root.clone())), field),
            op,
            value,
        ));
    }
    statements.push(Stmt::Tail(Expr::Var(root)));
    Expr::Block(statements)
}

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

/// Internal spelling for ownership-aware replacement. It remains a Set in
/// the source AST and is expanded only after the concrete owner type is known.
pub const REPLACE_PREFIX: &str = "__nova_internal_replace__\u{1f}";

pub fn replace_stmt(name: String, value: Expr) -> Stmt {
    Stmt::Set {
        name: format!("{REPLACE_PREFIX}{name}"),
        value,
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

// The marker only lives in the grammar AST until normalize_block_tail runs.
// Keeping the lowering in one place avoids a new runtime/HIR statement kind.
const REFUTABLE_LET_CONTINUATION: &str = "__nova_internal_refutable_let_continuation";

/// Build the source-level representation for:
/// `let Some(value) = optional else { return fallback; };`
/// This works for other enum variants too, without making Option special.
pub fn refutable_let_else(
    path: String,
    bindings: Vec<String>,
    value: Expr,
    failure: Vec<Stmt>,
) -> Stmt {
    Stmt::Match {
        value,
        arms: vec![
            MatchArm {
                pattern: Pattern::Variant { path, bindings },
                body: vec![Stmt::Expr(Expr::Var(REFUTABLE_LET_CONTINUATION.into()))],
            },
            MatchArm { pattern: Pattern::Wildcard, body: failure },
        ],
    }
}

// A refutable let-else must not leave its failure branch and then continue
// with an uninitialized binding. `return`, `break` and `continue` are
// sufficient exits; nested exhaustive control-flow can also qualify.
fn branch_exits(body: &[Stmt]) -> bool {
    match body.last() {
        Some(Stmt::Return(_) | Stmt::Break | Stmt::Continue) => true,
        Some(Stmt::If { then_body, else_body: Some(other), .. }) =>
            branch_exits(then_body) && branch_exits(other),
        Some(Stmt::Match { arms, .. }) if !arms.is_empty() =>
            arms.iter().all(|arm| branch_exits(&arm.body)),
        _ => false,
    }
}

fn is_refutable_let_marker(stmt: &Stmt) -> bool {
    let Stmt::Match { arms, .. } = stmt else { return false };
    matches!(arms.as_slice(), [success, failure]
        if matches!(&success.body[..], [Stmt::Expr(Expr::Var(name))] if name == REFUTABLE_LET_CONTINUATION)
        && matches!(&failure.pattern, Pattern::Wildcard))
}

// Destructuring is a compile-time rewrite; a single hidden binding evaluates
// the initializer once. The ordinary field/index type checker does the rest.
const DESTRUCTURE_PREFIX: &str = "__nova_internal_destructure__";
pub fn destructure_binding(owner: Option<String>, names: Vec<String>, init: Expr) -> Stmt {
    let shape = if let Some(owner) = owner { format!("struct:{owner}") } else { "array".into() };
    Stmt::Let {
        name: format!("{DESTRUCTURE_PREFIX}{shape}\u{1f}{}", names.join("\u{1f}")),
        mutable: false, ty: None, init, defer: None,
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
    // Expand destructuring before continuation normalization so the declared
    // names become visible to subsequent statements in the same block.
    let mut expanded = Vec::with_capacity(statements.len());
    for (idx, statement) in statements.into_iter().enumerate() {
        match statement {
            Stmt::Let {name, init, ..} if name.starts_with(DESTRUCTURE_PREFIX) => {
                let body = &name[DESTRUCTURE_PREFIX.len()..];
                let (shape, members) = body.split_once('\u{1f}').unwrap_or((body,""));
                let members: Vec<&str> = members.split('\u{1f}').filter(|s| !s.is_empty()).collect();
                let unique=format!("__nova_destructure_{idx}");
                let ty=shape.strip_prefix("struct:").map(|t| TypeRef::Named(t.to_string()));
                expanded.push(Stmt::Let {name:unique.clone(),mutable:false,ty,init,defer:None});
                for (index,member) in members.iter().enumerate() {
                    // Duplicate names are rejected by normal local checking.
                    let access=if shape=="array" {
                        Expr::Index(Box::new(Expr::Var(unique.clone())),Box::new(Expr::Int(index.to_string())))
                    } else {
                        Expr::Field(Box::new(Expr::Var(unique.clone())),(*member).to_string())
                    };
                    expanded.push(Stmt::Let {name:(*member).into(),mutable:false,ty:None,init:access,defer:None});
                }
            }
            other => expanded.push(other),
        }
    }
    let mut statements=expanded;
    // Right-to-left continuation lowering gives each binding lexical scope
    // over the remainder of its enclosing block. The underlying match already
    // evaluates its subject once and checks enum variants and payload types.
    for index in (0..statements.len()).rev() {
        if !is_refutable_let_marker(&statements[index]) { continue; }
        let rest = statements.split_off(index + 1);
        let Stmt::Match { arms, .. } = &mut statements[index] else { unreachable!() };
        if !branch_exits(&arms[1].body) {
            // Produce a precise diagnostic through the ordinary semantic
            // checker rather than silently falling through with an unbound
            // payload (the AST's existing unknown-local diagnostic applies).
            arms[1].body.push(Stmt::Set {
                name: "__nova_let_else_must_diverge__".into(),
                value: Expr::Unit,
            });
        }
        arms[0].body = rest;
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
