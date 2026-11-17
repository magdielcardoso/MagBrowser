//! JavaScript lexer, parser and syntax tree.
//!
//! This crate implements a dependency free lexer and recursive descent parser for
//! an ES5.1 subset of JavaScript extended with `let`/`const`, arrow functions,
//! template literals and `for...of`.

#![forbid(unsafe_code)]

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod regexp;
pub mod syntax;

pub use ast::{
    AssignOp, BinaryOp, CatchClause, DeclKind, Expr, ForInit, ForTarget, Function, LogicalOp,
    ObjectPatternProperty, Pattern, Position, Program, Property, PropertyKey, Stmt, SwitchCase,
    UnaryOp, UpdateOp, VarDecl, VarDeclarator,
};
pub use lexer::{Lexer, Punct, Token, TokenKind, tokenize};
pub use parser::{ParseError, Parser, parse};
pub use regexp::{Regex, RegexError, RegexFlags, RegexMatch};
pub use syntax::{Keyword, is_keyword};

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(source: &str) -> Program {
        parse(source).unwrap_or_else(|error| panic!("parse failed for {source:?}: {error}"))
    }

    fn expr(source: &str) -> Expr {
        match parse_ok(source).body.into_iter().next() {
            Some(Stmt::Expression(expr)) => expr,
            other => panic!("expected expression statement, got {other:?}"),
        }
    }

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .expect("tokenize")
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn lexes_decimal_and_hex_numbers() {
        let tokens = kinds("0x10 1.5 1e3 .5 7 2E-2");
        assert_eq!(
            tokens,
            vec![
                TokenKind::Number(16.0),
                TokenKind::Number(1.5),
                TokenKind::Number(1000.0),
                TokenKind::Number(0.5),
                TokenKind::Number(7.0),
                TokenKind::Number(0.02),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_string_escapes() {
        let tokens = kinds(r#"'a\nb' "q\"q" '\u0041' '\x41' '\\'"#);
        assert_eq!(
            tokens,
            vec![
                TokenKind::String("a\nb".to_owned()),
                TokenKind::String("q\"q".to_owned()),
                TokenKind::String("A".to_owned()),
                TokenKind::String("A".to_owned()),
                TokenKind::String("\\".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_comments_and_tracks_newlines() {
        let tokens = tokenize("a // trailing\nb /* block\ncomment */ c").expect("tokenize");
        let names: Vec<_> = tokens
            .iter()
            .filter_map(|token| match &token.kind {
                TokenKind::Identifier(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(tokens[1].newline_before);
        assert!(tokens[2].newline_before);
    }

    #[test]
    fn lexes_template_parts() {
        assert_eq!(
            kinds("`a${b}c`"),
            vec![
                TokenKind::TemplatePart {
                    cooked: "a".to_owned(),
                    tail: false,
                },
                TokenKind::Identifier("b".to_owned()),
                TokenKind::TemplatePart {
                    cooked: "c".to_owned(),
                    tail: true,
                },
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_multi_character_operators() {
        assert_eq!(
            kinds("=== !== <= >= && || ?? ** => ++ -- += ..."),
            vec![
                TokenKind::Punct(Punct::EqEqEq),
                TokenKind::Punct(Punct::BangEqEq),
                TokenKind::Punct(Punct::LtEq),
                TokenKind::Punct(Punct::GtEq),
                TokenKind::Punct(Punct::AmpAmp),
                TokenKind::Punct(Punct::PipePipe),
                TokenKind::Punct(Punct::QuestionQuestion),
                TokenKind::Punct(Punct::StarStar),
                TokenKind::Punct(Punct::Arrow),
                TokenKind::Punct(Punct::PlusPlus),
                TokenKind::Punct(Punct::MinusMinus),
                TokenKind::Punct(Punct::PlusEq),
                TokenKind::Punct(Punct::Ellipsis),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn reports_unterminated_string() {
        let error = tokenize("'abc").expect_err("should fail");
        assert!(error.message.contains("unterminated"));
        assert_eq!(error.line, 1);
    }

    #[test]
    fn reports_unterminated_block_comment() {
        let error = tokenize("/* abc").expect_err("should fail");
        assert!(error.message.contains("unterminated block comment"));
    }

    #[test]
    fn multiplies_before_adding() {
        match expr("1 + 2 * 3") {
            Expr::Binary {
                op: BinaryOp::Add,
                right,
                ..
            } => match *right {
                Expr::Binary {
                    op: BinaryOp::Mul, ..
                } => {}
                other => panic!("expected multiply, got {other:?}"),
            },
            other => panic!("expected add, got {other:?}"),
        }
    }

    #[test]
    fn exponentiation_is_right_associative() {
        match expr("2 ** 3 ** 2") {
            Expr::Binary {
                op: BinaryOp::Exp,
                right,
                ..
            } => match *right {
                Expr::Binary {
                    op: BinaryOp::Exp, ..
                } => {}
                other => panic!("expected nested exponentiation, got {other:?}"),
            },
            other => panic!("expected exponentiation, got {other:?}"),
        }
    }

    #[test]
    fn logical_precedence() {
        match expr("a || b && c") {
            Expr::Logical {
                op: LogicalOp::Or,
                right,
                ..
            } => match *right {
                Expr::Logical {
                    op: LogicalOp::And, ..
                } => {}
                other => panic!("expected &&, got {other:?}"),
            },
            other => panic!("expected ||, got {other:?}"),
        }
    }

    #[test]
    fn comparisons_bind_looser_than_arithmetic() {
        match expr("1 + 2 < 4") {
            Expr::Binary {
                op: BinaryOp::Lt,
                left,
                ..
            } => match *left {
                Expr::Binary {
                    op: BinaryOp::Add, ..
                } => {}
                other => panic!("expected add, got {other:?}"),
            },
            other => panic!("expected less-than, got {other:?}"),
        }
    }

    #[test]
    fn assignment_is_right_associative() {
        match expr("a = b = 1") {
            Expr::Assign { value, .. } => match *value {
                Expr::Assign {
                    op: AssignOp::Assign,
                    ..
                } => {}
                other => panic!("expected nested assignment, got {other:?}"),
            },
            other => panic!("expected assignment, got {other:?}"),
        }
    }

    #[test]
    fn compound_assignment_and_conditional() {
        assert!(matches!(
            expr("x += ok ? 1 : 2"),
            Expr::Assign {
                op: AssignOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn sequences_collect_expressions() {
        match expr("a, b, c") {
            Expr::Sequence(items) => assert_eq!(items.len(), 3),
            other => panic!("expected sequence, got {other:?}"),
        }
    }

    #[test]
    fn automatic_semicolon_insertion_before_brace_and_eof() {
        let program = parse_ok("a\nb");
        assert_eq!(program.body.len(), 2);
        let program = parse_ok("if (a) { b }");
        assert_eq!(program.body.len(), 1);
        let program = parse_ok("function f() { return 1 }");
        assert_eq!(program.body.len(), 1);
    }

    #[test]
    fn return_does_not_continue_across_newline() {
        let program = parse_ok("function f() { return\n42 }");
        let Stmt::FunctionDecl(function) = &program.body[0] else {
            panic!("expected function declaration");
        };
        assert_eq!(function.body[0], Stmt::Return(None));
        assert!(matches!(
            function.body[1],
            Stmt::Expression(Expr::Number(_))
        ));
    }

    #[test]
    fn parses_function_declaration() {
        let program = parse_ok("function add(a, b) { return a + b; }");
        let Stmt::FunctionDecl(function) = &program.body[0] else {
            panic!("expected function declaration");
        };
        assert_eq!(function.name.as_deref(), Some("add"));
        assert_eq!(function.params, vec!["a", "b"]);
        assert!(!function.is_arrow);
        assert_eq!(function.pos.line, 1);
    }

    #[test]
    fn parses_anonymous_function_expression() {
        assert!(matches!(
            expr("(function (x) { return x; })"),
            Expr::Function(Function { name: None, .. })
        ));
    }

    #[test]
    fn parses_arrow_expression_body() {
        match expr("x => x * 2") {
            Expr::Arrow(function) => {
                assert!(function.is_arrow);
                assert_eq!(function.params, vec!["x"]);
                assert_eq!(function.body.len(), 1);
            }
            other => panic!("expected arrow, got {other:?}"),
        }
    }

    #[test]
    fn parses_arrow_block_body_and_empty_params() {
        match expr("() => { let y = 1; return y; }") {
            Expr::Arrow(function) => {
                assert_eq!(function.params.len(), 0);
                assert_eq!(function.body.len(), 2);
            }
            other => panic!("expected arrow, got {other:?}"),
        }
        match expr("(a, b) => a + b") {
            Expr::Arrow(function) => assert_eq!(function.params, vec!["a", "b"]),
            other => panic!("expected arrow, got {other:?}"),
        }
    }

    #[test]
    fn parenthesized_expression_is_not_an_arrow() {
        assert!(matches!(expr("(a + b) * c"), Expr::Binary { .. }));
    }

    #[test]
    fn parses_object_literals_with_all_key_kinds() {
        let Expr::Object(properties) = expr("({ a: 1, 'b': 2, 3: 4, c, m(x) { return x; } })")
        else {
            panic!("expected object literal");
        };
        assert_eq!(properties.len(), 5);
        assert!(matches!(properties[0].key, PropertyKey::Ident(_)));
        assert!(matches!(properties[1].key, PropertyKey::String(_)));
        assert!(matches!(properties[2].key, PropertyKey::Number(_)));
        assert!(properties[3].shorthand);
        assert!(matches!(properties[4].value, Expr::Function(_)));
    }

    #[test]
    fn parses_array_literals_with_holes() {
        match expr("[1, , 3,]") {
            Expr::Array(items) => {
                assert_eq!(items.len(), 3);
                assert!(items[1].is_none());
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn parses_try_catch_finally() {
        let program = parse_ok("try { f(); } catch (e) { g(e); } finally { h(); }");
        let Stmt::Try {
            handler, finalizer, ..
        } = &program.body[0]
        else {
            panic!("expected try statement");
        };
        let handler = handler.as_ref().expect("catch clause");
        assert_eq!(handler.param.as_deref(), Some("e"));
        assert!(finalizer.is_some());
    }

    #[test]
    fn parses_for_of() {
        let program = parse_ok("for (const item of items) { total += item; }");
        let Stmt::ForIn {
            left, is_of, right, ..
        } = &program.body[0]
        else {
            panic!("expected for-of");
        };
        assert!(is_of);
        assert!(matches!(right, Expr::Identifier(name) if name == "items"));
        assert!(matches!(
            left,
            ForTarget::Var {
                kind: DeclKind::Const,
                name,
            } if name == "item"
        ));
    }

    #[test]
    fn parses_for_in_and_classic_for() {
        let program = parse_ok("for (var key in object) { log(key); }");
        assert!(matches!(program.body[0], Stmt::ForIn { is_of: false, .. }));
        let program = parse_ok("for (let i = 0; i < 10; i++) { sum += i; }");
        let Stmt::For {
            init, test, update, ..
        } = &program.body[0]
        else {
            panic!("expected for");
        };
        assert!(matches!(init, Some(ForInit::Var(_))));
        assert!(test.is_some());
        assert!(matches!(update, Some(Expr::Update { .. })));
    }

    #[test]
    fn parses_switch_with_default() {
        let program = parse_ok("switch (x) { case 1: a(); break; default: b(); }");
        let Stmt::Switch { cases, .. } = &program.body[0] else {
            panic!("expected switch");
        };
        assert_eq!(cases.len(), 2);
        assert!(cases[1].test.is_none());
    }

    #[test]
    fn parses_labeled_loops_and_break() {
        let program = parse_ok("outer: for (;;) { break outer; }");
        let Stmt::Labeled { label, body } = &program.body[0] else {
            panic!("expected labeled statement");
        };
        assert_eq!(label, "outer");
        let Stmt::For { body, .. } = body.as_ref() else {
            panic!("expected loop");
        };
        assert!(
            matches!(body.as_ref(), Stmt::Block(items) if items[0] == Stmt::Break(Some("outer".to_owned())))
        );
    }

    #[test]
    fn parses_unary_update_and_delete() {
        assert!(matches!(
            expr("typeof x"),
            Expr::Unary {
                op: UnaryOp::TypeOf,
                ..
            }
        ));
        assert!(matches!(
            expr("!x"),
            Expr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
        assert!(matches!(
            expr("x++"),
            Expr::Update {
                op: UpdateOp::Increment,
                prefix: false,
                ..
            }
        ));
        assert!(matches!(
            expr("--x"),
            Expr::Update {
                op: UpdateOp::Decrement,
                prefix: true,
                ..
            }
        ));
        assert!(matches!(
            expr("delete obj.key"),
            Expr::Unary {
                op: UnaryOp::Delete,
                ..
            }
        ));
    }

    #[test]
    fn parses_new_and_calls() {
        match expr("new Foo(1).bar()") {
            Expr::Call { callee, .. } => match *callee {
                Expr::Member { object, .. } => {
                    assert!(matches!(*object, Expr::New { .. }));
                }
                other => panic!("expected member, got {other:?}"),
            },
            other => panic!("expected call, got {other:?}"),
        }
    }

    #[test]
    fn parses_template_literal_with_expression() {
        match expr("`hello ${name}!`") {
            Expr::Template { parts, exprs } => {
                assert_eq!(parts, vec!["hello ", "!"]);
                assert_eq!(exprs.len(), 1);
            }
            other => panic!("expected template, got {other:?}"),
        }
    }

    #[test]
    fn template_expression_can_contain_object_and_nested_braces() {
        match expr("`v=${ {a: 1}.a }`") {
            Expr::Template { exprs, .. } => assert_eq!(exprs.len(), 1),
            other => panic!("expected template, got {other:?}"),
        }
    }

    #[test]
    fn parses_let_and_const_declarations() {
        let program = parse_ok("let a = 1; const b = 2;");
        assert!(matches!(
            program.body[0],
            Stmt::VarDecl(VarDecl {
                kind: DeclKind::Let,
                ..
            })
        ));
        assert!(matches!(
            program.body[1],
            Stmt::VarDecl(VarDecl {
                kind: DeclKind::Const,
                ..
            })
        ));
    }

    #[test]
    fn parses_throw_and_do_while() {
        let program = parse_ok("throw new Error('x');");
        assert!(matches!(program.body[0], Stmt::Throw(_)));
        let program = parse_ok("do { n++; } while (n < 3);");
        assert!(matches!(program.body[0], Stmt::DoWhile { .. }));
    }

    #[test]
    fn parses_empty_statement_and_block() {
        let program = parse_ok(";{}");
        assert_eq!(program.body[0], Stmt::Empty);
        assert!(matches!(program.body[1], Stmt::Block(_)));
    }

    #[test]
    fn reports_position_of_syntax_error() {
        let error = parse("let x = ;").expect_err("should fail");
        assert_eq!(error.line, 1);
        assert!(error.message.contains("expected"));
        assert_eq!(
            error.to_string(),
            format!("{} (line 1, column 9)", error.message)
        );
    }

    #[test]
    fn reports_error_on_second_line() {
        let error = parse("let x = 1;\nlet = 2;").expect_err("should fail");
        assert_eq!(error.line, 2);
    }

    #[test]
    fn parse_error_display_includes_position() {
        let error = ParseError::new("boom", 3, 4);
        assert_eq!(error.to_string(), "boom (line 3, column 4)");
    }

    #[test]
    fn keywords_are_not_binding_identifiers() {
        assert!(parse("var if = 1;").is_err());
    }

    #[test]
    fn parses_member_access_with_keyword_name() {
        assert!(matches!(
            expr("obj.default"),
            Expr::Member {
                computed: false,
                ..
            }
        ));
    }

    #[test]
    fn token_positions_are_one_based() {
        let tokens = tokenize("a\n  b").expect("tokenize");
        assert_eq!(tokens[0].position.line, 1);
        assert_eq!(tokens[0].position.column, 1);
        assert_eq!(tokens[1].position.line, 2);
        assert_eq!(tokens[1].position.column, 3);
    }

    // -----------------------------------------------------------------------
    // Bit shifts
    // -----------------------------------------------------------------------

    #[test]
    fn lexes_shift_operators() {
        assert_eq!(
            kinds("<< >> >>> <<= >>= >>>="),
            vec![
                TokenKind::Punct(Punct::LtLt),
                TokenKind::Punct(Punct::GtGt),
                TokenKind::Punct(Punct::GtGtGt),
                TokenKind::Punct(Punct::LtLtEq),
                TokenKind::Punct(Punct::GtGtEq),
                TokenKind::Punct(Punct::GtGtGtEq),
                TokenKind::Eof,
            ]
        );
        // `>=` and `>>` stay distinct.
        assert_eq!(
            kinds("a >= b >> c"),
            vec![
                TokenKind::Identifier("a".to_owned()),
                TokenKind::Punct(Punct::GtEq),
                TokenKind::Identifier("b".to_owned()),
                TokenKind::Punct(Punct::GtGt),
                TokenKind::Identifier("c".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn shift_binds_looser_than_addition() {
        match expr("a << 1 + 2") {
            Expr::Binary {
                op: BinaryOp::Shl,
                right,
                ..
            } => assert!(matches!(
                *right,
                Expr::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            )),
            other => panic!("expected shift, got {other:?}"),
        }
        match expr("1 < 2 << 3") {
            Expr::Binary {
                op: BinaryOp::Lt,
                right,
                ..
            } => assert!(matches!(
                *right,
                Expr::Binary {
                    op: BinaryOp::Shl,
                    ..
                }
            )),
            other => panic!("expected less-than, got {other:?}"),
        }
    }

    #[test]
    fn parses_shift_assignment_operators() {
        assert!(matches!(
            expr("a <<= 1"),
            Expr::Assign {
                op: AssignOp::Shl,
                ..
            }
        ));
        assert!(matches!(
            expr("a >>= 1"),
            Expr::Assign {
                op: AssignOp::Shr,
                ..
            }
        ));
        assert!(matches!(
            expr("a >>>= 1"),
            Expr::Assign {
                op: AssignOp::UShr,
                ..
            }
        ));
        assert!(matches!(
            expr("a >>> b"),
            Expr::Binary {
                op: BinaryOp::UShr,
                ..
            }
        ));
    }

    // -----------------------------------------------------------------------
    // Regular expression literals
    // -----------------------------------------------------------------------

    #[test]
    fn lexes_regex_literals() {
        assert_eq!(
            kinds("/abc/g"),
            vec![
                TokenKind::Regex {
                    pattern: "abc".to_owned(),
                    flags: "g".to_owned(),
                },
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds(r"/a\/b[\]x]/im"),
            vec![
                TokenKind::Regex {
                    pattern: r"a\/b[\]x]".to_owned(),
                    flags: "im".to_owned(),
                },
                TokenKind::Eof,
            ]
        );
        // At an expression start `/=` begins a regex, not an assignment.
        assert_eq!(
            kinds("/=/"),
            vec![
                TokenKind::Regex {
                    pattern: "=".to_owned(),
                    flags: String::new(),
                },
                TokenKind::Eof,
            ]
        );
        let tokens = kinds("a /= 2");
        assert!(matches!(tokens[1], TokenKind::Punct(Punct::SlashEq)));
    }

    #[test]
    fn slash_after_operand_is_division() {
        let tokens = kinds("a / b");
        assert!(matches!(tokens[1], TokenKind::Punct(Punct::Slash)));
        let tokens = kinds("1 / 2");
        assert!(matches!(tokens[1], TokenKind::Punct(Punct::Slash)));
        let tokens = kinds("f(x) / 2");
        assert!(matches!(tokens[4], TokenKind::Punct(Punct::Slash)));
        let tokens = kinds("arr[0] / 2");
        assert!(matches!(tokens[4], TokenKind::Punct(Punct::Slash)));
        let tokens = kinds("'s' / 2");
        assert!(matches!(tokens[1], TokenKind::Punct(Punct::Slash)));
        assert!(!matches!(tokens[1], TokenKind::Punct(Punct::SlashEq)));
    }

    #[test]
    fn slash_after_operators_starts_a_regex() {
        for source in [
            "x = /a/",
            "x + /a/",
            "(/a/)",
            "[ /a/ ]",
            "return /a/",
            "a && /b/",
        ] {
            let tokens = kinds(source);
            assert!(
                tokens.iter().any(|token| matches!(
                    token,
                    TokenKind::Regex { pattern, .. } if pattern == "a" || pattern == "b"
                )),
                "expected a regex token in {source:?}, got {tokens:?}"
            );
        }
    }

    #[test]
    fn regex_allowed_after_control_paren() {
        let tokens = kinds("if (x) /a/.test(s)");
        assert!(
            tokens
                .iter()
                .any(|token| matches!(token, TokenKind::Regex { .. }))
        );
        let tokens = kinds("while (x) /a/.test(s)");
        assert!(
            tokens
                .iter()
                .any(|token| matches!(token, TokenKind::Regex { .. }))
        );
        // A call parenthesis ends an operand, so `/` is division there.
        let tokens = kinds("f(x) / a");
        assert!(
            !tokens
                .iter()
                .any(|token| matches!(token, TokenKind::Regex { .. }))
        );
    }

    #[test]
    fn parses_regex_literal_expression() {
        match expr("/ab+c/gi") {
            Expr::Regex { pattern, flags } => {
                assert_eq!(pattern, "ab+c");
                assert_eq!(flags, "gi");
            }
            other => panic!("expected regex, got {other:?}"),
        }
    }

    #[test]
    fn invalid_regex_literal_is_a_parse_error() {
        let error = parse("/a(/").expect_err("invalid regex");
        assert!(error.message.contains("invalid regular expression"));
        assert!(parse("/[abc/").is_err());
        assert!(parse("/a/qq").is_err());
        assert!(parse("/a/gg").is_err());
    }

    #[test]
    fn unterminated_regex_literal_is_a_parse_error() {
        let error = parse("var re = /abc").expect_err("unterminated regex");
        assert!(error.message.contains("unterminated"));
    }

    // -----------------------------------------------------------------------
    // Destructuring
    // -----------------------------------------------------------------------

    #[test]
    fn parses_array_destructuring_declaration() {
        let program = parse_ok("var [a, , b = 1, ...rest] = items;");
        let Stmt::VarDecl(declaration) = &program.body[0] else {
            panic!("expected declaration");
        };
        let Pattern::Array(elements) = &declaration.declarations[0].pattern else {
            panic!("expected array pattern");
        };
        assert_eq!(elements.len(), 4);
        assert!(elements[1].is_none());
        assert!(matches!(elements[2], Some(Pattern::Default { .. })));
        assert!(matches!(elements[3], Some(Pattern::Rest(_))));
    }

    #[test]
    fn parses_object_destructuring_declaration() {
        let program = parse_ok("let {a, b: c, d = 2, ...rest} = source;");
        let Stmt::VarDecl(declaration) = &program.body[0] else {
            panic!("expected declaration");
        };
        let Pattern::Object { properties, rest } = &declaration.declarations[0].pattern else {
            panic!("expected object pattern");
        };
        assert_eq!(properties.len(), 3);
        assert!(matches!(properties[0].value, Pattern::Identifier(ref name) if name == "a"));
        assert!(matches!(properties[1].value, Pattern::Identifier(ref name) if name == "c"));
        assert!(matches!(properties[2].value, Pattern::Default { .. }));
        assert!(rest.is_some());
    }

    #[test]
    fn parses_nested_destructuring_patterns() {
        let program = parse_ok("var {a: [b, {c = 3}]} = source;");
        let Stmt::VarDecl(declaration) = &program.body[0] else {
            panic!("expected declaration");
        };
        let Pattern::Object { properties, .. } = &declaration.declarations[0].pattern else {
            panic!("expected object pattern");
        };
        let Pattern::Array(elements) = &properties[0].value else {
            panic!("expected nested array pattern");
        };
        assert_eq!(elements.len(), 2);
        assert!(matches!(elements[1], Some(Pattern::Object { .. })));
    }

    #[test]
    fn parses_destructuring_assignment_expressions() {
        match expr("[a, b = 2] = [b, a]") {
            Expr::AssignPattern { pattern, .. } => {
                let Pattern::Array(elements) = pattern else {
                    panic!("expected array pattern");
                };
                assert_eq!(elements.len(), 2);
                assert!(matches!(elements[1], Some(Pattern::Default { .. })));
            }
            other => panic!("expected destructuring assignment, got {other:?}"),
        }
        match expr("({a, b: c = 1} = source)") {
            Expr::AssignPattern { pattern, .. } => {
                let Pattern::Object { properties, .. } = pattern else {
                    panic!("expected object pattern");
                };
                assert_eq!(properties.len(), 2);
            }
            other => panic!("expected destructuring assignment, got {other:?}"),
        }
    }

    #[test]
    fn simple_assignments_remain_plain_assignments() {
        assert!(matches!(
            expr("a = 1"),
            Expr::Assign {
                op: AssignOp::Assign,
                ..
            }
        ));
        assert!(matches!(
            expr("obj.x = 1"),
            Expr::Assign {
                op: AssignOp::Assign,
                ..
            }
        ));
    }

    #[test]
    fn rejects_malformed_patterns() {
        assert!(parse("var [a = ;").is_err());
        assert!(parse("var {a: } = x;").is_err());
        assert!(parse("var [...a, b] = x;").is_err());
        assert!(parse("var {a} = ;").is_err());
    }

    // -----------------------------------------------------------------------
    // Default and rest parameters
    // -----------------------------------------------------------------------

    #[test]
    fn parses_default_parameters() {
        let program = parse_ok("function f(a, b = 2, c = a + b) { return c; }");
        let Stmt::FunctionDecl(function) = &program.body[0] else {
            panic!("expected function");
        };
        assert_eq!(function.patterns.len(), 3);
        assert!(matches!(function.patterns[0], Pattern::Identifier(_)));
        assert!(matches!(function.patterns[1], Pattern::Default { .. }));
        assert!(matches!(function.patterns[2], Pattern::Default { .. }));
    }

    #[test]
    fn parses_rest_parameters() {
        let program = parse_ok("function f(first, ...rest) { return rest; }");
        let Stmt::FunctionDecl(function) = &program.body[0] else {
            panic!("expected function");
        };
        assert_eq!(function.params, vec!["first"]);
        assert!(matches!(function.patterns[1], Pattern::Rest(_)));
    }

    #[test]
    fn parses_destructuring_and_default_arrow_parameters() {
        match expr("([a, b = 1], {c}) => a + b + c") {
            Expr::Arrow(function) => {
                assert_eq!(function.patterns.len(), 2);
                assert!(matches!(function.patterns[0], Pattern::Array(_)));
                assert!(matches!(function.patterns[1], Pattern::Object { .. }));
            }
            other => panic!("expected arrow, got {other:?}"),
        }
        match expr("(...args) => args.length") {
            Expr::Arrow(function) => {
                assert!(matches!(function.patterns[0], Pattern::Rest(_)));
            }
            other => panic!("expected arrow, got {other:?}"),
        }
        match expr("(a = 5) => a") {
            Expr::Arrow(function) => {
                assert!(matches!(function.patterns[0], Pattern::Default { .. }));
            }
            other => panic!("expected arrow, got {other:?}"),
        }
    }

    #[test]
    fn parenthesized_expressions_still_parse_after_arrow_attempt() {
        assert!(matches!(expr("(a, b)"), Expr::Sequence(_)));
        assert!(matches!(expr("(a = 1)"), Expr::Assign { .. }));
        assert!(matches!(expr("([1, 2])"), Expr::Array(_)));
        assert!(matches!(expr("(a + b) * c"), Expr::Binary { .. }));
    }

    #[test]
    fn parses_object_methods_with_complex_parameters() {
        let Expr::Object(properties) = expr("({ m({a}, b = 1, ...rest) { return a + b; } })")
        else {
            panic!("expected object literal");
        };
        let Expr::Function(function) = &properties[0].value else {
            panic!("expected method function");
        };
        assert_eq!(function.patterns.len(), 3);
        assert!(matches!(function.patterns[0], Pattern::Object { .. }));
        assert!(matches!(function.patterns[1], Pattern::Default { .. }));
        assert!(matches!(function.patterns[2], Pattern::Rest(_)));
    }
}
