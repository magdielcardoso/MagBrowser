//! Generator and async function execution.
//!
//! Generators and async functions share a small stack machine. A JavaScript
//! body is compiled into a flat instruction list; `yield` and `await` become
//! `Suspend` instructions that store their result in a temporary binding and
//! pause the machine. The whole continuation lives in the instruction stream
//! and in scopes on the environment stack rather than in Rust's call stack,
//! so resuming is just advancing the program counter. Side effects, lexical
//! scopes and loop state therefore survive across suspensions.

use std::cell::RefCell;
use std::rc::Rc;

use browser_js_parser::{DeclKind, Expr, ForInit, ForTarget, Function, Pattern, Stmt};

use crate::errors::JsError;
use crate::execution::collect_var_names;
use crate::object::SYMBOL_ITERATOR_KEY;
use crate::scope::{BindingKind, ScopeId};
use crate::value::Value;

/// Prefix for VM temporary bindings. A NUL byte cannot occur in a JavaScript
/// identifier, so these names cannot collide with user variables.
const TEMP_PREFIX: &str = "\u{0}gen_tmp_";
/// Binding holding the home object used by `super.member`.
pub(crate) const SUPER_HOME: &str = "\u{0}super_home";
/// Binding holding the superclass constructor used by `super()`.
pub(crate) const SUPER_CTOR: &str = "\u{0}super_ctor";

fn temp_name(index: usize) -> String {
    format!("{TEMP_PREFIX}{index}")
}

/// One operation of the generator stack machine.
#[derive(Clone, Debug)]
pub(crate) enum Instr {
    /// Evaluate a yield-free expression and store the result in a temporary.
    Eval { expr: Expr, temp: usize },
    /// Evaluate a yield-free expression for its side effects.
    Effect { expr: Expr },
    /// Pause. The already lowered `argument` is the yielded/awaited value; on
    /// resume the received value is written to `temp`.
    Suspend {
        argument: Option<Expr>,
        delegate: bool,
        temp: usize,
        is_await: bool,
    },
    /// Declare the variables bound by a pattern in the current scope.
    Declare {
        kind: DeclKind,
        pattern: Pattern,
        init: Option<usize>,
    },
    /// Assign a pattern from a temporary.
    Assign { pattern: Pattern, temp: usize },
    /// Unconditional jump. Targets are patched after compilation.
    Jump(usize),
    /// Jump when a temporary is falsy.
    JumpIfFalse { temp: usize, target: usize },
    /// Jump when a temporary is truthy.
    JumpIfTrue { temp: usize, target: usize },
    /// Enter a lexical block scope.
    PushScope,
    /// Leave the innermost lexical block scope.
    PopScope,
    /// Return from the function.
    Return { temp: Option<usize> },
    /// Throw the value held in a temporary.
    Throw { temp: usize },
    /// Enter a `try` block, recording the catch entry point.
    TryEnter {
        catch_target: usize,
        catch_param: Option<String>,
    },
    /// Leave the current `try` block.
    TryExit,
}

/// An active `try` block in the generator machine.
#[derive(Clone, Debug)]
struct Handler {
    catch_target: usize,
    catch_param: Option<String>,
    env_len: usize,
    scope_depth: usize,
}

/// State of an active `yield*` delegation.
#[derive(Clone, Debug)]
struct Delegate {
    iterator: Value,
    next: Value,
    temp: usize,
}

/// Mutable state of a suspended generator or async function.
#[derive(Debug)]
pub(crate) struct GeneratorFrame {
    pub(crate) instructions: Vec<Instr>,
    pub(crate) pc: usize,
    pub(crate) scope: ScopeId,
    pub(crate) var_scope: ScopeId,
    pub(crate) name: String,
    pub(crate) done: bool,
    pub(crate) suspended: bool,
    pub(crate) is_async: bool,
    /// Promise returned to the caller of an async function.
    pub(crate) promise: Option<Value>,
    /// Lexical block scopes currently entered by the instruction stream. They
    /// are restored on every resume because the Rust call stack is gone.
    pub(crate) block_scopes: Vec<ScopeId>,
    handlers: Vec<Handler>,
    delegate: Option<Delegate>,
}

impl GeneratorFrame {
    /// A human readable label for error stacks.
    pub(crate) fn label(&self) -> String {
        if self.is_async {
            format!("async {}", self.name)
        } else {
            self.name.clone()
        }
    }

    fn new(
        instructions: Vec<Instr>,
        scope: ScopeId,
        var_scope: ScopeId,
        name: String,
        is_async: bool,
        promise: Option<Value>,
    ) -> Self {
        Self {
            instructions,
            pc: 0,
            scope,
            var_scope,
            name,
            done: false,
            suspended: false,
            is_async,
            promise,
            block_scopes: Vec::new(),
            handlers: Vec::new(),
            delegate: None,
        }
    }
}

/// Why a machine stopped running.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// A `yield` produced a value.
    Yield(Value),
    /// An `await` produced a value that must settle before resuming.
    Await(Value),
    /// The function completed.
    Return(Value),
}

/// A loop being compiled, with the jump instructions that need patching once
/// the loop body's size is known.
struct LoopCtx {
    label: Option<String>,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

#[derive(Default)]
struct Compiler {
    code: Vec<Instr>,
    temps: usize,
    loops: Vec<LoopCtx>,
    try_stack: Vec<usize>,
}

impl Compiler {
    fn temp(&mut self) -> usize {
        let index = self.temps;
        self.temps += 1;
        index
    }

