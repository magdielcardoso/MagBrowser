//! Abstract syntax tree for the supported JavaScript subset.

/// A source position. Lines and columns are one based, offsets are character indices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    /// One based line number.
    pub line: usize,
    /// One based column number.
    pub column: usize,
    /// Zero based character offset from the start of the source.
    pub offset: usize,
}

/// A parsed program.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// Statements of the program, in source order.
    pub body: Vec<Stmt>,
}

/// Declaration kind of a variable statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    /// `var` declaration, function scoped.
    Var,
    /// `let` declaration, block scoped.
    Let,
    /// `const` declaration, block scoped and immutable.
    Const,
}

/// A single declarator inside a variable declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct VarDeclarator {
    /// Bound name. Destructuring is not supported.
    pub name: String,
    /// Optional initializer.
    pub init: Option<Expr>,
}

/// A `var`/`let`/`const` declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct VarDecl {
    /// Declaration kind.
    pub kind: DeclKind,
    /// Declarators, in source order.
    pub declarations: Vec<VarDeclarator>,
    /// Source position of the declaration keyword.
    pub pos: Position,
}

/// A function declaration, function expression or arrow function.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Name of the function, when one was written.
    pub name: Option<String>,
    /// Parameter names.
    pub params: Vec<String>,
    /// Function body statements.
    pub body: Vec<Stmt>,
    /// Whether the function is an arrow function.
    pub is_arrow: bool,
    /// Source position of the function.
    pub pos: Position,
}

/// A key used in an object literal.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyKey {
    /// An identifier key.
    Ident(String),
    /// A string literal key.
    String(String),
    /// A numeric literal key.
    Number(f64),
}

impl PropertyKey {
    /// The property key as a string, matching JavaScript's property key rules.
    pub fn as_key_string(&self) -> String {
        match self {
            Self::Ident(name) | Self::String(name) => name.clone(),
            Self::Number(n) => crate::ast::number_to_key(*n),
        }
    }
}

/// Format a number the way JavaScript formats a property key.
pub fn number_to_key(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A property of an object literal.
#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    /// Key of the property.
    pub key: PropertyKey,
    /// Value assigned to the property.
    pub value: Expr,
    /// Whether the property was written in shorthand form.
    pub shorthand: bool,
    /// Source position of the property key.
    pub pos: Position,
}

/// Unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// Unary plus.
    Plus,
    /// Unary minus.
    Minus,
    /// Logical not.
    Not,
    /// Bitwise not.
    BitNot,
    /// `typeof`.
    TypeOf,
    /// `void`.
    Void,
    /// `delete`.
    Delete,
}

/// Increment and decrement operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOp {
    /// `++`
    Increment,
    /// `--`
    Decrement,
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `**`
    Exp,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `===`
    StrictEq,
    /// `!==`
    StrictNe,
    /// `in`
    In,
    /// `instanceof`
    InstanceOf,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
}

/// Short-circuiting logical operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    /// `&&`
    And,
    /// `||`
    Or,
    /// `??`
    Nullish,
}

/// Assignment operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    /// `=`
    Assign,
    /// `+=`
    Add,
    /// `-=`
    Sub,
    /// `*=`
    Mul,
    /// `/=`
    Div,
    /// `%=`
    Mod,
    /// `**=`
    Exp,
}

