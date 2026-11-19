//! Keywords and token related syntax helpers.

use std::fmt;

/// Reserved words understood by the lexer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    /// `var`
    Var,
    /// `let`
    Let,
    /// `const`
    Const,
    /// `function`
    Function,
    /// `return`
    Return,
    /// `if`
    If,
    /// `else`
    Else,
    /// `while`
    While,
    /// `do`
    Do,
    /// `for`
    For,
    /// `in`
    In,
    /// `of`
    Of,
    /// `switch`
    Switch,
    /// `case`
    Case,
    /// `default`
    Default,
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `throw`
    Throw,
    /// `try`
    Try,
    /// `catch`
    Catch,
    /// `finally`
    Finally,
    /// `new`
    New,
    /// `typeof`
    TypeOf,
    /// `void`
    Void,
    /// `delete`
    Delete,
    /// `this`
    This,
    /// `true`
    True,
    /// `false`
    False,
    /// `null`
    Null,
    /// `instanceof`
    InstanceOf,
    /// `class`
    Class,
    /// `extends`
    Extends,
    /// `super`
    Super,
    /// `static`
    Static,
}

impl Keyword {
    /// Look up a keyword by its source text.
    pub fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "var" => Self::Var,
            "let" => Self::Let,
            "const" => Self::Const,
            "function" => Self::Function,
            "return" => Self::Return,
            "if" => Self::If,
            "else" => Self::Else,
            "while" => Self::While,
            "do" => Self::Do,
            "for" => Self::For,
            "in" => Self::In,
            "of" => Self::Of,
            "switch" => Self::Switch,
            "case" => Self::Case,
            "default" => Self::Default,
            "break" => Self::Break,
            "continue" => Self::Continue,
            "throw" => Self::Throw,
            "try" => Self::Try,
            "catch" => Self::Catch,
            "finally" => Self::Finally,
            "new" => Self::New,
            "typeof" => Self::TypeOf,
            "void" => Self::Void,
            "delete" => Self::Delete,
            "this" => Self::This,
            "true" => Self::True,
            "false" => Self::False,
            "null" => Self::Null,
            "instanceof" => Self::InstanceOf,
            "class" => Self::Class,
            "extends" => Self::Extends,
            "super" => Self::Super,
            "static" => Self::Static,
            _ => return None,
        })
    }

    /// The source text of the keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Var => "var",
            Self::Let => "let",
            Self::Const => "const",
            Self::Function => "function",
            Self::Return => "return",
            Self::If => "if",
            Self::Else => "else",
            Self::While => "while",
            Self::Do => "do",
            Self::For => "for",
            Self::In => "in",
            Self::Of => "of",
            Self::Switch => "switch",
            Self::Case => "case",
            Self::Default => "default",
            Self::Break => "break",
            Self::Continue => "continue",
            Self::Throw => "throw",
            Self::Try => "try",
            Self::Catch => "catch",
            Self::Finally => "finally",
            Self::New => "new",
            Self::TypeOf => "typeof",
            Self::Void => "void",
            Self::Delete => "delete",
            Self::This => "this",
            Self::True => "true",
            Self::False => "false",
            Self::Null => "null",
            Self::InstanceOf => "instanceof",
            Self::Class => "class",
            Self::Extends => "extends",
            Self::Super => "super",
            Self::Static => "static",
        }
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `text` is a reserved word.
pub fn is_keyword(text: &str) -> bool {
    Keyword::from_text(text).is_some()
}

/// Whether an identifier-like token may be used as a binding name.
pub fn is_identifier_name(text: &str) -> bool {
    !is_keyword(text)
}