    fn emit(&mut self, instr: Instr) -> usize {
        let index = self.code.len();
        self.code.push(instr);
        index
    }

    fn jump(&mut self) -> usize {
        self.emit(Instr::Jump(usize::MAX))
    }

    fn patch(&mut self, index: usize, target: usize) {
        match &mut self.code[index] {
            Instr::Jump(destination)
            | Instr::JumpIfFalse {
                target: destination,
                ..
            }
            | Instr::JumpIfTrue {
                target: destination,
                ..
            }
            | Instr::TryEnter {
                catch_target: destination,
                ..
            } => *destination = target,
            _ => {}
        }
    }

    fn patch_all(&mut self, indices: &[usize], target: usize) {
        for &index in indices {
            self.patch(index, target);
        }
    }

    /// Lower and evaluate an expression, storing the result in a temporary.
    fn eval_expr(&mut self, expr: &Expr) -> usize {
        let lowered = lower_expr(expr, self);
        self.eval(lowered)
    }

    fn eval(&mut self, expr: Expr) -> usize {
        let temp = self.temp();
        self.emit(Instr::Eval { expr, temp });
        temp
    }

    fn patch_try_catch(&mut self, index: usize, target: usize) {
        if let Instr::TryEnter { catch_target, .. } = &mut self.code[index] {
            *catch_target = target;
        }
    }

    fn closest_loop(&self, label: Option<&String>) -> Option<usize> {
        self.loops
            .iter()
            .rposition(|ctx| match (label, &ctx.label) {
                (None, _) => true,
                (Some(name), Some(loop_label)) => name == loop_label,
                _ => false,
            })
    }

