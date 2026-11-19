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

/// A single specifier inside an `import { ... }` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportSpecifier {
    /// The name exported by the imported module.
    pub imported: String,
    /// The local binding the name is bound to.
    pub local: String,
}

/// A static `import` declaration.
///
/// All forms share one shape: `import default_name, { named } from "source"`
/// and `import default_name, * as namespace from "source"`. A side-effect only
/// import (`import "source";`) leaves every binding empty.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportDecl {
    /// Default import binding, if present.
    pub default: Option<String>,
    /// Named import specifiers, in source order.
    pub named: Vec<ImportSpecifier>,
    /// Namespace import binding (`import * as ns`), if present.
    pub namespace: Option<String>,
    /// Module specifier string, without the quotes.
    pub source: String,
    /// Source position of the `import` keyword.
    pub pos: Position,
}

/// A single specifier inside an `export { ... }` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSpecifier {
    /// The name in the exporting module (local binding or imported name).
    pub local: String,
    /// The name exposed to importers.
    pub exported: String,
}

/// The payload of an `export` declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportKind {
    /// `export default <expression>;`
    DefaultExpr(Expr),
    /// `export default function ...;` or `export default class ...;`
    DefaultDecl(Box<Stmt>),
    /// `export var/let/const/function/class ...;`
    Decl(Box<Stmt>),
    /// `export { a, b as c };` optionally followed by `from "source"`.
    Named {
        /// Export specifiers in source order.
        specifiers: Vec<ExportSpecifier>,
        /// Source module for a re-export, when `from` was present.
        source: Option<String>,
    },
    /// `export * from "source";`
    All {
        /// Source module whose exports are re-exported.
        source: String,
    },
}

/// An `export` declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportDecl {
    /// Which form of export this is.
    pub kind: ExportKind,
    /// Source position of the `export` keyword.
    pub pos: Position,
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

/// A binding or destructuring pattern.
#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    /// A single identifier binding.
    Identifier(String),
    /// An array pattern. `None` elements are elisions (holes).
    Array(Vec<Option<Pattern>>),
    /// An object pattern.
    Object {
        /// Named properties in source order.
        properties: Vec<ObjectPatternProperty>,
        /// Optional `...rest` binding.
        rest: Option<Box<Pattern>>,
    },
    /// A `...target` rest element or parameter.
    Rest(Box<Pattern>),
    /// A pattern with a default value, applied when the bound value is `undefined`.
    Default {
        /// Pattern receiving the value.
        target: Box<Pattern>,
        /// Expression evaluated when the value is missing.
        default: Box<Expr>,
    },
    /// A member expression target, used by destructuring assignments.
    Member(Box<Expr>),
}

impl Pattern {
    /// Collect every identifier bound by the pattern, in source order.
    pub fn bound_names(&self, out: &mut Vec<String>) {
        match self {
            Self::Identifier(name) => out.push(name.clone()),
            Self::Array(elements) => {
                for element in elements.iter().flatten() {
                    element.bound_names(out);
                }
            }
            Self::Object { properties, rest } => {
                for property in properties {
                    property.value.bound_names(out);
                }
                if let Some(rest) = rest {
                    rest.bound_names(out);
                }
            }
            Self::Rest(target) | Self::Default { target, .. } => target.bound_names(out),
            Self::Member(_) => {}
        }
    }

    /// Whether the pattern contains a default value or a rest element, which
    /// means it cannot be used as a plain `for...in`/`for...of` target.
    pub fn is_simple_identifier(&self) -> Option<&str> {
        match self {
            Self::Identifier(name) => Some(name),
            _ => None,
        }
    }
}

/// A single property of an object binding pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectPatternProperty {
    /// Property key to read from the source object.
    pub key: PropertyKey,
    /// Pattern receiving the property value.
    pub value: Pattern,
}

/// A single declarator inside a variable declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct VarDeclarator {
    /// Bound pattern.
    pub pattern: Pattern,
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
    /// Simple identifier parameter names, in source order. Patterns that are not
    /// plain identifiers are omitted; use [`Function::patterns`] for the full
    /// parameter list.
    pub params: Vec<String>,
    /// Full parameter patterns, including destructuring, defaults and rest.
    pub patterns: Vec<Pattern>,
    /// Function body statements.
    pub body: Vec<Stmt>,
    /// Whether the function is an arrow function.
    pub is_arrow: bool,
    /// Whether the function is a generator (`function*` or `async function*`).
    /// Generator functions return a generator object and may use `yield`.
    pub is_generator: bool,
    /// Whether the function is `async`. Async functions return a promise and
    /// may use `await`.
    pub is_async: bool,
    /// Source position of the function.
    pub pos: Position,
}

