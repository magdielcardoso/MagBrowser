//! Completion values and statement helpers shared by the interpreter.

use browser_js_parser::{ForInit, ForTarget, Stmt};

use crate::value::Value;

/// The result of executing a statement.
#[derive(Clone, Debug)]
pub(crate) enum Completion {
    /// Fell through to the next statement.
    Normal,
    /// A `return` statement terminated execution.
    Return(Value),
    /// A `break` statement, optionally labelled.
    Break(Option<String>),
    /// A `continue` statement, optionally labelled.
    Continue(Option<String>),
}

/// What a loop should do with the completion of its body.
pub(crate) enum LoopAction {
    /// Run the next iteration.
    Continue,
    /// Leave the loop.
    Break,
    /// Propagate an unrelated completion.
    Propagate(Completion),
}

/// Reduce a loop body completion using the loop's own label.
pub(crate) fn loop_action(completion: Completion, label: Option<&str>) -> LoopAction {
    match completion {
        Completion::Normal => LoopAction::Continue,
        Completion::Continue(None) => LoopAction::Continue,
        Completion::Break(None) => LoopAction::Break,
        Completion::Continue(Some(name)) if label == Some(name.as_str()) => LoopAction::Continue,
        Completion::Break(Some(name)) if label == Some(name.as_str()) => LoopAction::Break,
        other => LoopAction::Propagate(other),
    }
}

/// Collect `var` declared names, descending into nested statements but not
/// nested functions.
pub(crate) fn collect_var_names(stmts: &[Stmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::VarDecl(declaration) => {
                if declaration.kind == browser_js_parser::DeclKind::Var {
                    for declarator in &declaration.declarations {
                        declarator.pattern.bound_names(out);
                    }
                }
            }
            Stmt::Block(body) => collect_var_names(body, out),
            Stmt::If {
                consequent,
                alternate,
                ..
            } => {
                collect_var_names(std::slice::from_ref(consequent), out);
                if let Some(alternate) = alternate {
                    collect_var_names(std::slice::from_ref(alternate), out);
                }
            }
            Stmt::While { body, .. } | Stmt::DoWhile { body, .. } => {
                collect_var_names(std::slice::from_ref(body), out);
            }
            Stmt::For { init, body, .. } => {
                if let Some(ForInit::Var(declaration)) = init
                    && declaration.kind == browser_js_parser::DeclKind::Var
                {
                    for declarator in &declaration.declarations {
                        declarator.pattern.bound_names(out);
                    }
                }
                collect_var_names(std::slice::from_ref(body), out);
            }
            Stmt::ForIn { left, body, .. } => {
                if let ForTarget::Var {
                    kind: browser_js_parser::DeclKind::Var,
                    name,
                } = left
                {
                    out.push(name.clone());
                }
                collect_var_names(std::slice::from_ref(body), out);
            }
            Stmt::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_var_names(block, out);
                if let Some(handler) = handler {
                    collect_var_names(&handler.body, out);
                }
                if let Some(finalizer) = finalizer {
                    collect_var_names(finalizer, out);
                }
            }
            Stmt::Switch { cases, .. } => {
                for case in cases {
                    collect_var_names(&case.body, out);
                }
            }
            Stmt::Labeled { body, .. } => {
                collect_var_names(std::slice::from_ref(body), out);
            }
            _ => {}
        }
    }
}
