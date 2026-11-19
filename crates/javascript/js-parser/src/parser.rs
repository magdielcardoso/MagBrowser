//! Recursive descent parser for the supported JavaScript subset.

use std::fmt;

use crate::ast::{
    AssignOp, BinaryOp, CatchClause, Class, ClassMember, DeclKind, ExportDecl, ExportKind,
    ExportSpecifier, Expr, ForInit, ForTarget, Function, ImportDecl, ImportSpecifier, LogicalOp,
    MethodKind, ObjectPatternProperty, OptionalChain, OptionalChainOp, Pattern, Position, Program,
    Property, PropertyKey, Stmt, SwitchCase, UnaryOp, UpdateOp, VarDecl, VarDeclarator,
};
use crate::lexer::{Punct, Token, TokenKind, tokenize};
use crate::regexp::Regex;
use crate::syntax::Keyword;

/// A syntax error with a source position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Human readable error message.
    pub message: String,
    /// One based line of the error.
    pub line: usize,
    /// One based column of the error.
    pub column: usize,
}

impl ParseError {
    /// Create a new error at the given position.
    pub fn new(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            message: message.into(),
            line,
            column,
        }
    }

    fn at(message: impl Into<String>, position: Position) -> Self {
        Self::new(message, position.line, position.column)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (line {}, column {})",
            self.message, self.line, self.column
        )
    }
}

impl std::error::Error for ParseError {}

/// Parse a JavaScript source string into a program.
pub fn parse(source: &str) -> Result<Program, ParseError> {
    let tokens = tokenize(source)?;
    Parser::new(tokens).parse_program()
}

/// The recursive descent parser.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    no_in: bool,
    depth: usize,
}

impl Parser {
    /// Maximum recursion depth while parsing statements and expressions.
    ///
    /// Deeply nested source (for example thousands of opening parentheses)
    /// would otherwise exhaust the native stack. Exceeding the limit produces
    /// a syntax error instead of overflowing.
    pub const MAX_RECURSION_DEPTH: usize = 512;