    fn finish_loop(&mut self, ctx: LoopCtx, break_target: usize, continue_target: usize) {
        self.patch_all(&ctx.breaks, break_target);
        self.patch_all(&ctx.continues, continue_target);
    }
}

/// Compile a function body into generator instructions. Returns the code and
/// the number of temporary bindings the frame must reserve.
pub(crate) fn compile(function: &Function) -> (Vec<Instr>, usize) {
    let mut compiler = Compiler::default();
    compile_stmts(&function.body, &mut compiler);
    compiler.emit(Instr::Return { temp: None });
    (compiler.code, compiler.temps)
}

fn compile_stmts(stmts: &[Stmt], compiler: &mut Compiler) {
    for stmt in stmts {
        compile_stmt(stmt, compiler);
    }
}

fn compile_stmt(stmt: &Stmt, compiler: &mut Compiler) {
    match stmt {
        Stmt::Empty | Stmt::FunctionDecl(_) | Stmt::ClassDecl(_) => {}
        Stmt::Expression(expr) => {
            let expr = lower_expr(expr, compiler);
            compiler.emit(Instr::Effect { expr });
        }
        Stmt::VarDecl(declaration) => compile_var_decl(declaration, compiler),
        Stmt::Return(value) => {
            let temp = value.as_ref().map(|expr| compiler.eval_expr(expr));
            compiler.emit(Instr::Return { temp });
        }
        Stmt::Throw(expr) => {
            let temp = compiler.eval_expr(expr);
            compiler.emit(Instr::Throw { temp });
        }
        Stmt::If {
            test,
            consequent,
            alternate,
        } => {
            let test_temp = compiler.eval_expr(test);
            let jump_else = compiler.emit(Instr::JumpIfFalse {
                temp: test_temp,
                target: usize::MAX,
            });
            compile_stmt(consequent, compiler);
            let jump_end = compiler.jump();
            compiler.patch(jump_else, compiler.code.len());
            if let Some(alternate) = alternate {
                compile_stmt(alternate, compiler);
            }
            compiler.patch(jump_end, compiler.code.len());
        }
        Stmt::Block(stmts) => {
            compiler.emit(Instr::PushScope);
            compile_stmts(stmts, compiler);
            compiler.emit(Instr::PopScope);
        }
        Stmt::While { test, body } => {
            let start = compiler.code.len();
            let ctx = LoopCtx {
                label: None,
                breaks: Vec::new(),
                continues: Vec::new(),
            };
            compiler.loops.push(ctx);
            let test_temp = compiler.eval_expr(test);
            let exit = compiler.emit(Instr::JumpIfFalse {
                temp: test_temp,
                target: usize::MAX,
            });
            compile_stmt(body, compiler);
            compiler.emit(Instr::Jump(start));
            let ctx = compiler.loops.pop().expect("loop context");
            let end = compiler.code.len();
            compiler.finish_loop(ctx, end, start);
            if exit != usize::MAX {
                // The exit jump targets the instruction after the loop, which
                // is exactly where `finish_loop` patched the breaks.
                if let Instr::JumpIfFalse { target, .. } = &mut compiler.code[exit] {
                    *target = end;
                }
            }
        }
        Stmt::DoWhile { body, test } => {
            let start = compiler.code.len();
            compiler.loops.push(LoopCtx {
                label: None,
                breaks: Vec::new(),
                continues: Vec::new(),
            });
            compile_stmt(body, compiler);
            let continue_target = compiler.code.len();
            let test_temp = compiler.eval_expr(test);
            compiler.emit(Instr::JumpIfTrue {
                temp: test_temp,
                target: start,
            });
            let ctx = compiler.loops.pop().expect("loop context");
            let end = compiler.code.len();
            compiler.finish_loop(ctx, end, continue_target);
        }
        Stmt::For {
            init,
            test,
            update,
            body,
        } => compile_for(
            init.as_ref(),
            test.as_ref(),
            update.as_ref(),
            body,
            compiler,
        ),
        Stmt::ForIn {
            left,
            right,
            body,
            is_of,
        } => compile_for_in(left, right, *is_of, body, compiler),
        Stmt::Break(label) => {
            if let Some(index) = compiler.closest_loop(label.as_ref()) {
                let jump = compiler.jump();
                compiler.loops[index].breaks.push(jump);
            }
        }
        Stmt::Continue(label) => {
            if let Some(index) = compiler.closest_loop(label.as_ref()) {
                let jump = compiler.jump();
                compiler.loops[index].continues.push(jump);
            }
        }
        Stmt::Labeled { label, body } => {
            // A label belongs to a loop when it directly wraps one.
            if matches!(
                body.as_ref(),
                Stmt::While { .. } | Stmt::DoWhile { .. } | Stmt::For { .. } | Stmt::ForIn { .. }
            ) {
                let saved = compiler.loops.len();
                compile_stmt(body, compiler);
                if let Some(ctx) = compiler.loops.get_mut(saved) {
                    ctx.label = Some(label.clone());
                }
            } else {
                compile_stmt(body, compiler);
            }
        }
        Stmt::Try {
            block,
            handler,
            finalizer,
        } => {
            let enter = compiler.emit(Instr::TryEnter {
                catch_target: usize::MAX,
                catch_param: handler.as_ref().and_then(|h| h.param.clone()),
            });
            compiler.try_stack.push(enter);
            compile_stmts(block, compiler);
            compiler.emit(Instr::TryExit);
            let skip_catch = compiler.jump();
            if handler.is_some() {
                compiler.patch_try_catch(enter, compiler.code.len());
                compile_stmts(&handler.as_ref().expect("handler").body, compiler);
            } else {
                compiler.patch_try_catch(enter, compiler.code.len());
            }
            let finally_start = compiler.code.len();
            compiler.patch(skip_catch, finally_start);
            if let Some(finalizer) = finalizer {
                compile_stmts(finalizer, compiler);
            }
            compiler.try_stack.pop();
        }
        // `switch` inside generators is outside this small machine's subset;
        // the ordinary interpreter supports it in normal functions.
        Stmt::Switch { .. } => {}
        Stmt::Import(_) | Stmt::Export(_) => {}
    }
}

fn compile_var_decl(declaration: &browser_js_parser::VarDecl, compiler: &mut Compiler) {
    for declarator in &declaration.declarations {
        let init = declarator
            .init
            .as_ref()
            .map(|init| compiler.eval_expr(init));
        let pattern = lower_pattern(&declarator.pattern, compiler);
        compiler.emit(Instr::Declare {
            kind: declaration.kind,
            pattern,
            init,
        });
    }
}

fn compile_for(
    init: Option<&ForInit>,
    test: Option<&Expr>,
    update: Option<&Expr>,
    body: &Stmt,
    compiler: &mut Compiler,
) {
    match init {
        Some(ForInit::Var(declaration)) => compile_var_decl(declaration, compiler),
        Some(ForInit::Expr(expr)) => {
            let lowered = lower_expr(expr, compiler);
            compiler.emit(Instr::Effect { expr: lowered });
        }
        None => {}
    }
    let test_start = compiler.code.len();
    let exit = test.map(|test| {
        let temp = compiler.eval_expr(test);
        compiler.emit(Instr::JumpIfFalse {
            temp,
            target: usize::MAX,
        })
    });
    compiler.loops.push(LoopCtx {
        label: None,
        breaks: Vec::new(),
        continues: Vec::new(),
    });
    compile_stmt(body, compiler);
    let continue_target = compiler.code.len();
    if let Some(update) = update {
        let lowered = lower_expr(update, compiler);
        compiler.emit(Instr::Effect { expr: lowered });
    }
    compiler.emit(Instr::Jump(test_start));
    let ctx = compiler.loops.pop().expect("loop context");
    let end = compiler.code.len();
    compiler.finish_loop(ctx, end, continue_target);
    if let Some(exit) = exit
        && let Instr::JumpIfFalse { target, .. } = &mut compiler.code[exit]
    {
        *target = end;
    }
}

fn compile_for_in(
    left: &ForTarget,
    right: &Expr,
    is_of: bool,
    body: &Stmt,
    compiler: &mut Compiler,
) {
    // Lower the loop into an explicit `Symbol.iterator()` / `next()` loop so
    // generators use the same iteration protocol as ordinary `for...of`.
    let iter_index = compiler.temp();
    let step_index = compiler.temp();
    let item_index = compiler.temp();
    let iter_name = temp_name(iter_index);
    let step_name = temp_name(step_index);
    let symbol_iterator = Expr::Member {
        object: Box::new(Expr::Identifier("Symbol".to_owned())),
        property: Box::new(Expr::Identifier("iterator".to_owned())),
        computed: false,
    };
    let iter_call = Expr::Call {
        callee: Box::new(Expr::Member {
            object: Box::new(right.clone()),
            property: Box::new(symbol_iterator),
            computed: true,
        }),
        arguments: Vec::new(),
    };
    let iter_call_temp = compiler.eval_expr(&iter_call);
    compiler.emit(Instr::Eval {
        expr: Expr::Identifier(temp_name(iter_call_temp)),
        temp: iter_index,
    });

    let loop_start = compiler.code.len();
    compiler.loops.push(LoopCtx {
        label: None,
        breaks: Vec::new(),
        continues: Vec::new(),
    });
    let next_call = Expr::Call {
        callee: Box::new(Expr::Member {
            object: Box::new(Expr::Identifier(iter_name.clone())),
            property: Box::new(Expr::Identifier("next".to_owned())),
            computed: false,
        }),
        arguments: Vec::new(),
    };
    let step_call_temp = compiler.eval(next_call);
    compiler.emit(Instr::Eval {
        expr: Expr::Identifier(temp_name(step_call_temp)),
        temp: step_index,
    });
    let done_temp = compiler.eval(Expr::Member {
        object: Box::new(Expr::Identifier(step_name.clone())),
        property: Box::new(Expr::Identifier("done".to_owned())),
        computed: false,
    });
    let exit = compiler.emit(Instr::JumpIfTrue {
        temp: done_temp,
        target: usize::MAX,
    });
    let value_temp = compiler.eval(Expr::Member {
        object: Box::new(Expr::Identifier(step_name)),
        property: Box::new(Expr::Identifier("value".to_owned())),
        computed: false,
    });
    compiler.emit(Instr::Eval {
        expr: Expr::Identifier(temp_name(value_temp)),
        temp: item_index,
    });
    match left {
        ForTarget::Var { kind, name } => {
            compiler.emit(Instr::Declare {
                kind: *kind,
                pattern: Pattern::Identifier(name.clone()),
                init: Some(item_index),
            });
        }
        ForTarget::Pattern { kind, pattern } => {
            compiler.emit(Instr::Declare {
                kind: *kind,
                pattern: pattern.clone(),
                init: Some(item_index),
            });
        }
        ForTarget::Expr(target) => {
            if let Some(pattern) = crate::interpreter::pattern_from_assignment_target(target) {
                compiler.emit(Instr::Assign {
                    pattern,
                    temp: item_index,
                });
            } else {
                compiler.emit(Instr::Effect {
                    expr: Expr::Assign {
                        op: browser_js_parser::AssignOp::Assign,
                        target: Box::new(target.clone()),
                        value: Box::new(Expr::Identifier(temp_name(item_index))),
                    },
                });
            }
        }
    }
    compile_stmt(body, compiler);
    compiler.emit(Instr::Jump(loop_start));
    let ctx = compiler.loops.pop().expect("loop context");
    let end = compiler.code.len();
    compiler.finish_loop(ctx, end, loop_start);
    if let Instr::JumpIfTrue { target, .. } = &mut compiler.code[exit] {
        *target = end;
    }
    let _ = is_of;
}

/// Replace every `yield`/`await` with a temporary read plus a suspension
/// instruction. The returned expression is free of suspension points and can
/// be evaluated by the ordinary interpreter.
fn lower_expr(expr: &Expr, compiler: &mut Compiler) -> Expr {
    match expr {
        Expr::Yield { argument, delegate } => {
            let lowered = argument
                .as_ref()
                .map(|argument| lower_expr(argument, compiler));
            let temp = compiler.temp();
            compiler.emit(Instr::Suspend {
                argument: lowered,
                delegate: *delegate,
                temp,
                is_await: false,
            });
            Expr::Identifier(temp_name(temp))
        }
        Expr::Await(argument) => {
            let lowered = lower_expr(argument, compiler);
            let temp = compiler.temp();
            compiler.emit(Instr::Suspend {
                argument: Some(lowered),
                delegate: false,
                temp,
                is_await: true,
            });
            Expr::Identifier(temp_name(temp))
        }
        Expr::Number(_)
        | Expr::String(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::This
        | Expr::Super
        | Expr::Regex { .. }
        | Expr::Function(_)
        | Expr::Arrow(_)
        | Expr::Class(_) => expr.clone(),
        Expr::Identifier(name) => Expr::Identifier(name.clone()),
        Expr::Array(items) => Expr::Array(
            items
                .iter()
                .map(|item| item.as_ref().map(|item| lower_expr(item, compiler)))
                .collect(),
        ),
        Expr::Object(properties) => Expr::Object(
            properties
                .iter()
                .map(|property| browser_js_parser::Property {
                    key: property.key.clone(),
                    value: lower_expr(&property.value, compiler),
                    shorthand: property.shorthand,
                    pos: property.pos,
                })
                .collect(),
        ),
        Expr::Spread(inner) => Expr::Spread(Box::new(lower_expr(inner, compiler))),
        Expr::Unary { op, argument } => Expr::Unary {
            op: *op,
            argument: Box::new(lower_expr(argument, compiler)),
        },
        Expr::Update {
            op,
            prefix,
            argument,
        } => Expr::Update {
            op: *op,
            prefix: *prefix,
            argument: Box::new(lower_expr(argument, compiler)),
        },
        Expr::Binary { op, left, right } => Expr::Binary {
            op: *op,
            left: Box::new(lower_expr(left, compiler)),
            right: Box::new(lower_expr(right, compiler)),
        },
        Expr::Logical { op, left, right } => Expr::Logical {
            op: *op,
            left: Box::new(lower_expr(left, compiler)),
            right: Box::new(lower_expr(right, compiler)),
        },
        Expr::Assign { op, target, value } => Expr::Assign {
            op: *op,
            target: Box::new(lower_expr(target, compiler)),
            value: Box::new(lower_expr(value, compiler)),
        },
        Expr::AssignPattern { pattern, value } => Expr::AssignPattern {
            pattern: lower_pattern(pattern, compiler),
            value: Box::new(lower_expr(value, compiler)),
        },
        Expr::Conditional {
            test,
            consequent,
            alternate,
        } => Expr::Conditional {
            test: Box::new(lower_expr(test, compiler)),
            consequent: Box::new(lower_expr(consequent, compiler)),
            alternate: Box::new(lower_expr(alternate, compiler)),
        },
        Expr::Call { callee, arguments } => Expr::Call {
            callee: Box::new(lower_expr(callee, compiler)),
            arguments: arguments
                .iter()
                .map(|argument| lower_expr(argument, compiler))
                .collect(),
        },
        Expr::New { callee, arguments } => Expr::New {
            callee: Box::new(lower_expr(callee, compiler)),
            arguments: arguments
                .iter()
                .map(|argument| lower_expr(argument, compiler))
                .collect(),
        },
        Expr::Member {
            object,
            property,
            computed,
        } => Expr::Member {
            object: Box::new(lower_expr(object, compiler)),
            property: Box::new(lower_expr(property, compiler)),
            computed: *computed,
        },
        Expr::Sequence(expressions) => Expr::Sequence(
            expressions
                .iter()
                .map(|expression| lower_expr(expression, compiler))
                .collect(),
        ),
        Expr::Template { parts, exprs } => Expr::Template {
            parts: parts.clone(),
            exprs: exprs
                .iter()
                .map(|expression| lower_expr(expression, compiler))
                .collect(),
        },
        Expr::ImportCall(argument) => Expr::ImportCall(Box::new(lower_expr(argument, compiler))),
        Expr::MetaProperty { meta, property } => Expr::MetaProperty {
            meta: meta.clone(),
            property: property.clone(),
        },
        Expr::OptionalChain(chain) => Expr::OptionalChain(browser_js_parser::OptionalChain {
            base: Box::new(lower_expr(&chain.base, compiler)),
            ops: chain
                .ops
                .iter()
                .map(|op| match op {
                    browser_js_parser::OptionalChainOp::Member {
                        property,
                        computed,
                        optional,
                    } => browser_js_parser::OptionalChainOp::Member {
                        property: Box::new(lower_expr(property, compiler)),
                        computed: *computed,
                        optional: *optional,
                    },
                    browser_js_parser::OptionalChainOp::Call {
                        arguments,
                        optional,
                    } => browser_js_parser::OptionalChainOp::Call {
                        arguments: arguments
                            .iter()
                            .map(|argument| lower_expr(argument, compiler))
                            .collect(),
                        optional: *optional,
                    },
                })
                .collect(),
        }),
    }
}

fn lower_pattern(pattern: &Pattern, compiler: &mut Compiler) -> Pattern {
    match pattern {
        Pattern::Default { target, default } => Pattern::Default {
            target: Box::new(lower_pattern(target, compiler)),
            default: Box::new(lower_expr(default, compiler)),
        },
        Pattern::Array(elements) => Pattern::Array(
            elements
                .iter()
                .map(|element| {
                    element
                        .as_ref()
                        .map(|element| lower_pattern(element, compiler))
                })
                .collect(),
        ),
        Pattern::Object { properties, rest } => Pattern::Object {
            properties: properties
                .iter()
                .map(|property| browser_js_parser::ObjectPatternProperty {
                    key: property.key.clone(),
                    value: lower_pattern(&property.value, compiler),
                })
                .collect(),
            rest: rest
                .as_ref()
                .map(|rest| Box::new(lower_pattern(rest, compiler))),
        },
        Pattern::Rest(target) => Pattern::Rest(Box::new(lower_pattern(target, compiler))),
        other => other.clone(),
    }
}

impl crate::Runtime {
    /// Create a generator/async frame for `function` with the supplied `this`,
    /// arguments and closure environment.
    pub(crate) fn create_generator_frame(
        &mut self,
        function: &Function,
        this: Value,
        args: Vec<Value>,
        env: ScopeId,
        promise: Option<Value>,
    ) -> Result<Rc<RefCell<GeneratorFrame>>, JsError> {
        let (instructions, temps) = compile(function);
        let name = function.name.clone().unwrap_or_default();
        let scope = self.create_scope(Some(env), true);
        let root_mark = self.temp_roots.len();
        self.temp_roots.push(this.clone());
        for arg in &args {
            self.temp_roots.push(arg.clone());
        }
        for index in 0..temps {
            self.declare_binding(
                scope,
                &temp_name(index),
                BindingKind::Var,
                Value::Undefined,
                true,
            );
        }
        let mut names = Vec::new();
        collect_var_names(&function.body, &mut names);
        for name in names {
            if self
                .scope(scope)
                .is_some_and(|scope| scope.bindings.iter().any(|(bound, _)| bound == &name))
            {
                continue;
            }
            self.declare_binding(scope, &name, BindingKind::Var, Value::Undefined, true);
        }
        self.declare_binding(scope, "this", BindingKind::This, this.clone(), true);
        let arguments = self.new_array(args.clone());
        self.declare_binding(scope, "arguments", BindingKind::Arguments, arguments, true);
        self.bind_parameters_for_generator(scope, &function.patterns, &args)?;
        self.temp_roots.truncate(root_mark);
        Ok(Rc::new(RefCell::new(GeneratorFrame::new(
            instructions,
            scope,
            scope,
            name,
            function.is_async,
            promise,
        ))))
    }

    fn bind_parameters_for_generator(
        &mut self,
        scope: ScopeId,
        patterns: &[Pattern],
        args: &[Value],
    ) -> Result<(), JsError> {
        let base = self.env_stack.len();
        self.env_stack.push(scope);
        let result = (|| {
            for (index, pattern) in patterns.iter().enumerate() {
                if let Pattern::Rest(target) = pattern {
                    let rest = args.get(index..).map(<[Value]>::to_vec).unwrap_or_default();
                    let array = self.new_array(rest);
                    self.temp_roots.push(array.clone());
                    let result = self.bind_param_pattern(target, array);
                    self.temp_roots.pop();
                    result?;
                    break;
                }
                let value = args.get(index).cloned().unwrap_or(Value::Undefined);
                self.bind_param_pattern(pattern, value)?;
            }
            Ok(())
        })();
        self.env_stack.truncate(base);
        result
    }

    /// Run a frame until it yields, awaits or completes. `resume` supplies the
    /// value sent by `next()`, `return()` or a settled promise.
    pub(crate) fn step_generator(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        resume: Option<Value>,
    ) -> Result<Step, JsError> {
        let base_env = self.env_stack.len();
        let base_var = self.var_scope_stack.len();
        {
            let borrowed = frame.borrow();
            self.env_stack.push(borrowed.scope);
            for scope in &borrowed.block_scopes {
                self.env_stack.push(*scope);
            }
            self.var_scope_stack.push(borrowed.var_scope);
        }
        let outcome = match resume {
            Some(value) => self.resume_frame(frame, value),
            None => self.run_generator(frame),
        };
        self.env_stack.truncate(base_env);
        self.var_scope_stack.truncate(base_var);
        match outcome {
            Ok(Step::Return(value)) => {
                frame.borrow_mut().done = true;
                Ok(Step::Return(value))
            }
            Err(mut error) => {
                frame.borrow_mut().done = true;
                let label = frame.borrow().label();
                error.ensure_stack(&[label]);
                Err(error)
            }
            other => other,
        }
    }

    /// Raise an error at the current suspension point, running a catch handler
    /// when one covers it. Used when an awaited promise rejects.
    pub(crate) fn throw_generator(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        error: JsError,
    ) -> Result<Step, JsError> {
        let base_env = self.env_stack.len();
        let base_var = self.var_scope_stack.len();
        {
            let borrowed = frame.borrow();
            self.env_stack.push(borrowed.scope);
            for scope in &borrowed.block_scopes {
                self.env_stack.push(*scope);
            }
            self.var_scope_stack.push(borrowed.var_scope);
        }
        let outcome = if self.enter_catch(frame, error.clone())? {
            self.run_generator(frame)
        } else {
            Err(error)
        };
        self.env_stack.truncate(base_env);
        self.var_scope_stack.truncate(base_var);
        match outcome {
            Ok(Step::Return(value)) => {
                frame.borrow_mut().done = true;
                Ok(Step::Return(value))
            }
            Err(mut error) => {
                frame.borrow_mut().done = true;
                let label = frame.borrow().label();
                error.ensure_stack(&[label]);
                Err(error)
            }
            other => other,
        }
    }

    fn resume_frame(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        value: Value,
    ) -> Result<Step, JsError> {
        if frame.borrow().delegate.is_some() {
            if let Some(yielded) = self.advance_delegate(frame, value)? {
                return Ok(Step::Yield(yielded));
            }
        } else {
            let temp = frame
                .borrow()
                .instructions
                .get(frame.borrow().pc)
                .and_then(|instr| match instr {
                    Instr::Suspend { temp, .. } => Some(*temp),
                    _ => None,
                });
            if let Some(temp) = temp {
                let _ = self.assign_binding(&temp_name(temp), value);
                frame.borrow_mut().pc += 1;
                frame.borrow_mut().suspended = false;
            }
        }
        self.run_generator(frame)
    }

    fn run_generator(&mut self, frame: &Rc<RefCell<GeneratorFrame>>) -> Result<Step, JsError> {
        loop {
            self.tick()?;
            if frame.borrow().delegate.is_some() {
                if let Some(value) = self.advance_delegate(frame, Value::Undefined)? {
                    return Ok(Step::Yield(value));
                }
                continue;
            }
            let (pc, instr) = {
                let borrowed = frame.borrow();
                if borrowed.done || borrowed.pc >= borrowed.instructions.len() {
                    (borrowed.pc, None)
                } else {
                    (
                        borrowed.pc,
                        Some(borrowed.instructions[borrowed.pc].clone()),
                    )
                }
            };
            let Some(instr) = instr else {
                return Ok(Step::Return(Value::Undefined));
            };
            match self.execute_instruction(frame, pc, instr) {
                Ok(None) => {}
                Ok(Some(step)) => return Ok(step),
                Err(error) => {
                    if !self.enter_catch(frame, error.clone())? {
                        return Err(error);
                    }
                }
            }
        }
    }

    /// Execute one instruction. `Ok(None)` asks the caller to advance to the
    /// next instruction; `Ok(Some(step))` stops the machine.
    fn execute_instruction(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        pc: usize,
        instr: Instr,
    ) -> Result<Option<Step>, JsError> {
        match instr {
            Instr::Eval { expr, temp } => {
                let value = self.eval_expr(&expr)?;
                self.assign_binding(&temp_name(temp), value)?;
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::Effect { expr } => {
                self.eval_expr(&expr)?;
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::Declare {
                kind,
                pattern,
                init,
            } => {
                let value = match init {
                    Some(temp) => self.read_binding(&temp_name(temp))?,
                    None => Value::Undefined,
                };
                let scope = self.current_scope();
                self.bind_decl_pattern_in_scope(&pattern, value, kind, scope)?;
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::Assign { pattern, temp } => {
                let value = self.read_binding(&temp_name(temp))?;
                self.assign_pattern(&pattern, value)?;
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::Jump(target) => frame.borrow_mut().pc = target,
            Instr::JumpIfFalse { temp, target } => {
                if self.read_binding(&temp_name(temp))?.is_truthy() {
                    frame.borrow_mut().pc = pc + 1;
                } else {
                    frame.borrow_mut().pc = target;
                }
            }
            Instr::JumpIfTrue { temp, target } => {
                if self.read_binding(&temp_name(temp))?.is_truthy() {
                    frame.borrow_mut().pc = target;
                } else {
                    frame.borrow_mut().pc = pc + 1;
                }
            }
            Instr::PushScope => {
                let scope = self.create_scope(Some(self.current_scope()), false);
                self.env_stack.push(scope);
                frame.borrow_mut().block_scopes.push(scope);
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::PopScope => {
                self.env_stack.pop();
                frame.borrow_mut().block_scopes.pop();
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::TryEnter {
                catch_target,
                catch_param,
            } => {
                let env_len = self.env_stack.len();
                let scope_depth = frame.borrow().block_scopes.len();
                let handler = Handler {
                    catch_target,
                    catch_param,
                    env_len,
                    scope_depth,
                };
                frame.borrow_mut().handlers.push(handler);
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::TryExit => {
                frame.borrow_mut().handlers.pop();
                frame.borrow_mut().pc = pc + 1;
            }
            Instr::Return { temp } => {
                let value = match temp {
                    Some(temp) => self.read_binding(&temp_name(temp))?,
                    None => Value::Undefined,
                };
                return Ok(Some(Step::Return(value)));
            }
            Instr::Throw { temp } => {
                let value = self.read_binding(&temp_name(temp))?;
                return Err(self.error_from_throw(value));
            }
            Instr::Suspend {
                argument,
                delegate,
                temp,
                is_await,
            } => {
                if delegate {
                    let iterable = match argument {
                        Some(expr) => self.eval_expr(&expr)?,
                        None => Value::Undefined,
                    };
                    self.begin_delegate(frame, iterable, temp)?;
                    return Ok(None);
                }
                let value = match argument {
                    Some(expr) => self.eval_expr(&expr)?,
                    None => Value::Undefined,
                };
                frame.borrow_mut().suspended = true;
                return Ok(Some(if is_await {
                    Step::Await(value)
                } else {
                    Step::Yield(value)
                }));
            }
        }
        Ok(None)
    }

    /// If an error occurred inside a `try` block, jump to its catch handler.
    /// Returns whether the error was handled.
    fn enter_catch(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        error: JsError,
    ) -> Result<bool, JsError> {
        let Some(handler) = frame.borrow().handlers.last().cloned() else {
            return Ok(false);
        };
        if handler.catch_target == usize::MAX {
            return Ok(false);
        }
        {
            let mut borrowed = frame.borrow_mut();
            borrowed.block_scopes.truncate(handler.scope_depth);
            borrowed.handlers.pop();
            borrowed.pc = handler.catch_target;
            borrowed.suspended = false;
        }
        self.env_stack.truncate(handler.env_len);
        let scope = self.create_scope(Some(self.current_scope()), false);
        if let Some(param) = handler.catch_param {
            self.declare_binding(scope, &param, BindingKind::Let, error.thrown_value(), true);
        }
        self.env_stack.push(scope);
        frame.borrow_mut().block_scopes.push(scope);
        Ok(true)
    }

    fn begin_delegate(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        iterable: Value,
        temp: usize,
    ) -> Result<(), JsError> {
        let method = self.get_property_checked(&iterable, SYMBOL_ITERATOR_KEY)?;
        if !self.is_callable(&method) {
            return Err(JsError::type_error("value is not iterable"));
        }
        let iterator = self.call_function(&method, iterable, Vec::new())?;
        let next = self.get_property_checked(&iterator, "next")?;
        if !self.is_callable(&next) {
            return Err(JsError::type_error(
                "iterator does not have a callable next",
            ));
        }
        frame.borrow_mut().delegate = Some(Delegate {
            iterator,
            next,
            temp,
        });
        frame.borrow_mut().suspended = true;
        Ok(())
    }

    /// Advance an inner `yield*` iterator. `Some(value)` means the generator
    /// suspends with that value; `None` means the inner iterator finished and
    /// the main instruction stream has continued.
    fn advance_delegate(
        &mut self,
        frame: &Rc<RefCell<GeneratorFrame>>,
        argument: Value,
    ) -> Result<Option<Value>, JsError> {
        let (iterator, next, temp) = {
            let borrowed = frame.borrow();
            let delegate = borrowed.delegate.as_ref().expect("delegate is active");
            (
                delegate.iterator.clone(),
                delegate.next.clone(),
                delegate.temp,
            )
        };
        let result = self.call_function(&next, iterator, vec![argument])?;
        let done = self.get_property_checked(&result, "done")?.is_truthy();
        let value = self.get_property_checked(&result, "value")?;
        if done {
            frame.borrow_mut().delegate = None;
            frame.borrow_mut().suspended = false;
            let _ = self.assign_binding(&temp_name(temp), value);
            frame.borrow_mut().pc += 1;
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    fn bind_decl_pattern_in_scope(
        &mut self,
        pattern: &Pattern,
        value: Value,
        kind: DeclKind,
        scope: ScopeId,
    ) -> Result<(), JsError> {
        let base = self.env_stack.len();
        self.env_stack.push(scope);
        let outcome = self.bind_decl_pattern(pattern, value, kind);
        self.env_stack.truncate(base);
        outcome
    }

    /// Start an async function's frame and wire its result promise.
    pub(crate) fn start_async(
        &mut self,
        frame: Rc<RefCell<GeneratorFrame>>,
        promise: Value,
    ) -> Result<(), JsError> {
        self.continue_async(frame, promise, None)
    }

    /// Continue an async function with a resumed value (or from the start
    /// when `resume` is `None`).
    pub(crate) fn continue_async(
        &mut self,
        frame: Rc<RefCell<GeneratorFrame>>,
        promise: Value,
        resume: Option<Value>,
    ) -> Result<(), JsError> {
        match self.step_generator(&frame, resume) {
            Ok(Step::Await(value)) => {
                self.schedule_async_resume(frame, promise, value);
                Ok(())
            }
            Ok(Step::Yield(value)) => {
                // Async generators are not part of the supported subset.
                self.schedule_async_resume(frame, promise, value);
                Ok(())
            }
            Ok(Step::Return(value)) => {
                self.resolve_promise_value(&promise, value);
                Ok(())
            }
            Err(error) => {
                self.reject_promise_value(&promise, error.thrown_value());
                Ok(())
            }
        }
    }

    fn schedule_async_resume(
        &mut self,
        frame: Rc<RefCell<GeneratorFrame>>,
        promise: Value,
        awaited: Value,
    ) {
        let resolve_frame = Rc::clone(&frame);
        let resolve_promise = promise.clone();
        let reject_frame = Rc::clone(&frame);
        let reject_promise = promise.clone();
        let resolve = self.new_native_value(
            "asyncResolve",
            1,
            Box::new(move |runtime, _this, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                runtime
                    .continue_async(
                        Rc::clone(&resolve_frame),
                        resolve_promise.clone(),
                        Some(value),
                    )
                    .map(|()| Value::Undefined)
            }),
        );
        let reject = self.new_native_value(
            "asyncReject",
            1,
            Box::new(move |runtime, _this, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                runtime.reject_async(Rc::clone(&reject_frame), reject_promise.clone(), value)
            }),
        );
        let then = self.get_property(&awaited, "then");
        if self.is_callable(&then) {
            let _ = self.call_function(&then, awaited.clone(), vec![resolve, reject]);
        } else {
            self.enqueue_microtask(Box::new(move |runtime, _this, _args| {
                runtime
                    .continue_async(Rc::clone(&frame), promise.clone(), Some(awaited.clone()))
                    .map(|()| Value::Undefined)
            }));
        }
    }

    fn reject_async(
        &mut self,
        frame: Rc<RefCell<GeneratorFrame>>,
        promise: Value,
        value: Value,
    ) -> Result<Value, JsError> {
        let error = self.error_from_throw(value);
        match self.throw_generator(&frame, error) {
            Ok(Step::Await(awaited)) => {
                self.schedule_async_resume(frame, promise, awaited);
            }
            Ok(Step::Yield(value)) => {
                self.schedule_async_resume(frame, promise, value);
            }
            Ok(Step::Return(value)) => self.resolve_promise_value(&promise, value),
            Err(error) => self.reject_promise_value(&promise, error.thrown_value()),
        }
        Ok(Value::Undefined)
    }

    fn resolve_promise_value(&mut self, promise: &Value, value: Value) {
        if let Value::Object(id) = promise {
            self.resolve_promise(*id, value);
        }
    }

    fn reject_promise_value(&mut self, promise: &Value, value: Value) {
        if let Value::Object(id) = promise {
            self.reject_promise(*id, value);
        }
    }
}