/// A JavaScript expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Numeric literal.
    Number(f64),
    /// String literal.
    String(String),
    /// Boolean literal.
    Bool(bool),
    /// `null` literal.
    Null,
    /// `this`.
    This,
    /// Identifier reference.
    Identifier(String),
    /// Array literal. `None` represents an elision (hole).
    Array(Vec<Option<Expr>>),
    /// Object literal.
    Object(Vec<Property>),
    /// Function expression or declaration body.
    Function(Function),
    /// Arrow function.
    Arrow(Function),
    /// Unary expression.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        argument: Box<Expr>,
    },
    /// Prefix or postfix update expression.
    Update {
        /// Operator.
        op: UpdateOp,
        /// Whether the operator precedes the operand.
        prefix: bool,
        /// Operand.
        argument: Box<Expr>,
    },
    /// Binary expression.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        left: Box<Expr>,
        /// Right operand.
        right: Box<Expr>,
    },
    /// Logical expression.
    Logical {
        /// Operator.
        op: LogicalOp,
        /// Left operand.
        left: Box<Expr>,
        /// Right operand.
        right: Box<Expr>,
    },
    /// Assignment expression.
    Assign {
        /// Operator.
        op: AssignOp,
        /// Assignment target.
        target: Box<Expr>,
        /// Assigned value.
        value: Box<Expr>,
    },
    /// Conditional expression.
    Conditional {
        /// Condition.
        test: Box<Expr>,
        /// Value when the condition is truthy.
        consequent: Box<Expr>,
        /// Value when the condition is falsy.
        alternate: Box<Expr>,
    },
    /// Call expression.
    Call {
        /// Callee.
        callee: Box<Expr>,
        /// Arguments.
        arguments: Vec<Expr>,
    },
    /// `new` expression.
    New {
        /// Constructor.
        callee: Box<Expr>,
        /// Arguments.
        arguments: Vec<Expr>,
    },
    /// Member access, either `object.name` or `object[property]`.
    Member {
        /// Object expression.
        object: Box<Expr>,
        /// Property expression; an identifier when `computed` is false.
        property: Box<Expr>,
        /// Whether the access uses brackets.
        computed: bool,
    },
    /// Comma separated sequence expression.
    Sequence(Vec<Expr>),
    /// Template literal. `parts.len() == exprs.len() + 1`.
    Template {
        /// Cooked string parts.
        parts: Vec<String>,
        /// Interpolated expressions.
        exprs: Vec<Expr>,
    },
}

/// Initializer of a `for` statement.
#[derive(Clone, Debug, PartialEq)]
pub enum ForInit {
    /// Variable declaration initializer.
    Var(VarDecl),
    /// Expression initializer.
    Expr(Expr),
}

/// Left hand side of a `for...in` or `for...of` statement.
#[derive(Clone, Debug, PartialEq)]
pub enum ForTarget {
    /// A variable declaration without initializer.
    Var {
        /// Declaration kind.
        kind: DeclKind,
        /// Bound variable name.
        name: String,
    },
    /// An assignment target expression.
    Expr(Expr),
}

/// A `catch` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct CatchClause {
    /// Optional exception binding name.
    pub param: Option<String>,
    /// Statements of the catch block.
    pub body: Vec<Stmt>,
}

/// A `case` or `default` clause of a `switch` statement.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    /// Test expression; `None` for `default`.
    pub test: Option<Expr>,
    /// Statements of the clause.
    pub body: Vec<Stmt>,
}

/// A JavaScript statement.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    /// Expression statement.
    Expression(Expr),
    /// Variable declaration statement.
    VarDecl(VarDecl),
    /// Function declaration.
    FunctionDecl(Function),
    /// `return` statement.
    Return(Option<Expr>),
    /// `if` statement.
    If {
        /// Condition.
        test: Expr,
        /// Executed when the condition is truthy.
        consequent: Box<Stmt>,
        /// Executed otherwise.
        alternate: Option<Box<Stmt>>,
    },
    /// `while` statement.
    While {
        /// Condition.
        test: Expr,
        /// Loop body.
        body: Box<Stmt>,
    },
    /// `do...while` statement.
    DoWhile {
        /// Loop body.
        body: Box<Stmt>,
        /// Condition.
        test: Expr,
    },
    /// Three part `for` statement.
    For {
        /// Initializer.
        init: Option<ForInit>,
        /// Test expression.
        test: Option<Expr>,
        /// Update expression.
        update: Option<Expr>,
        /// Loop body.
        body: Box<Stmt>,
    },
    /// `for...in` or `for...of` statement.
    ForIn {
        /// Loop binding or assignment target.
        left: ForTarget,
        /// Iterated or enumerated expression.
        right: Expr,
        /// Loop body.
        body: Box<Stmt>,
        /// Whether this is a `for...of` loop.
        is_of: bool,
    },
    /// Block statement.
    Block(Vec<Stmt>),
    /// Empty statement.
    Empty,
    /// `break` statement with an optional label.
    Break(Option<String>),
    /// `continue` statement with an optional label.
    Continue(Option<String>),
    /// `throw` statement.
    Throw(Expr),
    /// `try...catch...finally` statement.
    Try {
        /// Statements of the try block.
        block: Vec<Stmt>,
        /// Optional catch clause.
        handler: Option<CatchClause>,
        /// Optional finally block.
        finalizer: Option<Vec<Stmt>>,
    },
    /// `switch` statement.
    Switch {
        /// Discriminant expression.
        discriminant: Expr,
        /// Clauses in source order.
        cases: Vec<SwitchCase>,
    },
    /// Labelled statement.
    Labeled {
        /// Label name.
        label: String,
        /// Labelled statement.
        body: Box<Stmt>,
    },
}