    /// Create a parser over a token stream. The stream must end with an `Eof` token.
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            no_in: false,
            depth: 0,
        }
    }

    /// Enter a recursive parse function, rejecting nesting that is too deep.
    fn enter_recursion(&mut self) -> Result<(), ParseError> {
        if self.depth >= Self::MAX_RECURSION_DEPTH {
            return self.error("expression is nested too deeply");
        }
        self.depth += 1;
        Ok(())
    }

    /// Leave a recursive parse function. Always paired with `enter_recursion`
    /// even when the inner parse returns an error.
    fn leave_recursion(&mut self) {
        self.depth -= 1;
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len().saturating_sub(1))]
    }

    fn kind(&self) -> &TokenKind {
        &self.current().kind
    }

    fn at_end(&self) -> bool {
        matches!(self.kind(), TokenKind::Eof)
    }

    fn peek_kind(&self, offset: usize) -> &TokenKind {
        let index = (self.pos + offset).min(self.tokens.len().saturating_sub(1));
        &self.tokens[index].kind
    }

    fn advance(&mut self) -> Token {
        let token = self.current().clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError::at(message, self.current().position))
    }

    fn unexpected<T>(&self, expected: &str) -> Result<T, ParseError> {
        self.error(format!(
            "expected {expected}, found {}",
            self.kind().describe()
        ))
    }

    fn is_punct(&self, punct: Punct) -> bool {
        matches!(self.kind(), TokenKind::Punct(p) if *p == punct)
    }

    fn eat_punct(&mut self, punct: Punct) -> bool {
        if self.is_punct(punct) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, punct: Punct) -> Result<Token, ParseError> {
        if self.is_punct(punct) {
            Ok(self.advance())
        } else {
            self.unexpected(&format!("`{}`", punct.as_str()))
        }
    }

    fn is_keyword(&self, keyword: Keyword) -> bool {
        matches!(self.kind(), TokenKind::Keyword(k) if *k == keyword)
    }

    fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        if self.is_keyword(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, keyword: Keyword) -> Result<Token, ParseError> {
        if self.is_keyword(keyword) {
            Ok(self.advance())
        } else {
            self.unexpected(&format!("`{keyword}`"))
        }
    }

    /// Consume a statement terminator, applying automatic semicolon insertion.
    fn semicolon(&mut self) -> Result<(), ParseError> {
        if self.eat_punct(Punct::Semi) {
            return Ok(());
        }
        if self.is_punct(Punct::RBrace) || self.at_end() || self.current().newline_before {
            return Ok(());
        }
        self.unexpected("`;`")
    }

    fn identifier_name(&mut self) -> Result<String, ParseError> {
        match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(name)
            }
            TokenKind::Keyword(keyword) => {
                // Keywords are accepted as property names and object keys.
                self.advance();
                Ok(keyword.as_str().to_owned())
            }
            _ => self.unexpected("an identifier"),
        }
    }

    fn binding_identifier(&mut self) -> Result<String, ParseError> {
        match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(name)
            }
            _ => self.unexpected("a binding identifier"),
        }
    }

    /// Whether the current token is the identifier-shaped word `word`.
    fn is_identifier_word(&self, word: &str) -> bool {
        matches!(self.kind(), TokenKind::Identifier(text) if text == word)
    }

    fn eat_identifier_word(&mut self, word: &str) -> bool {
        if self.is_identifier_word(word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_identifier_word(&mut self, word: &str) -> Result<(), ParseError> {
        if self.eat_identifier_word(word) {
            Ok(())
        } else {
            self.unexpected(&format!("`{word}`"))
        }
    }

    /// Consume a quoted module specifier, returning its cooked text.
    fn parse_module_specifier(&mut self) -> Result<String, ParseError> {
        match self.kind().clone() {
            TokenKind::String(source) => {
                self.advance();
                Ok(source)
            }
            _ => self.unexpected("a module specifier string"),
        }
    }

    /// Parse the `{ ... }` part of an import declaration. The opening brace is
    /// expected to be the current token.
    fn parse_import_specifiers(&mut self) -> Result<Vec<ImportSpecifier>, ParseError> {
        self.expect_punct(Punct::LBrace)?;
        let mut specifiers = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            // `import { default as d }` needs the keyword-tolerant name here.
            let imported = self.identifier_name()?;
            let local = if self.eat_identifier_word("as") {
                self.binding_identifier()?
            } else {
                imported.clone()
            };
            specifiers.push(ImportSpecifier { imported, local });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace)?;
        Ok(specifiers)
    }

    /// Parse the `{ ... }` part of an export declaration. The opening brace is
    /// expected to be the current token.
    fn parse_export_specifiers(&mut self) -> Result<Vec<ExportSpecifier>, ParseError> {
        self.expect_punct(Punct::LBrace)?;
        let mut specifiers = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            let local = self.identifier_name()?;
            let exported = if self.eat_identifier_word("as") {
                self.identifier_name()?
            } else {
                local.clone()
            };
            specifiers.push(ExportSpecifier { local, exported });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace)?;
        Ok(specifiers)
    }

    /// Parse a static `import` declaration.
    fn parse_import_declaration(&mut self) -> Result<Stmt, ParseError> {
        let pos = self.current().position;
        self.advance(); // `import`

        // Side-effect only: `import "module";`
        if matches!(self.kind(), TokenKind::String(_)) {
            let source = self.parse_module_specifier()?;
            self.semicolon()?;
            return Ok(Stmt::Import(ImportDecl {
                default: None,
                named: Vec::new(),
                namespace: None,
                source,
                pos,
            }));
        }

        let mut default = None;
        let mut named = Vec::new();
        let mut namespace = None;
        match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                default = Some(name);
                if self.eat_punct(Punct::Comma) {
                    if self.eat_punct(Punct::Star) {
                        self.expect_identifier_word("as")?;
                        namespace = Some(self.binding_identifier()?);
                    } else {
                        named = self.parse_import_specifiers()?;
                    }
                }
            }
            TokenKind::Punct(Punct::Star) => {
                self.advance();
                self.expect_identifier_word("as")?;
                namespace = Some(self.binding_identifier()?);
            }
            TokenKind::Punct(Punct::LBrace) => {
                named = self.parse_import_specifiers()?;
            }
            _ => return self.unexpected("an import clause or a module specifier"),
        }
        self.expect_identifier_word("from")?;
        let source = self.parse_module_specifier()?;
        self.semicolon()?;
        Ok(Stmt::Import(ImportDecl {
            default,
            named,
            namespace,
            source,
            pos,
        }))
    }

    /// Parse an `export` declaration.
    fn parse_export_declaration(&mut self) -> Result<Stmt, ParseError> {
        let pos = self.current().position;
        self.advance(); // `export`

        if self.eat_keyword(Keyword::Default) {
            let kind = if self.is_keyword(Keyword::Function) {
                // `export default function () {}` has no name; accepting an
                // expression-style function covers both named and anonymous.
                let mut function = self.parse_function(true, false)?;
                if function.name.is_none() {
                    function.name = Some("default".to_owned());
                }
                ExportKind::DefaultDecl(Box::new(Stmt::FunctionDecl(function)))
            } else if self.is_keyword(Keyword::Class) {
                let mut class = self.parse_class()?;
                if class.name.is_none() {
                    class.name = Some("default".to_owned());
                }
                ExportKind::DefaultDecl(Box::new(Stmt::ClassDecl(class)))
            } else if self.is_identifier_word("async")
                && matches!(self.peek_kind(1), TokenKind::Keyword(Keyword::Function))
            {
                self.advance();
                let mut function = self.parse_function(true, true)?;
                if function.name.is_none() {
                    function.name = Some("default".to_owned());
                }
                ExportKind::DefaultDecl(Box::new(Stmt::FunctionDecl(function)))
            } else {
                let expression = self.parse_assignment()?;
                self.semicolon()?;
                ExportKind::DefaultExpr(expression)
            };
            return Ok(Stmt::Export(ExportDecl { kind, pos }));
        }

        if self.eat_punct(Punct::Star) {
            self.expect_identifier_word("from")?;
            let source = self.parse_module_specifier()?;
            self.semicolon()?;
            return Ok(Stmt::Export(ExportDecl {
                kind: ExportKind::All { source },
                pos,
            }));
        }

        if self.is_punct(Punct::LBrace) {
            let specifiers = self.parse_export_specifiers()?;
            let source = if self.eat_identifier_word("from") {
                Some(self.parse_module_specifier()?)
            } else {
                None
            };
            self.semicolon()?;
            return Ok(Stmt::Export(ExportDecl {
                kind: ExportKind::Named { specifiers, source },
                pos,
            }));
        }

        let declaration = match self.kind().clone() {
            TokenKind::Keyword(Keyword::Var)
            | TokenKind::Keyword(Keyword::Let)
            | TokenKind::Keyword(Keyword::Const) => self.parse_variable_statement()?,
            TokenKind::Keyword(Keyword::Function) => {
                Stmt::FunctionDecl(self.parse_function(false, false)?)
            }
            TokenKind::Keyword(Keyword::Class) => Stmt::ClassDecl(self.parse_class()?),
            _ if self.is_identifier_word("async")
                && matches!(self.peek_kind(1), TokenKind::Keyword(Keyword::Function)) =>
            {
                self.advance();
                Stmt::FunctionDecl(self.parse_function(false, true)?)
            }
            _ => return self.unexpected("a declaration, `default`, `{` or `*`"),
        };
        Ok(Stmt::Export(ExportDecl {
            kind: ExportKind::Decl(Box::new(declaration)),
            pos,
        }))
    }

    /// Parse a complete program.
    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut body = Vec::new();
        while !self.at_end() {
            body.push(self.parse_statement()?);
        }
        Ok(Program { body })
    }

    fn parse_statement(&mut self) -> Result<Stmt, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_statement_inner();
        self.leave_recursion();
        result
    }

    fn parse_statement_inner(&mut self) -> Result<Stmt, ParseError> {
        match self.kind().clone() {
            TokenKind::Punct(Punct::Semi) => {
                self.advance();
                Ok(Stmt::Empty)
            }
            TokenKind::Punct(Punct::LBrace) => Ok(Stmt::Block(self.parse_block()?)),
            TokenKind::Keyword(Keyword::Var) => self.parse_variable_statement(),
            TokenKind::Keyword(Keyword::Let) | TokenKind::Keyword(Keyword::Const) => {
                self.parse_variable_statement()
            }
            TokenKind::Keyword(Keyword::Function) => {
                let function = self.parse_function(false, false)?;
                Ok(Stmt::FunctionDecl(function))
            }
            TokenKind::Keyword(Keyword::Class) => Ok(Stmt::ClassDecl(self.parse_class()?)),
            TokenKind::Identifier(ref name)
                if name == "import"
                    && !matches!(
                        self.peek_kind(1),
                        TokenKind::Punct(Punct::LParen | Punct::Dot | Punct::QuestionDot)
                    ) =>
            {
                self.parse_import_declaration()
            }
            TokenKind::Identifier(ref name) if name == "export" => self.parse_export_declaration(),
            TokenKind::Identifier(ref name)
                if name.as_str() == "async"
                    && matches!(self.peek_kind(1), TokenKind::Keyword(Keyword::Function)) =>
            {
                self.advance();
                let function = self.parse_function(false, true)?;
                Ok(Stmt::FunctionDecl(function))
            }
            TokenKind::Keyword(Keyword::If) => self.parse_if(),
            TokenKind::Keyword(Keyword::While) => self.parse_while(),
            TokenKind::Keyword(Keyword::Do) => self.parse_do_while(),
            TokenKind::Keyword(Keyword::For) => self.parse_for(),
            TokenKind::Keyword(Keyword::Switch) => self.parse_switch(),
            TokenKind::Keyword(Keyword::Break) => self.parse_break_continue(true),
            TokenKind::Keyword(Keyword::Continue) => self.parse_break_continue(false),
            TokenKind::Keyword(Keyword::Return) => self.parse_return(),
            TokenKind::Keyword(Keyword::Throw) => self.parse_throw(),
            TokenKind::Keyword(Keyword::Try) => self.parse_try(),
            TokenKind::Identifier(name)
                if matches!(self.peek_kind(1), TokenKind::Punct(Punct::Colon)) =>
            {
                self.advance();
                self.advance();
                let body = self.parse_statement()?;
                Ok(Stmt::Labeled {
                    label: name,
                    body: Box::new(body),
                })
            }
            _ => {
                let expr = self.parse_expression()?;
                self.semicolon()?;
                Ok(Stmt::Expression(expr))
            }
        }
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, ParseError> {
        self.expect_punct(Punct::LBrace)?;
        self.parse_block_body()
    }

    fn parse_block_body(&mut self) -> Result<Vec<Stmt>, ParseError> {
        let mut body = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            if self.at_end() {
                return self.unexpected("`}`");
            }
            body.push(self.parse_statement()?);
        }
        self.expect_punct(Punct::RBrace)?;
        Ok(body)
    }

    fn parse_variable_statement(&mut self) -> Result<Stmt, ParseError> {
        let decl = self.parse_var_decl()?;
        self.semicolon()?;
        Ok(Stmt::VarDecl(decl))
    }

    fn parse_var_decl(&mut self) -> Result<VarDecl, ParseError> {
        let position = self.current().position;
        let kind = match self.kind() {
            TokenKind::Keyword(Keyword::Var) => DeclKind::Var,
            TokenKind::Keyword(Keyword::Let) => DeclKind::Let,
            TokenKind::Keyword(Keyword::Const) => DeclKind::Const,
            _ => return self.unexpected("a variable declaration"),
        };
        self.advance();
        let mut declarations = Vec::new();
        loop {
            let pattern = self.parse_binding_pattern(false)?;
            let init = if self.eat_punct(Punct::Eq) {
                Some(self.parse_assignment()?)
            } else {
                None
            };
            declarations.push(VarDeclarator { pattern, init });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        Ok(VarDecl {
            kind,
            declarations,
            pos: position,
        })
    }

    /// Parse a binding pattern. `allow_default` permits a trailing `= expr`
    /// inside parameter and array/object element positions.
    fn parse_binding_pattern(&mut self, allow_default: bool) -> Result<Pattern, ParseError> {
        let base = self.parse_binding_pattern_base()?;
        if allow_default && self.eat_punct(Punct::Eq) {
            let default = self.parse_assignment()?;
            return Ok(Pattern::Default {
                target: Box::new(base),
                default: Box::new(default),
            });
        }
        Ok(base)
    }

    fn parse_binding_pattern_base(&mut self) -> Result<Pattern, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_binding_pattern_base_inner();
        self.leave_recursion();
        result
    }

    fn parse_binding_pattern_base_inner(&mut self) -> Result<Pattern, ParseError> {
        match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(Pattern::Identifier(name))
            }
            TokenKind::Punct(Punct::LBracket) => {
                self.advance();
                let mut elements: Vec<Option<Pattern>> = Vec::new();
                loop {
                    if self.eat_punct(Punct::RBracket) {
                        break;
                    }
                    if self.eat_punct(Punct::Comma) {
                        elements.push(None);
                        continue;
                    }
                    if self.eat_punct(Punct::Ellipsis) {
                        let target = self.parse_binding_pattern(false)?;
                        elements.push(Some(Pattern::Rest(Box::new(target))));
                        self.expect_punct(Punct::RBracket)?;
                        break;
                    }
                    let element = self.parse_binding_pattern(true)?;
                    elements.push(Some(element));
                    if self.eat_punct(Punct::Comma) {
                        continue;
                    }
                    self.expect_punct(Punct::RBracket)?;
                    break;
                }
                Ok(Pattern::Array(elements))
            }
            TokenKind::Punct(Punct::LBrace) => {
                self.advance();
                let mut properties = Vec::new();
                let mut rest = None;
                loop {
                    if self.eat_punct(Punct::RBrace) {
                        break;
                    }
                    if self.eat_punct(Punct::Ellipsis) {
                        let target = self.parse_binding_pattern(false)?;
                        rest = Some(Box::new(target));
                        self.expect_punct(Punct::RBrace)?;
                        break;
                    }
                    let key = self.parse_property_key()?;
                    let value = if self.eat_punct(Punct::Colon) {
                        self.parse_binding_pattern(true)?
                    } else {
                        let PropertyKey::Ident(name) = &key else {
                            return self.error("shorthand object patterns need an identifier key");
                        };
                        let base = Pattern::Identifier(name.clone());
                        if self.eat_punct(Punct::Eq) {
                            Pattern::Default {
                                target: Box::new(base),
                                default: Box::new(self.parse_assignment()?),
                            }
                        } else {
                            base
                        }
                    };
                    properties.push(ObjectPatternProperty { key, value });
                    if self.eat_punct(Punct::Comma) {
                        continue;
                    }
                    self.expect_punct(Punct::RBrace)?;
                    break;
                }
                Ok(Pattern::Object { properties, rest })
            }
            _ => self.unexpected("a binding pattern"),
        }
    }

    fn parse_property_key(&mut self) -> Result<PropertyKey, ParseError> {
        match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(PropertyKey::Ident(name))
            }
            TokenKind::Keyword(keyword) => {
                self.advance();
                Ok(PropertyKey::Ident(keyword.as_str().to_owned()))
            }
            TokenKind::String(value) => {
                self.advance();
                Ok(PropertyKey::String(value))
            }
            TokenKind::Number(value) => {
                self.advance();
                Ok(PropertyKey::Number(value))
            }
            TokenKind::Punct(Punct::LBracket) => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect_punct(Punct::RBracket)?;
                Ok(PropertyKey::Computed(Box::new(expr)))
            }
            _ => self.unexpected("a property key"),
        }
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::If)?;
        self.expect_punct(Punct::LParen)?;
        let test = self.parse_expression()?;
        self.expect_punct(Punct::RParen)?;
        let consequent = Box::new(self.parse_statement()?);
        let alternate = if self.eat_keyword(Keyword::Else) {
            Some(Box::new(self.parse_statement()?))
        } else {
            None
        };
        Ok(Stmt::If {
            test,
            consequent,
            alternate,
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::While)?;
        self.expect_punct(Punct::LParen)?;
        let test = self.parse_expression()?;
        self.expect_punct(Punct::RParen)?;
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::While { test, body })
    }

    fn parse_do_while(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::Do)?;
        let body = Box::new(self.parse_statement()?);
        self.expect_keyword(Keyword::While)?;
        self.expect_punct(Punct::LParen)?;
        let test = self.parse_expression()?;
        self.expect_punct(Punct::RParen)?;
        self.semicolon()?;
        Ok(Stmt::DoWhile { body, test })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::For)?;
        self.expect_punct(Punct::LParen)?;

        if self.eat_punct(Punct::Semi) {
            return self.parse_for_tail(None);
        }

        let is_decl = matches!(
            self.kind(),
            TokenKind::Keyword(Keyword::Var)
                | TokenKind::Keyword(Keyword::Let)
                | TokenKind::Keyword(Keyword::Const)
        );

        if is_decl {
            let decl = self.parse_var_decl()?;
            if self.eat_keyword(Keyword::In) {
                let right = self.parse_expression()?;
                return self.finish_for_in(decl, right, false);
            }
            if self.eat_keyword(Keyword::Of) {
                let right = self.parse_assignment()?;
                return self.finish_for_in(decl, right, true);
            }
            self.expect_punct(Punct::Semi)?;
            return self.parse_for_tail(Some(ForInit::Var(decl)));
        }

        let saved = self.no_in;
        self.no_in = true;
        let expr = self.parse_expression()?;
        self.no_in = saved;

        if self.eat_keyword(Keyword::In) {
            let right = self.parse_expression()?;
            return self.finish_for_in_expr(expr, right, false);
        }
        if self.eat_keyword(Keyword::Of) {
            let right = self.parse_assignment()?;
            return self.finish_for_in_expr(expr, right, true);
        }
        self.expect_punct(Punct::Semi)?;
        self.parse_for_tail(Some(ForInit::Expr(expr)))
    }

    fn finish_for_in(
        &mut self,
        decl: VarDecl,
        right: Expr,
        is_of: bool,
    ) -> Result<Stmt, ParseError> {
        if decl.declarations.len() != 1 {
            return self.error("for-in/of declarations must bind exactly one variable");
        }
        let declarator = &decl.declarations[0];
        if declarator.init.is_some() {
            return self.error("for-in/of declaration may not have an initializer");
        }
        let left = if let Some(name) = declarator.pattern.is_simple_identifier() {
            ForTarget::Var {
                kind: decl.kind,
                name: name.to_owned(),
            }
        } else {
            ForTarget::Pattern {
                kind: decl.kind,
                pattern: declarator.pattern.clone(),
            }
        };
        self.expect_punct(Punct::RParen)?;
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::ForIn {
            left,
            right,
            body,
            is_of,
        })
    }

    fn finish_for_in_expr(
        &mut self,
        left: Expr,
        right: Expr,
        is_of: bool,
    ) -> Result<Stmt, ParseError> {
        self.expect_punct(Punct::RParen)?;
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::ForIn {
            left: ForTarget::Expr(left),
            right,
            body,
            is_of,
        })
    }

    fn parse_for_tail(&mut self, init: Option<ForInit>) -> Result<Stmt, ParseError> {
        let test = if self.is_punct(Punct::Semi) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect_punct(Punct::Semi)?;
        let update = if self.is_punct(Punct::RParen) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect_punct(Punct::RParen)?;
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::For {
            init,
            test,
            update,
            body,
        })
    }

    fn parse_switch(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::Switch)?;
        self.expect_punct(Punct::LParen)?;
        let discriminant = self.parse_expression()?;
        self.expect_punct(Punct::RParen)?;
        self.expect_punct(Punct::LBrace)?;
        let mut cases = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            let test = if self.eat_keyword(Keyword::Case) {
                let expr = self.parse_expression()?;
                Some(expr)
            } else if self.eat_keyword(Keyword::Default) {
                None
            } else {
                return self.unexpected("`case`, `default` or `}`");
            };
            self.expect_punct(Punct::Colon)?;
            let mut body = Vec::new();
            while !self.is_punct(Punct::RBrace)
                && !self.is_keyword(Keyword::Case)
                && !self.is_keyword(Keyword::Default)
            {
                if self.at_end() {
                    return self.unexpected("`}`");
                }
                body.push(self.parse_statement()?);
            }
            cases.push(SwitchCase { test, body });
        }
        self.expect_punct(Punct::RBrace)?;
        Ok(Stmt::Switch {
            discriminant,
            cases,
        })
    }

    fn parse_break_continue(&mut self, is_break: bool) -> Result<Stmt, ParseError> {
        if is_break {
            self.expect_keyword(Keyword::Break)?;
        } else {
            self.expect_keyword(Keyword::Continue)?;
        }
        let label = if !self.current().newline_before {
            match self.kind().clone() {
                TokenKind::Identifier(name) => {
                    self.advance();
                    Some(name)
                }
                _ => None,
            }
        } else {
            None
        };
        self.semicolon()?;
        Ok(if is_break {
            Stmt::Break(label)
        } else {
            Stmt::Continue(label)
        })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::Return)?;
        if self.current().newline_before
            || self.is_punct(Punct::Semi)
            || self.is_punct(Punct::RBrace)
            || self.at_end()
        {
            self.semicolon()?;
            return Ok(Stmt::Return(None));
        }
        let value = self.parse_expression()?;
        self.semicolon()?;
        Ok(Stmt::Return(Some(value)))
    }

    fn parse_throw(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::Throw)?;
        if self.current().newline_before {
            return self.error("line terminator not allowed after `throw`");
        }
        let value = self.parse_expression()?;
        self.semicolon()?;
        Ok(Stmt::Throw(value))
    }

    fn parse_try(&mut self) -> Result<Stmt, ParseError> {
        self.expect_keyword(Keyword::Try)?;
        let block = self.parse_block()?;
        let handler = if self.eat_keyword(Keyword::Catch) {
            let param = if self.eat_punct(Punct::LParen) {
                let name = self.binding_identifier()?;
                self.expect_punct(Punct::RParen)?;
                Some(name)
            } else {
                None
            };
            Some(CatchClause {
                param,
                body: self.parse_block()?,
            })
        } else {
            None
        };
        let finalizer = if self.eat_keyword(Keyword::Finally) {
            Some(self.parse_block()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return self.error("missing catch or finally after try");
        }
        Ok(Stmt::Try {
            block,
            handler,
            finalizer,
        })
    }

    fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_expression_inner();
        self.leave_recursion();
        result
    }

    fn parse_expression_inner(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_assignment()?;
        if !self.is_punct(Punct::Comma) {
            return Ok(first);
        }
        let mut expressions = vec![first];
        while self.eat_punct(Punct::Comma) {
            expressions.push(self.parse_assignment()?);
        }
        Ok(Expr::Sequence(expressions))
    }

    fn parse_assignment(&mut self) -> Result<Expr, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_assignment_inner();
        self.leave_recursion();
        result
    }

    fn parse_assignment_inner(&mut self) -> Result<Expr, ParseError> {
        if self.is_punct(Punct::LBrace)
            && let Some(expr) = self.try_parse_object_assignment_pattern()?
        {
            return Ok(expr);
        }
        let target = self.parse_conditional()?;
        let op = match self.kind() {
            TokenKind::Punct(Punct::Eq) => Some(AssignOp::Assign),
            TokenKind::Punct(Punct::PlusEq) => Some(AssignOp::Add),
            TokenKind::Punct(Punct::MinusEq) => Some(AssignOp::Sub),
            TokenKind::Punct(Punct::StarEq) => Some(AssignOp::Mul),
            TokenKind::Punct(Punct::SlashEq) => Some(AssignOp::Div),
            TokenKind::Punct(Punct::PercentEq) => Some(AssignOp::Mod),
            TokenKind::Punct(Punct::StarStarEq) => Some(AssignOp::Exp),
            TokenKind::Punct(Punct::LtLtEq) => Some(AssignOp::Shl),
            TokenKind::Punct(Punct::GtGtEq) => Some(AssignOp::Shr),
            TokenKind::Punct(Punct::GtGtGtEq) => Some(AssignOp::UShr),
            _ => None,
        };
        let Some(op) = op else {
            return Ok(target);
        };
        self.advance();
        if op == AssignOp::Assign
            && matches!(target, Expr::Array(_) | Expr::Object(_))
            && let Some(pattern) = pattern_from_expression(&target)
        {
            let value = self.parse_assignment()?;
            return Ok(Expr::AssignPattern {
                pattern,
                value: Box::new(value),
            });
        }
        let value = self.parse_assignment()?;
        Ok(Expr::Assign {
            op,
            target: Box::new(target),
            value: Box::new(value),
        })
    }

    /// Try to parse `{ pattern } = value` as a destructuring assignment. The
    /// object configuration is ambiguous with an object literal, so this backs
    /// up and restores the cursor when no `=` follows the pattern.
    fn try_parse_object_assignment_pattern(&mut self) -> Result<Option<Expr>, ParseError> {
        let saved = self.pos;
        let saved_no_in = self.no_in;
        let attempt = (|| -> Result<Option<Expr>, ParseError> {
            let pattern = self.parse_assignment_pattern_object()?;
            if !self.eat_punct(Punct::Eq) {
                return Ok(None);
            }
            let value = self.parse_assignment()?;
            Ok(Some(Expr::AssignPattern {
                pattern,
                value: Box::new(value),
            }))
        })();
        match attempt {
            Ok(Some(expr)) => Ok(Some(expr)),
            _ => {
                self.pos = saved;
                self.no_in = saved_no_in;
                Ok(None)
            }
        }
    }

    /// Parse an object destructuring assignment target, allowing member
    /// expressions on the right hand side of `:`.
    fn parse_assignment_pattern_object(&mut self) -> Result<Pattern, ParseError> {
        self.expect_punct(Punct::LBrace)?;
        let mut properties = Vec::new();
        let mut rest = None;
        loop {
            if self.eat_punct(Punct::RBrace) {
                break;
            }
            if self.eat_punct(Punct::Ellipsis) {
                rest = Some(Box::new(self.parse_assignment_pattern_target(false)?));
                self.expect_punct(Punct::RBrace)?;
                break;
            }
            let key = self.parse_property_key()?;
            let value = if self.eat_punct(Punct::Colon) {
                self.parse_assignment_pattern_target(true)?
            } else {
                let PropertyKey::Ident(name) = &key else {
                    return self.error("shorthand object patterns need an identifier key");
                };
                let base = Pattern::Identifier(name.clone());
                if self.eat_punct(Punct::Eq) {
                    Pattern::Default {
                        target: Box::new(base),
                        default: Box::new(self.parse_assignment()?),
                    }
                } else {
                    base
                }
            };
            properties.push(ObjectPatternProperty { key, value });
            if self.eat_punct(Punct::Comma) {
                continue;
            }
            self.expect_punct(Punct::RBrace)?;
            break;
        }
        Ok(Pattern::Object { properties, rest })
    }

    /// Parse a single assignment target: an identifier with member suffixes, or
    /// a nested array/object pattern.
    fn parse_assignment_pattern_target(
        &mut self,
        allow_default: bool,
    ) -> Result<Pattern, ParseError> {
        let base = match self.kind().clone() {
            TokenKind::Punct(Punct::LBracket) => self.parse_binding_pattern_base()?,
            TokenKind::Punct(Punct::LBrace) => self.parse_assignment_pattern_object()?,
            TokenKind::Identifier(name) => {
                self.advance();
                let mut member_expr = Expr::Identifier(name.clone());
                let mut member = false;
                while matches!(self.kind(), TokenKind::Punct(Punct::Dot | Punct::LBracket)) {
                    if self.eat_punct(Punct::Dot) {
                        let property = self.identifier_name()?;
                        member_expr = Expr::Member {
                            object: Box::new(member_expr),
                            property: Box::new(Expr::Identifier(property)),
                            computed: false,
                        };
                    } else {
                        self.expect_punct(Punct::LBracket)?;
                        let property = self.parse_expression()?;
                        self.expect_punct(Punct::RBracket)?;
                        member_expr = Expr::Member {
                            object: Box::new(member_expr),
                            property: Box::new(property),
                            computed: true,
                        };
                    }
                    member = true;
                }
                if member {
                    Pattern::Member(Box::new(member_expr))
                } else {
                    Pattern::Identifier(name)
                }
            }
            _ => return self.unexpected("an assignment target"),
        };
        if allow_default && self.eat_punct(Punct::Eq) {
            return Ok(Pattern::Default {
                target: Box::new(base),
                default: Box::new(self.parse_assignment()?),
            });
        }
        Ok(base)
    }

    fn parse_conditional(&mut self) -> Result<Expr, ParseError> {
        let test = self.parse_binary(0)?;
        if !self.eat_punct(Punct::Question) {
            return Ok(test);
        }
        let consequent = self.parse_assignment()?;
        self.expect_punct(Punct::Colon)?;
        let alternate = self.parse_assignment()?;
        Ok(Expr::Conditional {
            test: Box::new(test),
            consequent: Box::new(consequent),
            alternate: Box::new(alternate),
        })
    }

    fn binary_operator(&self) -> Option<(u8, bool, BinaryKind)> {
        let kind = match self.kind() {
            TokenKind::Punct(punct) => match punct {
                Punct::StarStar => (12, true, BinaryKind::Binary(BinaryOp::Exp)),
                Punct::Star => (11, false, BinaryKind::Binary(BinaryOp::Mul)),
                Punct::Slash => (11, false, BinaryKind::Binary(BinaryOp::Div)),
                Punct::Percent => (11, false, BinaryKind::Binary(BinaryOp::Mod)),
                Punct::Plus => (10, false, BinaryKind::Binary(BinaryOp::Add)),
                Punct::Minus => (10, false, BinaryKind::Binary(BinaryOp::Sub)),
                Punct::LtLt => (9, false, BinaryKind::Binary(BinaryOp::Shl)),
                Punct::GtGt => (9, false, BinaryKind::Binary(BinaryOp::Shr)),
                Punct::GtGtGt => (9, false, BinaryKind::Binary(BinaryOp::UShr)),
                Punct::Lt => (8, false, BinaryKind::Binary(BinaryOp::Lt)),
                Punct::LtEq => (8, false, BinaryKind::Binary(BinaryOp::Le)),
                Punct::Gt => (8, false, BinaryKind::Binary(BinaryOp::Gt)),
                Punct::GtEq => (8, false, BinaryKind::Binary(BinaryOp::Ge)),
                Punct::EqEq => (7, false, BinaryKind::Binary(BinaryOp::Eq)),
                Punct::BangEq => (7, false, BinaryKind::Binary(BinaryOp::Ne)),
                Punct::EqEqEq => (7, false, BinaryKind::Binary(BinaryOp::StrictEq)),
                Punct::BangEqEq => (7, false, BinaryKind::Binary(BinaryOp::StrictNe)),
                Punct::Amp => (6, false, BinaryKind::Binary(BinaryOp::BitAnd)),
                Punct::Caret => (5, false, BinaryKind::Binary(BinaryOp::BitXor)),
                Punct::Pipe => (4, false, BinaryKind::Binary(BinaryOp::BitOr)),
                Punct::AmpAmp => (3, false, BinaryKind::Logical(LogicalOp::And)),
                Punct::PipePipe => (2, false, BinaryKind::Logical(LogicalOp::Or)),
                Punct::QuestionQuestion => (2, false, BinaryKind::Logical(LogicalOp::Nullish)),
                _ => return None,
            },
            TokenKind::Keyword(Keyword::InstanceOf) => {
                (8, false, BinaryKind::Binary(BinaryOp::InstanceOf))
            }
            TokenKind::Keyword(Keyword::In) if !self.no_in => {
                (8, false, BinaryKind::Binary(BinaryOp::In))
            }
            _ => return None,
        };
        Some(kind)
    }

    fn parse_binary(&mut self, min_precedence: u8) -> Result<Expr, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_binary_inner(min_precedence);
        self.leave_recursion();
        result
    }

    fn parse_binary_inner(&mut self, min_precedence: u8) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        let mut seen_nullish = false;
        let mut seen_or_and = false;
        while let Some((precedence, right_assoc, kind)) = self.binary_operator() {
            if precedence < min_precedence {
                break;
            }
            // JavaScript requires parentheses when `??` is mixed with `||`
            // or `&&`; the three operators have equal precedence.
            if let BinaryKind::Logical(op) = kind {
                match op {
                    LogicalOp::Nullish if seen_or_and => {
                        return self
                            .error("`??` cannot be mixed with `||` or `&&` without parentheses");
                    }
                    LogicalOp::And | LogicalOp::Or if seen_nullish => {
                        return self
                            .error("`??` cannot be mixed with `||` or `&&` without parentheses");
                    }
                    _ => {}
                }
            }
            self.advance();
            let next_min = if right_assoc {
                precedence
            } else {
                precedence + 1
            };
            let right = self.parse_binary(next_min)?;
            if let BinaryKind::Logical(op) = kind {
                match op {
                    LogicalOp::Nullish => seen_nullish = true,
                    LogicalOp::And | LogicalOp::Or => seen_or_and = true,
                }
            }
            left = match kind {
                BinaryKind::Binary(op) => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                BinaryKind::Logical(op) => Expr::Logical {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_unary_inner();
        self.leave_recursion();
        result
    }

    fn parse_unary_inner(&mut self) -> Result<Expr, ParseError> {
        let op = match self.kind() {
            TokenKind::Punct(Punct::Plus) => Some(UnaryOp::Plus),
            TokenKind::Punct(Punct::Minus) => Some(UnaryOp::Minus),
            TokenKind::Punct(Punct::Bang) => Some(UnaryOp::Not),
            TokenKind::Punct(Punct::Tilde) => Some(UnaryOp::BitNot),
            TokenKind::Keyword(Keyword::TypeOf) => Some(UnaryOp::TypeOf),
            TokenKind::Keyword(Keyword::Void) => Some(UnaryOp::Void),
            TokenKind::Keyword(Keyword::Delete) => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let argument = self.parse_unary()?;
            return Ok(Expr::Unary {
                op,
                argument: Box::new(argument),
            });
        }
        let update = match self.kind() {
            TokenKind::Punct(Punct::PlusPlus) => Some(UpdateOp::Increment),
            TokenKind::Punct(Punct::MinusMinus) => Some(UpdateOp::Decrement),
            _ => None,
        };
        if let Some(op) = update {
            self.advance();
            let argument = self.parse_unary()?;
            return Ok(Expr::Update {
                op,
                prefix: true,
                argument: Box::new(argument),
            });
        }

        let expr = self.parse_lhs()?;
        let postfix = match self.kind() {
            TokenKind::Punct(Punct::PlusPlus) => Some(UpdateOp::Increment),
            TokenKind::Punct(Punct::MinusMinus) => Some(UpdateOp::Decrement),
            _ => None,
        };
        if let Some(op) = postfix
            && !self.current().newline_before
        {
            self.advance();
            return Ok(Expr::Update {
                op,
                prefix: false,
                argument: Box::new(expr),
            });
        }
        Ok(expr)
    }

    fn parse_lhs(&mut self) -> Result<Expr, ParseError> {
        let expr = if self.is_keyword(Keyword::New) {
            self.parse_new()?
        } else {
            self.parse_primary()?
        };
        self.parse_chain(expr)
    }

    fn parse_new(&mut self) -> Result<Expr, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_new_inner();
        self.leave_recursion();
        result
    }

    fn parse_new_inner(&mut self) -> Result<Expr, ParseError> {
        self.expect_keyword(Keyword::New)?;
        let callee = if self.is_keyword(Keyword::New) {
            self.parse_new()?
        } else {
            let primary = self.parse_primary()?;
            self.parse_member_chain(primary)?
        };
        let arguments = if self.eat_punct(Punct::LParen) {
            self.parse_arguments()?
        } else {
            Vec::new()
        };
        Ok(Expr::New {
            callee: Box::new(callee),
            arguments,
        })
    }

    fn parse_member_chain(&mut self, mut expr: Expr) -> Result<Expr, ParseError> {
        loop {
            if self.eat_punct(Punct::Dot) {
                let name = self.identifier_name()?;
                expr = Expr::Member {
                    object: Box::new(expr),
                    property: Box::new(Expr::Identifier(name)),
                    computed: false,
                };
            } else if self.eat_punct(Punct::LBracket) {
                let property = self.parse_expression()?;
                self.expect_punct(Punct::RBracket)?;
                expr = Expr::Member {
                    object: Box::new(expr),
                    property: Box::new(property),
                    computed: true,
                };
            } else {
                return Ok(expr);
            }
        }
    }

    fn parse_chain(&mut self, mut expr: Expr) -> Result<Expr, ParseError> {
        let mut chain: Option<OptionalChain> = None;
        loop {
            if self.eat_punct(Punct::QuestionDot) {
                if chain.is_none() {
                    chain = Some(OptionalChain {
                        base: Box::new(expr.clone()),
                        ops: Vec::new(),
                    });
                }
                let current = chain.as_mut().expect("optional chain was just created");
                if self.eat_punct(Punct::LParen) {
                    current.ops.push(OptionalChainOp::Call {
                        arguments: self.parse_arguments()?,
                        optional: true,
                    });
                } else {
                    let (property, computed) = if self.eat_punct(Punct::LBracket) {
                        let property = self.parse_expression()?;
                        self.expect_punct(Punct::RBracket)?;
                        (property, true)
                    } else {
                        (Expr::Identifier(self.identifier_name()?), false)
                    };
                    current.ops.push(OptionalChainOp::Member {
                        property: Box::new(property),
                        computed,
                        optional: true,
                    });
                }
                continue;
            }
            if self.eat_punct(Punct::Dot) {
                let property = Expr::Identifier(self.identifier_name()?);
                if let Some(current) = &mut chain {
                    current.ops.push(OptionalChainOp::Member {
                        property: Box::new(property),
                        computed: false,
                        optional: false,
                    });
                } else {
                    expr = Expr::Member {
                        object: Box::new(expr),
                        property: Box::new(property),
                        computed: false,
                    };
                }
            } else if self.eat_punct(Punct::LBracket) {
                let property = self.parse_expression()?;
                self.expect_punct(Punct::RBracket)?;
                if let Some(current) = &mut chain {
                    current.ops.push(OptionalChainOp::Member {
                        property: Box::new(property),
                        computed: true,
                        optional: false,
                    });
                } else {
                    expr = Expr::Member {
                        object: Box::new(expr),
                        property: Box::new(property),
                        computed: true,
                    };
                }
            } else if self.eat_punct(Punct::LParen) {
                let arguments = self.parse_arguments()?;
                if let Some(current) = &mut chain {
                    current.ops.push(OptionalChainOp::Call {
                        arguments,
                        optional: false,
                    });
                } else {
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        arguments,
                    };
                }
            } else {
                break;
            }
        }
        match chain {
            Some(chain) => Ok(Expr::OptionalChain(chain)),
            None => Ok(expr),
        }
    }

    fn parse_arguments(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut arguments = Vec::new();
        if self.eat_punct(Punct::RParen) {
            return Ok(arguments);
        }
        loop {
            let argument = if self.eat_punct(Punct::Ellipsis) {
                Expr::Spread(Box::new(self.parse_assignment()?))
            } else {
                self.parse_assignment()?
            };
            arguments.push(argument);
            if self.eat_punct(Punct::Comma) {
                if self.is_punct(Punct::RParen) {
                    self.advance();
                    return Ok(arguments);
                }
            } else {
                self.expect_punct(Punct::RParen)?;
                return Ok(arguments);
            }
        }
    }

    /// Attempt to parse `( parameters ) =>` as an arrow function. The cursor is
    /// restored when the parenthesised text turns out to be an expression.
    fn try_parse_arrow(&mut self) -> Result<Option<Expr>, ParseError> {
        let saved = self.pos;
        let saved_no_in = self.no_in;
        let position = self.current().position;
        let attempt = (|| -> Result<Expr, ParseError> {
            self.expect_punct(Punct::LParen)?;
            let patterns = self.parse_parameter_patterns()?;
            if self.current().newline_before || !self.is_punct(Punct::Arrow) {
                return self.unexpected("`=>`");
            }
            self.parse_arrow(patterns, position)
        })();
        match attempt {
            Ok(expr) => Ok(Some(expr)),
            Err(_) => {
                self.pos = saved;
                self.no_in = saved_no_in;
                Ok(None)
            }
        }
    }

    /// Parse the parameter list of an arrow function or a parenthesised
    /// expression, then convert it to an arrow function body.
    fn parse_arrow(
        &mut self,
        patterns: Vec<Pattern>,
        position: Position,
    ) -> Result<Expr, ParseError> {
        self.expect_punct(Punct::Arrow)?;
        let body = if self.is_punct(Punct::LBrace) {
            self.parse_block()?
        } else {
            let value = self.parse_assignment()?;
            vec![Stmt::Return(Some(value))]
        };
        Ok(Expr::Arrow(Function {
            name: None,
            params: simple_parameter_names(&patterns),
            patterns,
            body,
            is_arrow: true,
            is_generator: false,
            is_async: false,
            pos: position,
        }))
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        self.enter_recursion()?;
        let result = self.parse_primary_inner();
        self.leave_recursion();
        result
    }

    fn parse_primary_inner(&mut self) -> Result<Expr, ParseError> {
        match self.kind().clone() {
            TokenKind::Number(value) => {
                self.advance();
                Ok(Expr::Number(value))
            }
            TokenKind::String(value) => {
                self.advance();
                Ok(Expr::String(value))
            }
            TokenKind::TemplatePart { cooked, tail } => {
                self.advance();
                self.parse_template(cooked, tail)
            }
            TokenKind::Keyword(Keyword::True) => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.advance();
                Ok(Expr::Null)
            }
            TokenKind::Keyword(Keyword::This) => {
                self.advance();
                Ok(Expr::This)
            }
            TokenKind::Keyword(Keyword::Super) => {
                self.advance();
                Ok(Expr::Super)
            }
            TokenKind::Keyword(Keyword::Class) => Ok(Expr::Class(self.parse_class()?)),
            TokenKind::Keyword(Keyword::Function) => {
                let function = self.parse_function(true, false)?;
                Ok(Expr::Function(function))
            }
            TokenKind::Regex { pattern, flags } => {
                let position = self.current().position;
                Regex::new(&pattern, &flags).map_err(|error| {
                    ParseError::at(
                        format!("invalid regular expression: {}", error.message),
                        position,
                    )
                })?;
                self.advance();
                Ok(Expr::Regex { pattern, flags })
            }
            TokenKind::Identifier(name) => {
                match name.as_str() {
                    "import" if matches!(self.peek_kind(1), TokenKind::Punct(Punct::LParen)) => {
                        self.advance(); // `import`
                        self.advance(); // `(`
                        let argument = self.parse_assignment()?;
                        self.expect_punct(Punct::RParen)?;
                        return Ok(Expr::ImportCall(Box::new(argument)));
                    }
                    "import"
                        if matches!(self.peek_kind(1), TokenKind::Punct(Punct::Dot))
                            && matches!(self.peek_kind(2), TokenKind::Identifier(property) if property == "meta") =>
                    {
                        self.advance(); // `import`
                        self.advance(); // `.`
                        self.advance(); // `meta`
                        return Ok(Expr::MetaProperty {
                            meta: "import".to_owned(),
                            property: "meta".to_owned(),
                        });
                    }
                    "async" => return self.parse_async_primary(),
                    "await" => {
                        self.advance();
                        return Ok(Expr::Await(Box::new(self.parse_unary()?)));
                    }
                    "yield" => return self.parse_yield(),
                    _ => {}
                }
                if matches!(self.peek_kind(1), TokenKind::Punct(Punct::Arrow))
                    && !self.tokens[self.pos + 1].newline_before
                {
                    let position = self.current().position;
                    self.advance();
                    let patterns = vec![Pattern::Identifier(name)];
                    return self.parse_arrow(patterns, position);
                }
                self.advance();
                Ok(Expr::Identifier(name))
            }
            TokenKind::Punct(Punct::LParen) => {
                if let Some(arrow) = self.try_parse_arrow()? {
                    return Ok(arrow);
                }
                self.advance();
                let expr = self.parse_expression()?;
                self.expect_punct(Punct::RParen)?;
                Ok(expr)
            }
            TokenKind::Punct(Punct::LBracket) => self.parse_array_literal(),
            TokenKind::Punct(Punct::LBrace) => self.parse_object_literal(),
            _ => self.unexpected("an expression"),
        }
    }

    /// Parse `async function ...`, `async (...) => ...` or `async x => ...`.
    /// Falls back to the plain identifier `async` when none of those forms
    /// match, so `async` remains usable as a variable name.
    fn parse_async_primary(&mut self) -> Result<Expr, ParseError> {
        let saved = self.pos;
        let position = self.current().position;
        self.advance();
        let attempt = (|| -> Result<Expr, ParseError> {
            if self.is_keyword(Keyword::Function) {
                return Ok(Expr::Function(self.parse_function(true, true)?));
            }
            if self.is_punct(Punct::LParen) {
                self.expect_punct(Punct::LParen)?;
                let patterns = self.parse_parameter_patterns()?;
                if self.current().newline_before || !self.is_punct(Punct::Arrow) {
                    return self.unexpected("`=>`");
                }
                return self.parse_arrow(patterns, position);
            }
            if let TokenKind::Identifier(name) = self.kind().clone()
                && matches!(self.peek_kind(1), TokenKind::Punct(Punct::Arrow))
                && !self.tokens[self.pos + 1].newline_before
            {
                let patterns = vec![Pattern::Identifier(name)];
                self.advance();
                return self.parse_arrow(patterns, position);
            }
            self.unexpected("`function` or `=>` after `async`")
        })();
        match attempt {
            Ok(mut expr) => {
                if let Expr::Arrow(function) | Expr::Function(function) = &mut expr {
                    function.is_async = true;
                }
                Ok(expr)
            }
            Err(_) => {
                self.pos = saved;
                self.advance();
                Ok(Expr::Identifier("async".to_owned()))
            }
        }
    }

    /// Parse a `yield` expression, including `yield*` delegation.
    fn parse_yield(&mut self) -> Result<Expr, ParseError> {
        self.expect_keyword_or_name("yield")?;
        let delegate = self.eat_punct(Punct::Star);
        let has_argument = !self.current().newline_before
            && !self.is_punct(Punct::Semi)
            && !self.is_punct(Punct::RParen)
            && !self.is_punct(Punct::RBracket)
            && !self.is_punct(Punct::RBrace)
            && !self.is_punct(Punct::Comma)
            && !self.is_punct(Punct::Colon)
            && !self.at_end();
        let argument = if has_argument {
            Some(Box::new(self.parse_assignment()?))
        } else {
            None
        };
        Ok(Expr::Yield { argument, delegate })
    }

    /// Consume an identifier-shaped keyword that the lexer reports as an
    /// identifier (used for contextual keywords such as `yield`).
    fn expect_keyword_or_name(&mut self, name: &str) -> Result<(), ParseError> {
        if matches!(self.kind(), TokenKind::Identifier(text) if text == name) {
            self.advance();
            Ok(())
        } else {
            self.unexpected(&format!("`{name}`"))
        }
    }

    fn parse_template(&mut self, first: String, tail: bool) -> Result<Expr, ParseError> {
        let mut parts = vec![first];
        let mut exprs = Vec::new();
        if tail {
            return Ok(Expr::Template { parts, exprs });
        }
        loop {
            exprs.push(self.parse_expression()?);
            let (cooked, tail) = match self.kind().clone() {
                TokenKind::TemplatePart { cooked, tail } => {
                    self.advance();
                    (cooked, tail)
                }
                _ => return self.unexpected("a template literal continuation"),
            };
            parts.push(cooked);
            if tail {
                return Ok(Expr::Template { parts, exprs });
            }
        }
    }

    fn parse_array_literal(&mut self) -> Result<Expr, ParseError> {
        self.expect_punct(Punct::LBracket)?;
        let mut items = Vec::new();
        loop {
            if self.eat_punct(Punct::RBracket) {
                return Ok(Expr::Array(items));
            }
            if self.eat_punct(Punct::Comma) {
                items.push(None);
                continue;
            }
            let value = if self.eat_punct(Punct::Ellipsis) {
                Expr::Spread(Box::new(self.parse_assignment()?))
            } else {
                self.parse_assignment()?
            };
            items.push(Some(value));
            if self.eat_punct(Punct::Comma) {
                continue;
            }
            self.expect_punct(Punct::RBracket)?;
            return Ok(Expr::Array(items));
        }
    }

    fn parse_object_literal(&mut self) -> Result<Expr, ParseError> {
        self.expect_punct(Punct::LBrace)?;
        let mut properties = Vec::new();
        loop {
            if self.eat_punct(Punct::RBrace) {
                return Ok(Expr::Object(properties));
            }
            let position = self.current().position;
            let mut is_generator = self.eat_punct(Punct::Star);
            let mut is_async = false;
            if !is_generator
                && matches!(self.kind(), TokenKind::Identifier(name) if name.as_str() == "async")
                && !self.current().newline_before
            {
                let next = self.peek_kind(1);
                let after_name = self.peek_kind(2);
                let modifier = if matches!(next, TokenKind::Punct(Punct::Star)) {
                    true
                } else if matches!(
                    next,
                    TokenKind::Identifier(_) | TokenKind::Keyword(_) | TokenKind::String(_)
                ) {
                    matches!(after_name, TokenKind::Punct(Punct::LParen | Punct::Star))
                } else {
                    false
                };
                if modifier {
                    self.advance();
                    is_async = true;
                    is_generator = self.eat_punct(Punct::Star);
                }
            }
            let key = self.parse_property_key()?;

            let (value, shorthand) = if self.eat_punct(Punct::Colon) {
                (self.parse_assignment()?, false)
            } else if self.is_punct(Punct::LParen) {
                let patterns = self.parse_parameter_list()?;
                self.expect_punct(Punct::LBrace)?;
                let body = self.parse_block_body()?;
                let name = match &key {
                    PropertyKey::Ident(name) | PropertyKey::String(name) => Some(name.clone()),
                    PropertyKey::Number(n) => Some(crate::ast::number_to_key(*n)),
                    PropertyKey::Computed(_) => None,
                };
                (
                    Expr::Function(Function {
                        name,
                        params: simple_parameter_names(&patterns),
                        patterns,
                        body,
                        is_arrow: false,
                        is_generator,
                        is_async,
                        pos: position,
                    }),
                    false,
                )
            } else if is_generator || is_async {
                return self.unexpected("`(`");
            } else {
                match &key {
                    PropertyKey::Ident(name) => (Expr::Identifier(name.clone()), true),
                    _ => return self.error("shorthand properties need an identifier key"),
                }
            };

            properties.push(Property {
                key,
                value,
                shorthand,
                pos: position,
            });
            if self.eat_punct(Punct::Comma) {
                continue;
            }
            self.expect_punct(Punct::RBrace)?;
            return Ok(Expr::Object(properties));
        }
    }

    fn parse_parameter_list(&mut self) -> Result<Vec<Pattern>, ParseError> {
        self.expect_punct(Punct::LParen)?;
        self.parse_parameter_patterns()
    }

    /// Parse parameters with the opening parenthesis already consumed.
    fn parse_parameter_patterns(&mut self) -> Result<Vec<Pattern>, ParseError> {
        let mut patterns = Vec::new();
        if self.eat_punct(Punct::RParen) {
            return Ok(patterns);
        }
        loop {
            if self.eat_punct(Punct::Ellipsis) {
                let target = self.parse_binding_pattern(false)?;
                patterns.push(Pattern::Rest(Box::new(target)));
                self.expect_punct(Punct::RParen)?;
                return Ok(patterns);
            }
            patterns.push(self.parse_binding_pattern(true)?);
            if self.eat_punct(Punct::Comma) {
                continue;
            }
            self.expect_punct(Punct::RParen)?;
            return Ok(patterns);
        }
    }

    fn parse_function(
        &mut self,
        is_expression: bool,
        is_async: bool,
    ) -> Result<Function, ParseError> {
        let position = self.current().position;
        self.expect_keyword(Keyword::Function)?;
        let is_generator = self.eat_punct(Punct::Star);
        let name = match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Some(name)
            }
            _ => {
                if !is_expression {
                    return self.unexpected("a function name");
                }
                None
            }
        };
        let patterns = self.parse_parameter_list()?;
        self.expect_punct(Punct::LBrace)?;
        let body = self.parse_block_body()?;
        Ok(Function {
            name,
            params: simple_parameter_names(&patterns),
            patterns,
            body,
            is_arrow: false,
            is_generator,
            is_async,
            pos: position,
        })
    }

    /// Parse a class declaration or expression. The `class` keyword has not
    /// been consumed yet.
    fn parse_class(&mut self) -> Result<Class, ParseError> {
        let pos = self.current().position;
        self.expect_keyword(Keyword::Class)?;
        let name = match self.kind().clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Some(name)
            }
            _ => None,
        };
        let super_class = if self.eat_keyword(Keyword::Extends) {
            Some(Box::new(self.parse_lhs()?))
        } else {
            None
        };
        self.expect_punct(Punct::LBrace)?;
        let mut body = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            if self.at_end() {
                return self.unexpected("`}`");
            }
            body.push(self.parse_class_member()?);
        }
        self.expect_punct(Punct::RBrace)?;
        Ok(Class {
            name,
            super_class,
            body,
            pos,
        })
    }

    /// Parse a single class body member.
    fn parse_class_member(&mut self) -> Result<ClassMember, ParseError> {
        while self.eat_punct(Punct::Semi) {}
        let position = self.current().position;
        let is_static = self.eat_keyword(Keyword::Static);
        let mut is_generator = self.eat_punct(Punct::Star);
        let mut is_async = false;
        if !is_generator
            && matches!(self.kind(), TokenKind::Identifier(name) if name == "async")
            && !self.current().newline_before
        {
            // `async` is a modifier only when a method name follows it. A
            // method literally named `async` is followed directly by `(`.
            let next = self.peek_kind(1);
            let after_name = self.peek_kind(2);
            let modifier = if matches!(next, TokenKind::Punct(Punct::Star)) {
                true
            } else if matches!(
                next,
                TokenKind::Identifier(_)
                    | TokenKind::Keyword(_)
                    | TokenKind::String(_)
                    | TokenKind::Number(_)
            ) {
                matches!(after_name, TokenKind::Punct(Punct::LParen | Punct::Star))
            } else {
                false
            };
            if modifier {
                self.advance();
                is_async = true;
                is_generator = self.eat_punct(Punct::Star);
            }
        }
        let mut kind = MethodKind::Method;
        if matches!(self.kind(), TokenKind::Identifier(name) if name.as_str() == "get" || name.as_str() == "set")
            && !matches!(
                self.peek_kind(1),
                TokenKind::Punct(Punct::LParen | Punct::Star | Punct::Semi)
            )
        {
            kind = if matches!(self.kind(), TokenKind::Identifier(name) if name == "get") {
                MethodKind::Getter
            } else {
                MethodKind::Setter
            };
            self.advance();
        }
        let key = self.parse_property_key()?;
        let patterns = self.parse_parameter_list()?;
        self.expect_punct(Punct::LBrace)?;
        let body = self.parse_block_body()?;
        let name = key.as_key_string();
        Ok(ClassMember::Method {
            key,
            function: Function {
                name: Some(name),
                params: simple_parameter_names(&patterns),
                patterns,
                body,
                is_arrow: false,
                is_generator,
                is_async,
                pos: position,
            },
            is_static,
            kind,
        })
    }
}