/// A class declaration or class expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Class {
    /// Class name, when one was written.
    pub name: Option<String>,
    /// Expression evaluated to obtain the superclass.
    pub super_class: Option<Box<Expr>>,
    /// Methods and static methods in source order.
    pub body: Vec<ClassMember>,
    /// Source position of the `class` keyword.
    pub pos: Position,
}

/// A member of a class body.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassMember {
    /// A method, getter, setter, generator or async method.
    Method {
        /// Property key of the method.
        key: PropertyKey,
        /// The method body and parameters.
        function: Function,
        /// Whether the member is `static`.
        is_static: bool,
        /// Whether this is a getter, setter or regular method.
        kind: MethodKind,
    },
}

/// The kind of a class or object method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    /// A regular method.
    Method,
    /// A `get` accessor.
    Getter,
    /// A `set` accessor.
    Setter,
}

impl ClassMember {
    /// Whether the member is static.
    pub fn is_static(&self) -> bool {
        match self {
            Self::Method { is_static, .. } => *is_static,
        }
    }
}

/// A single step of an optional chain such as `a?.b[0]?.()`.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionalChainOp {
    /// A property access. `optional` is true for `?.name` / `?.[expr]`.
    Member {
        /// Property expression (an identifier when `computed` is false).
        property: Box<Expr>,
        /// Whether the access is computed (`[expr]`).
        computed: bool,
        /// Whether this step short circuits the whole chain when nullish.
        optional: bool,
    },
    /// A call. `optional` is true for `?.(...)`.
    Call {
        /// Call arguments, which may contain spread elements.
        arguments: Vec<Expr>,
        /// Whether the call short circuits the whole chain when nullish.
        optional: bool,
    },
}

/// An optional chaining expression: a base followed by a sequence of member
/// and call steps. If a step marked `optional` receives `null` or `undefined`,
/// the whole expression evaluates to `undefined` without evaluating later steps.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionalChain {
    /// The expression the chain starts from.
    pub base: Box<Expr>,
    /// Each member access or call in the chain, in source order.
    pub ops: Vec<OptionalChainOp>,
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
    /// A computed key `[expr]`, evaluated when the object or class is created.
    Computed(Box<Expr>),
}

impl PropertyKey {
    /// The property key as a string, matching JavaScript's property key rules.
    pub fn as_key_string(&self) -> String {
        match self {
            Self::Ident(name) | Self::String(name) => name.clone(),
            Self::Number(n) => crate::ast::number_to_key(*n),
            // Computed keys are evaluated by the runtime; the parser cannot
            // know their value.
            Self::Computed(_) => "[computed]".to_owned(),
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
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `>>>`
    UShr,
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
    /// `<<=`
    Shl,
    /// `>>=`
    Shr,
    /// `>>>=`
    UShr,
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
    /// Regular expression literal.
    Regex {
        /// Pattern text, without the surrounding slashes.
        pattern: String,
        /// Flag characters, in source order.
        flags: String,
    },
    /// Function expression or declaration body.
    Function(Function),
    /// Arrow function.
    Arrow(Function),
    /// Destructuring assignment such as `[a, b] = [b, a]`.
    AssignPattern {
        /// Target pattern.
        pattern: Pattern,
        /// Assigned value.
        value: Box<Expr>,
    },
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
    /// A `class` expression.
    Class(Class),
    /// The `super` keyword. It is only meaningful as the callee of `super(...)`
    /// or as the object of `super.member` / `super[member]`.
    Super,
    /// A `yield` or `yield*` expression, valid inside generator functions.
    Yield {
        /// Optional value to yield.
        argument: Option<Box<Expr>>,
        /// Whether this is `yield*` (delegating iteration).
        delegate: bool,
    },
    /// An `await` expression, valid inside async functions.
    Await(Box<Expr>),
    /// An optional chaining expression (`a?.b`, `a?.[b]`, `a?.()`).
    OptionalChain(OptionalChain),
    /// A spread element (`...value`) used in array literals and call arguments.
    Spread(Box<Expr>),
    /// A dynamic `import(specifier)` expression. Evaluates to a promise for the
    /// imported module's namespace object.
    ImportCall(Box<Expr>),
    /// A meta property such as `import.meta`.
    MetaProperty {
        /// The meta object identifier (`import`).
        meta: String,
        /// The property name (`meta`).
        property: String,
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
    /// A destructuring declaration target such as `for (const [a, b] of xs)`.
    Pattern {
        /// Declaration kind.
        kind: DeclKind,
        /// Bound pattern.
        pattern: Pattern,
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
    /// Class declaration.
    ClassDecl(Class),
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
    /// A static `import` declaration.
    Import(ImportDecl),
    /// An `export` declaration.
    Export(ExportDecl),
}