/// The simple identifier names of a parameter list, omitting patterns.
fn simple_parameter_names(patterns: &[Pattern]) -> Vec<String> {
    patterns
        .iter()
        .filter_map(|pattern| match pattern {
            Pattern::Identifier(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Convert an expression that was parsed as an array or object literal into a
/// destructuring assignment pattern, when possible.
fn pattern_from_expression(expr: &Expr) -> Option<Pattern> {
    match expr {
        Expr::Identifier(name) => Some(Pattern::Identifier(name.clone())),
        Expr::Member { .. } => Some(Pattern::Member(Box::new(expr.clone()))),
        Expr::Array(items) => {
            let mut elements = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    None => elements.push(None),
                    Some(item) => elements.push(Some(assignment_pattern_from_element(item)?)),
                }
            }
            Some(Pattern::Array(elements))
        }
        Expr::Object(properties) => {
            let mut converted = Vec::with_capacity(properties.len());
            for property in properties {
                let value = assignment_pattern_from_element(&property.value)?;
                converted.push(ObjectPatternProperty {
                    key: property.key.clone(),
                    value,
                });
            }
            Some(Pattern::Object {
                properties: converted,
                rest: None,
            })
        }
        _ => None,
    }
}

fn assignment_pattern_from_element(expr: &Expr) -> Option<Pattern> {
    if let Expr::Assign {
        op: AssignOp::Assign,
        target,
        value,
    } = expr
    {
        return Some(Pattern::Default {
            target: Box::new(pattern_from_expression(target)?),
            default: value.clone(),
        });
    }
    pattern_from_expression(expr)
}

#[derive(Clone, Copy)]
enum BinaryKind {
    Binary(BinaryOp),
    Logical(LogicalOp),
}
