//! The tree walking interpreter.

use std::rc::Rc;

use browser_js_parser::{
    AssignOp, BinaryOp, CatchClause, Class, ClassMember, DeclKind, ExportDecl, ExportKind, Expr,
    ForInit, ForTarget, Function, LogicalOp, MethodKind, ObjectPatternProperty, OptionalChain,
    OptionalChainOp, Pattern, Program, PropertyKey, Stmt, SwitchCase, UnaryOp, UpdateOp, VarDecl,
};

use crate::errors::JsError;
use crate::execution::{Completion, LoopAction, collect_var_names, loop_action};
use crate::function::UserFunction;
use crate::generator::{SUPER_CTOR, SUPER_HOME};
use crate::object::{ObjectKind, SYMBOL_ITERATOR_KEY};
use crate::scope::{BindingKind, ScopeId};
use crate::value::Value;

/// Convert a number to a signed 32 bit integer the way JavaScript does.
pub(crate) fn to_int32(value: f64) -> i32 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    let truncated = value.trunc();
    let modulo = truncated.rem_euclid(4_294_967_296.0);
    if modulo >= 2_147_483_648.0 {
        (modulo - 4_294_967_296.0) as i32
    } else {
        modulo as i32
    }
}

/// Convert an assignment target expression into a destructuring pattern.
pub(crate) fn pattern_from_assignment_target(expr: &Expr) -> Option<Pattern> {
    match expr {
        Expr::Identifier(name) => Some(Pattern::Identifier(name.clone())),
        Expr::Member { .. } => Some(Pattern::Member(Box::new(expr.clone()))),
        Expr::Array(items) => Some(Pattern::Array(
            items
                .iter()
                .map(|item| item.as_ref().and_then(pattern_from_assignment_target))
                .collect(),
        )),
        Expr::Object(properties) => Some(Pattern::Object {
            properties: properties
                .iter()
                .map(|property| ObjectPatternProperty {
                    key: property.key.clone(),
                    value: pattern_from_assignment_target(&property.value)
                        .unwrap_or(Pattern::Identifier("_".to_owned())),
                })
                .collect(),
            rest: None,
        }),
        _ => None,
    }
}

/// The declaration an `export`/`export default` wraps, when there is one.
fn exported_declaration(stmt: &Stmt) -> Option<&Stmt> {
    match stmt {
        Stmt::Export(export) => match &export.kind {
            ExportKind::Decl(inner) | ExportKind::DefaultDecl(inner) => Some(inner.as_ref()),
            _ => None,
        },
        other => Some(other),
    }
}

/// The `.length` of a function: the number of parameters before the first
/// default value or rest parameter.
fn function_length(patterns: &[Pattern]) -> usize {
    let mut length = 0;
    for pattern in patterns {
        if matches!(pattern, Pattern::Default { .. } | Pattern::Rest(_)) {
            break;
        }
        length += 1;
    }
    length
}

/// Convert a number to an unsigned 32 bit integer the way JavaScript does.
pub(crate) fn to_uint32(value: f64) -> u32 {
    to_int32(value) as u32
}

impl crate::Runtime {
    pub(crate) fn exec_program(&mut self, program: &Program) -> Result<Value, JsError> {
        let global_scope = self.global_scope;
        self.prepare_lexical(&program.body, global_scope);
        self.hoist_vars(&program.body);
        self.hoist_functions(&program.body, global_scope);
        self.exec_stmts(&program.body)?;
        Ok(std::mem::replace(
            &mut self.completion_value,
            Value::Undefined,
        ))
    }

    /// Execute an already-instantiated module's body in its own scope.
    pub(crate) fn exec_module_body(&mut self, id: usize) -> Result<Value, JsError> {
        let (scope, url, program) = {
            let module = self.module(id);
            (module.scope, module.url.clone(), module.program.clone())
        };
        self.env_stack.push(scope);
        self.var_scope_stack.push(scope);
        let previous_module = self.current_module.replace(id);
        self.call_stack.push(url);
        let result = self.exec_stmts(&program.body);
        self.call_stack.pop();
        self.current_module = previous_module;
        self.env_stack.pop();
        self.var_scope_stack.pop();
        result?;
        Ok(self.module_namespace(id))
    }

    pub(crate) fn exec_stmts(&mut self, stmts: &[Stmt]) -> Result<Completion, JsError> {
        for stmt in stmts {
            match self.exec_stmt(stmt, None)? {
                Completion::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Completion::Normal)
    }

    fn exec_stmt(&mut self, stmt: &Stmt, label: Option<&str>) -> Result<Completion, JsError> {
        self.tick()?;
        self.maybe_gc();
        match stmt {
            Stmt::Empty | Stmt::FunctionDecl(_) => Ok(Completion::Normal),
            // Static imports are linked before execution, so nothing to do.
            Stmt::Import(_) => Ok(Completion::Normal),
            Stmt::Export(export) => self.exec_export(export),
            Stmt::ClassDecl(class) => {
                let value = self.create_class(class)?;
                let name = class.name.clone().unwrap_or_default();
                let scope = self.current_scope();
                self.initialize_binding(scope, &name, value)?;
                Ok(Completion::Normal)
            }
            Stmt::Expression(expr) => {
                let value = self.eval_expr(expr)?;
                self.completion_value = value;
                Ok(Completion::Normal)
            }
            Stmt::VarDecl(declaration) => {
                self.exec_var_decl(declaration)?;
                Ok(Completion::Normal)
            }
            Stmt::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval_expr(expr)?,
                    None => Value::Undefined,
                };
                Ok(Completion::Return(value))
            }
            Stmt::If {
                test,
                consequent,
                alternate,
            } => {
                if self.eval_expr(test)?.is_truthy() {
                    self.exec_stmt(consequent, None)
                } else if let Some(alternate) = alternate {
                    self.exec_stmt(alternate, None)
                } else {
                    Ok(Completion::Normal)
                }
            }
            Stmt::Block(stmts) => {
                let completion = self.exec_block(stmts)?;
                if let Completion::Break(Some(name)) = &completion
                    && label == Some(name.as_str())
                {
                    return Ok(Completion::Normal);
                }
                Ok(completion)
            }
            Stmt::While { test, body } => self.exec_while(test, body, label),
            Stmt::DoWhile { body, test } => self.exec_do_while(body, test, label),
            Stmt::For {
                init,
                test,
                update,
                body,
            } => self.exec_for(init.as_ref(), test.as_ref(), update.as_ref(), body, label),
            Stmt::ForIn {
                left,
                right,
                body,
                is_of,
                ..
            } => self.exec_for_in(left, right, *is_of, body, label),
            Stmt::Break(name) => Ok(Completion::Break(name.clone())),
            Stmt::Continue(name) => Ok(Completion::Continue(name.clone())),
            Stmt::Throw(expr) => {
                let value = self.eval_expr(expr)?;
                Err(self.error_from_throw(value))
            }
            Stmt::Try {
                block,
                handler,
                finalizer,
            } => self.exec_try(block, handler.as_ref(), finalizer.as_deref()),
            Stmt::Switch {
                discriminant,
                cases,
            } => self.exec_switch(discriminant, cases, label),
            Stmt::Labeled { label: name, body } => match self.exec_stmt(body, Some(name))? {
                Completion::Break(Some(found)) if found == *name => Ok(Completion::Normal),
                other => Ok(other),
            },
        }
    }

    fn exec_var_decl(&mut self, declaration: &VarDecl) -> Result<(), JsError> {
        for declarator in &declaration.declarations {
            match (&declarator.pattern, &declarator.init) {
                (Pattern::Identifier(name), None) => {
                    if declaration.kind == DeclKind::Var {
                        if self.resolve_scope(name).is_none() {
                            self.declare_var(name, Value::Undefined);
                        }
                    } else {
                        let scope = self.current_scope();
                        self.initialize_binding(scope, name, Value::Undefined)?;
                    }
                }
                (pattern, Some(init)) => {
                    let value = self.eval_expr(init)?;
                    match declaration.kind {
                        DeclKind::Var => self.bind_decl_pattern(pattern, value, DeclKind::Var)?,
                        DeclKind::Let => self.bind_decl_pattern(pattern, value, DeclKind::Let)?,
                        DeclKind::Const => {
                            self.bind_decl_pattern(pattern, value, DeclKind::Const)?
                        }
                    }
                }
                (_, None) => {
                    return Err(JsError::syntax_error(
                        "destructuring declaration requires an initializer",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Bind a declaration pattern to `value`.
    pub(crate) fn bind_decl_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
        kind: DeclKind,
    ) -> Result<(), JsError> {
        match pattern {
            Pattern::Identifier(name) => {
                if kind == DeclKind::Var {
                    if !self.assign_binding(name, value.clone())? {
                        self.declare_var(name, value);
                    }
                    Ok(())
                } else {
                    let scope = self.current_scope();
                    self.initialize_binding(scope, name, value)
                }
            }
            Pattern::Default { target, default } => {
                let value = if matches!(value, Value::Undefined) {
                    self.eval_expr(default)?
                } else {
                    value
                };
                self.bind_decl_pattern(target, value, kind)
            }
            Pattern::Array(elements) => {
                let items = self.destructure_iterable(&value)?;
                self.bind_array_pattern(elements, &items, |runtime, pattern, item| {
                    runtime.bind_decl_pattern(pattern, item, kind)
                })
            }
            Pattern::Object { properties, rest } => self.bind_object_pattern(
                properties,
                rest.as_deref(),
                &value,
                |runtime, pattern, item| runtime.bind_decl_pattern(pattern, item, kind),
            ),
            Pattern::Rest(_) => Err(JsError::syntax_error("unexpected rest element")),
            Pattern::Member(_) => Err(JsError::syntax_error(
                "member expressions cannot be declaration targets",
            )),
        }
    }

    /// Bind a parameter pattern in its call scope.
    pub(crate) fn bind_param_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
    ) -> Result<(), JsError> {
        match pattern {
            Pattern::Identifier(name) => {
                let scope = self.current_scope();
                self.declare_binding(scope, name, BindingKind::Param, value, true);
                Ok(())
            }
            Pattern::Default { target, default } => {
                let value = if matches!(value, Value::Undefined) {
                    self.eval_expr(default)?
                } else {
                    value
                };
                self.bind_param_pattern(target, value)
            }
            Pattern::Array(elements) => {
                let items = self.destructure_iterable(&value)?;
                self.bind_array_pattern(elements, &items, |runtime, pattern, item| {
                    runtime.bind_param_pattern(pattern, item)
                })
            }
            Pattern::Object { properties, rest } => self.bind_object_pattern(
                properties,
                rest.as_deref(),
                &value,
                |runtime, pattern, item| runtime.bind_param_pattern(pattern, item),
            ),
            Pattern::Rest(_) => Err(JsError::syntax_error("unexpected rest element")),
            Pattern::Member(_) => Err(JsError::syntax_error(
                "member expressions cannot be parameter targets",
            )),
        }
    }

    /// Assign a destructuring pattern in an expression context.
    pub(crate) fn assign_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
    ) -> Result<(), JsError> {
        match pattern {
            Pattern::Identifier(name) => {
                if !self.assign_binding(name, value.clone())? {
                    let global_scope = self.global_scope;
                    self.declare_binding(global_scope, name, BindingKind::Var, value, true);
                }
                Ok(())
            }
            Pattern::Member(expr) => self.assign_target(expr, value),
            Pattern::Default { target, default } => {
                let value = if matches!(value, Value::Undefined) {
                    self.eval_expr(default)?
                } else {
                    value
                };
                self.assign_pattern(target, value)
            }
            Pattern::Array(elements) => {
                let items = self.destructure_iterable(&value)?;
                self.bind_array_pattern(elements, &items, |runtime, pattern, item| {
                    runtime.assign_pattern(pattern, item)
                })
            }
            Pattern::Object { properties, rest } => self.bind_object_pattern(
                properties,
                rest.as_deref(),
                &value,
                |runtime, pattern, item| runtime.assign_pattern(pattern, item),
            ),
            Pattern::Rest(_) => Err(JsError::syntax_error("unexpected rest element")),
        }
    }

    /// Convert a value to the element sequence an array pattern consumes.
    fn destructure_iterable(&mut self, value: &Value) -> Result<Vec<Value>, JsError> {
        match value {
            Value::Undefined | Value::Null => {
                Err(JsError::type_error("cannot destructure undefined or null"))
            }
            _ => self.iterate_values(value),
        }
    }

    fn bind_array_pattern(
        &mut self,
        elements: &[Option<Pattern>],
        items: &[Value],
        bind: impl Fn(&mut Self, &Pattern, Value) -> Result<(), JsError>,
    ) -> Result<(), JsError> {
        for (index, element) in elements.iter().enumerate() {
            let Some(pattern) = element else {
                continue;
            };
            if let Pattern::Rest(target) = pattern {
                let rest = items
                    .get(index..)
                    .map(<[Value]>::to_vec)
                    .unwrap_or_default();
                let array = self.new_array(rest);
                bind(self, target, array)?;
                break;
            }
            let value = items.get(index).cloned().unwrap_or(Value::Undefined);
            bind(self, pattern, value)?;
        }
        Ok(())
    }

    fn bind_object_pattern(
        &mut self,
        properties: &[browser_js_parser::ObjectPatternProperty],
        rest: Option<&Pattern>,
        value: &Value,
        bind: impl Fn(&mut Self, &Pattern, Value) -> Result<(), JsError>,
    ) -> Result<(), JsError> {
        match value {
            Value::Undefined | Value::Null => {
                return Err(JsError::type_error("cannot destructure undefined or null"));
            }
            _ => {}
        }
        let mut consumed = Vec::with_capacity(properties.len());
        for property in properties {
            let key = self.property_key_string(&property.key)?;
            consumed.push(key.clone());
            let item = self.get_property_checked(value, &key)?;
            bind(self, &property.value, item)?;
        }
        if let Some(rest) = rest {
            let object = self.new_object();
            let mark = self.temp_roots.len();
            self.temp_roots.push(object.clone());
            let outcome = (|| -> Result<(), JsError> {
                if matches!(value, Value::Object(_)) {
                    for key in self.object_keys(value) {
                        if consumed.iter().any(|existing| existing == &key) {
                            continue;
                        }
                        let item = self.get_property_checked(value, &key)?;
                        self.set_property_checked(&object, &key, item)?;
                    }
                }
                Ok(())
            })();
            self.temp_roots.truncate(mark);
            outcome?;
            bind(self, rest, object)?;
        }
        Ok(())
    }

    /// Execute an `export` declaration. Named and star exports are already
    /// linked during instantiation; declarations and default values run here.
    fn exec_export(&mut self, export: &ExportDecl) -> Result<Completion, JsError> {
        match &export.kind {
            ExportKind::DefaultExpr(expr) => {
                let value = self.eval_expr(expr)?;
                if let Some(scope) = self.current_module_scope() {
                    self.initialize_binding(scope, "default", value.clone())?;
                }
                self.completion_value = value;
                Ok(Completion::Normal)
            }
            ExportKind::DefaultDecl(inner) => {
                self.exec_stmt(inner, None)?;
                if let Some(scope) = self.current_module_scope() {
                    let name = match inner.as_ref() {
                        Stmt::FunctionDecl(function) => function
                            .name
                            .clone()
                            .unwrap_or_else(|| "default".to_owned()),
                        Stmt::ClassDecl(class) => {
                            class.name.clone().unwrap_or_else(|| "default".to_owned())
                        }
                        _ => "default".to_owned(),
                    };
                    let value = self.read_binding_from_scope(scope, &name)?;
                    self.initialize_binding(scope, "default", value)?;
                }
                Ok(Completion::Normal)
            }
            ExportKind::Decl(inner) => self.exec_stmt(inner, None),
            ExportKind::Named { .. } | ExportKind::All { .. } => Ok(Completion::Normal),
        }
    }

    pub(crate) fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Completion, JsError> {
        let scope = self.create_scope(Some(self.current_scope()), false);
        self.prepare_lexical(stmts, scope);
        self.prepare_block_functions(stmts, scope);
        self.env_stack.push(scope);
        let result = self.exec_stmts(stmts);
        self.env_stack.pop();
        result
    }

    pub(crate) fn prepare_lexical(&mut self, stmts: &[Stmt], scope: ScopeId) {
        for stmt in stmts {
            if let Stmt::Export(export) = stmt
                && matches!(export.kind, ExportKind::DefaultExpr(_))
            {
                self.declare_binding(scope, "default", BindingKind::Let, Value::Undefined, false);
            }
            let Some(inner) = exported_declaration(stmt) else {
                continue;
            };
            if let Stmt::ClassDecl(class) = inner
                && let Some(name) = &class.name
            {
                self.declare_binding(scope, name, BindingKind::Let, Value::Undefined, false);
            }
            if let Stmt::VarDecl(declaration) = inner
                && declaration.kind != DeclKind::Var
            {
                let kind = if declaration.kind == DeclKind::Const {
                    BindingKind::Const
                } else {
                    BindingKind::Let
                };
                for declarator in &declaration.declarations {
                    let mut names = Vec::new();
                    declarator.pattern.bound_names(&mut names);
                    for name in names {
                        self.declare_binding(scope, &name, kind, Value::Undefined, false);
                    }
                }
            }
        }
    }

    fn prepare_block_functions(&mut self, stmts: &[Stmt], scope: ScopeId) {
        for stmt in stmts {
            if let Stmt::FunctionDecl(function) = stmt
                && let Some(name) = &function.name
            {
                let value = self.create_function_from_ast(function, scope);
                self.declare_binding(scope, name, BindingKind::Function, value, true);
            }
        }
    }

    pub(crate) fn hoist_vars(&mut self, stmts: &[Stmt]) {
        let mut names = Vec::new();
        collect_var_names(stmts, &mut names);
        let scope = self.var_scope();
        for name in names {
            let exists = self
                .scope(scope)
                .is_some_and(|scope| scope.bindings.iter().any(|(bound, _)| bound == &name));
            if !exists {
                self.declare_binding(scope, &name, BindingKind::Var, Value::Undefined, true);
            }
        }
    }

    pub(crate) fn hoist_functions(&mut self, stmts: &[Stmt], scope: ScopeId) {
        for stmt in stmts {
            let Some(inner) = exported_declaration(stmt) else {
                continue;
            };
            if let Stmt::FunctionDecl(function) = inner
                && let Some(name) = &function.name
            {
                let value = self.create_function_from_ast(function, scope);
                self.declare_binding(scope, name, BindingKind::Function, value, true);
            }
        }
    }

    fn create_function_from_ast(&mut self, function: &Function, env: ScopeId) -> Value {
        let name = function.name.clone().unwrap_or_default();
        let user = UserFunction {
            name: name.clone(),
            params: function.params.clone(),
            patterns: function.patterns.clone(),
            body: function.body.clone(),
            env,
            is_arrow: function.is_arrow,
            is_generator: function.is_generator,
            is_async: function.is_async,
        };
        let value =
            self.new_user_function_value(&name, function_length(&function.patterns), Rc::new(user));
        if !function.is_arrow {
            let mark = self.temp_roots.len();
            self.temp_roots.push(value.clone());
            let prototype = self.new_object();
            self.set_property(prototype.clone(), "constructor", value.clone());
            if let Value::Object(id) = &value {
                self.define_data_property(*id, "prototype", prototype);
            }
            self.temp_roots.truncate(mark);
        }
        value
    }

    fn exec_while(
        &mut self,
        test: &Expr,
        body: &Stmt,
        label: Option<&str>,
    ) -> Result<Completion, JsError> {
        loop {
            self.tick()?;
            if !self.eval_expr(test)?.is_truthy() {
                break;
            }
            match loop_action(self.exec_stmt(body, None)?, label) {
                LoopAction::Continue => {}
                LoopAction::Break => break,
                LoopAction::Propagate(completion) => return Ok(completion),
            }
        }
        Ok(Completion::Normal)
    }

    fn exec_do_while(
        &mut self,
        body: &Stmt,
        test: &Expr,
        label: Option<&str>,
    ) -> Result<Completion, JsError> {
        loop {
            self.tick()?;
            match loop_action(self.exec_stmt(body, None)?, label) {
                LoopAction::Continue => {}
                LoopAction::Break => break,
                LoopAction::Propagate(completion) => return Ok(completion),
            }
            if !self.eval_expr(test)?.is_truthy() {
                break;
            }
        }
        Ok(Completion::Normal)
    }

    fn exec_for(
        &mut self,
        init: Option<&ForInit>,
        test: Option<&Expr>,
        update: Option<&Expr>,
        body: &Stmt,
        label: Option<&str>,
    ) -> Result<Completion, JsError> {
        let mut loop_scope = None;
        if let Some(ForInit::Var(declaration)) = init
            && declaration.kind != DeclKind::Var
        {
            let scope = self.create_scope(Some(self.current_scope()), false);
            let kind = if declaration.kind == DeclKind::Const {
                BindingKind::Const
            } else {
                BindingKind::Let
            };
            for declarator in &declaration.declarations {
                let mut names = Vec::new();
                declarator.pattern.bound_names(&mut names);
                for name in names {
                    self.declare_binding(scope, &name, kind, Value::Undefined, false);
                }
            }
            self.env_stack.push(scope);
            loop_scope = Some(scope);
        }

        let outcome = (|| -> Result<Completion, JsError> {
            match init {
                Some(ForInit::Var(declaration)) => self.exec_var_decl(declaration)?,
                Some(ForInit::Expr(expr)) => {
                    self.eval_expr(expr)?;
                }
                None => {}
            }
            loop {
                self.tick()?;
                if let Some(test) = test
                    && !self.eval_expr(test)?.is_truthy()
                {
                    break;
                }
                match loop_action(self.exec_stmt(body, None)?, label) {
                    LoopAction::Continue => {}
                    LoopAction::Break => break,
                    LoopAction::Propagate(completion) => return Ok(completion),
                }
                if let Some(update) = update {
                    self.eval_expr(update)?;
                }
            }
            Ok(Completion::Normal)
        })();

        if loop_scope.is_some() {
            self.env_stack.pop();
        }
        outcome
    }

    fn exec_for_in(
        &mut self,
        left: &ForTarget,
        right: &Expr,
        is_of: bool,
        body: &Stmt,
        label: Option<&str>,
    ) -> Result<Completion, JsError> {
        let iterable = self.eval_expr(right)?;
        let values: Vec<Value> = if is_of {
            self.iterate_values(&iterable)?
        } else {
            match &iterable {
                Value::Object(_) => self
                    .object_keys(&iterable)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
                Value::String(text) => (0..text.chars().count())
                    .map(|index| Value::Number(index as f64))
                    .collect(),
                _ => Vec::new(),
            }
        };

        for item in values {
            self.tick()?;
            let per_iteration = match left {
                ForTarget::Var { kind, .. } | ForTarget::Pattern { kind, .. } => {
                    *kind != DeclKind::Var
                }
                ForTarget::Expr(_) => false,
            };
            let scope = if per_iteration {
                Some(self.create_scope(Some(self.current_scope()), false))
            } else {
                None
            };
            let outcome = self.exec_for_iteration(left, item, scope, body);
            match loop_action(outcome?, label) {
                LoopAction::Continue => {}
                LoopAction::Break => return Ok(Completion::Normal),
                LoopAction::Propagate(completion) => return Ok(completion),
            }
        }
        Ok(Completion::Normal)
    }

    /// Bind one iteration of a `for...in`/`for...of` loop and execute its body.
    fn exec_for_iteration(
        &mut self,
        left: &ForTarget,
        item: Value,
        scope: Option<ScopeId>,
        body: &Stmt,
    ) -> Result<Completion, JsError> {
        if let Some(scope) = scope {
            self.env_stack.push(scope);
        }
        let result = (|| match left {
            ForTarget::Var { kind, name } => {
                if *kind == DeclKind::Var {
                    if !self.assign_binding(name, item.clone())? {
                        self.declare_var(name, item.clone());
                    }
                } else {
                    let binding_kind = if *kind == DeclKind::Const {
                        BindingKind::Const
                    } else {
                        BindingKind::Let
                    };
                    self.declare_binding(
                        scope.expect("per iteration scope"),
                        name,
                        binding_kind,
                        item.clone(),
                        true,
                    );
                }
                self.exec_stmt(body, None)
            }
            ForTarget::Pattern { kind, pattern } => {
                if let Some(scope) = scope {
                    let mut names = Vec::new();
                    pattern.bound_names(&mut names);
                    let binding_kind = if *kind == DeclKind::Const {
                        BindingKind::Const
                    } else {
                        BindingKind::Let
                    };
                    for name in names {
                        self.declare_binding(scope, &name, binding_kind, Value::Undefined, false);
                    }
                }
                self.bind_decl_pattern(pattern, item.clone(), *kind)?;
                self.exec_stmt(body, None)
            }
            ForTarget::Expr(target) => {
                self.assign_target(target, item)?;
                self.exec_stmt(body, None)
            }
        })();
        if scope.is_some() {
            self.env_stack.pop();
        }
        result
    }

    fn exec_switch(
        &mut self,
        discriminant: &Expr,
        cases: &[SwitchCase],
        label: Option<&str>,
    ) -> Result<Completion, JsError> {
        let value = self.eval_expr(discriminant)?;
        let mut start = None;
        let mut default_index = None;
        for (index, case) in cases.iter().enumerate() {
            match &case.test {
                None => default_index = Some(index),
                Some(test) if start.is_none() => {
                    let test_value = self.eval_expr(test)?;
                    if value.strict_equals(&test_value) {
                        start = Some(index);
                    }
                }
                Some(_) => {}
            }
        }
        let Some(start) = start.or(default_index) else {
            return Ok(Completion::Normal);
        };
        for case in &cases[start..] {
            match self.exec_stmts(&case.body)? {
                Completion::Normal => {}
                Completion::Break(None) => return Ok(Completion::Normal),
                Completion::Break(Some(name)) if label == Some(name.as_str()) => {
                    return Ok(Completion::Normal);
                }
                other => return Ok(other),
            }
        }
        Ok(Completion::Normal)
    }

    fn exec_try(
        &mut self,
        block: &[Stmt],
        handler: Option<&CatchClause>,
        finalizer: Option<&[Stmt]>,
    ) -> Result<Completion, JsError> {
        let result = match self.exec_block(block) {
            Ok(completion) => Ok(completion),
            Err(error) => {
                if let Some(handler) = handler {
                    let scope = self.create_scope(Some(self.current_scope()), false);
                    if let Some(param) = &handler.param {
                        self.declare_binding(
                            scope,
                            param,
                            BindingKind::Let,
                            error.thrown_value(),
                            true,
                        );
                    }
                    self.env_stack.push(scope);
                    let outcome = self.exec_stmts(&handler.body);
                    self.env_stack.pop();
                    outcome
                } else {
                    Err(error)
                }
            }
        };
        if let Some(finalizer) = finalizer {
            match self.exec_block(finalizer) {
                Ok(Completion::Normal) => {}
                Ok(other) => return Ok(other),
                Err(error) => return Err(error),
            }
        }
        result
    }

    pub(crate) fn eval_expr(&mut self, expr: &Expr) -> Result<Value, JsError> {
        match expr {
            Expr::Number(value) => Ok(Value::Number(*value)),
            Expr::String(value) => Ok(Value::String(value.clone())),
            Expr::Bool(value) => Ok(Value::Bool(*value)),
            Expr::Null => Ok(Value::Null),
            Expr::This => Ok(self
                .read_binding("this")
                .unwrap_or(Value::Object(self.global))),
            Expr::Identifier(name) => self.read_binding(name),
            Expr::Array(items) => self.eval_array(items),
            Expr::Object(properties) => self.eval_object(properties),
            Expr::Regex { pattern, flags } => self.new_regexp_value(pattern, flags),
            Expr::AssignPattern { pattern, value } => {
                let assigned = self.eval_expr(value)?;
                self.assign_pattern(pattern, assigned.clone())?;
                Ok(assigned)
            }
            Expr::Function(function) | Expr::Arrow(function) => {
                let env = self.current_scope();
                Ok(self.create_function_from_ast(function, env))
            }
            Expr::Unary { op, argument } => self.eval_unary(*op, argument),
            Expr::Update {
                op,
                prefix,
                argument,
            } => self.eval_update(*op, *prefix, argument),
            Expr::Binary { op, left, right } => {
                let mark = self.temp_roots.len();
                let left_value = match self.eval_expr(left) {
                    Ok(value) => value,
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                };
                self.temp_roots.push(left_value.clone());
                let outcome = match self.eval_expr(right) {
                    Ok(right_value) => self.apply_binary(*op, &left_value, &right_value),
                    Err(error) => Err(error),
                };
                self.temp_roots.truncate(mark);
                outcome
            }
            Expr::Logical { op, left, right } => {
                let left_value = self.eval_expr(left)?;
                match op {
                    LogicalOp::And => {
                        if left_value.is_truthy() {
                            self.eval_expr(right)
                        } else {
                            Ok(left_value)
                        }
                    }
                    LogicalOp::Or => {
                        if left_value.is_truthy() {
                            Ok(left_value)
                        } else {
                            self.eval_expr(right)
                        }
                    }
                    LogicalOp::Nullish => match left_value {
                        Value::Undefined | Value::Null => self.eval_expr(right),
                        other => Ok(other),
                    },
                }
            }
            Expr::Assign { op, target, value } => self.eval_assign(*op, target, value),
            Expr::Conditional {
                test,
                consequent,
                alternate,
            } => {
                if self.eval_expr(test)?.is_truthy() {
                    self.eval_expr(consequent)
                } else {
                    self.eval_expr(alternate)
                }
            }
            Expr::Call { callee, arguments } => self.eval_call(callee, arguments),
            Expr::New { callee, arguments } => self.eval_new(callee, arguments),
            Expr::Member {
                object,
                property,
                computed,
            } => self.eval_member(object, property, *computed),
            Expr::Sequence(expressions) => {
                let mut last = Value::Undefined;
                for expression in expressions {
                    last = self.eval_expr(expression)?;
                }
                Ok(last)
            }
            Expr::Template { parts, exprs } => {
                let mut out = String::new();
                for (index, part) in parts.iter().enumerate() {
                    out.push_str(part);
                    if let Some(expression) = exprs.get(index) {
                        let value = self.eval_expr(expression)?;
                        out.push_str(&self.to_string_rt(&value)?);
                    }
                }
                Ok(Value::String(out))
            }
            Expr::Class(class) => self.create_class(class),
            Expr::Super => Err(JsError::syntax_error(
                "`super` is only valid in a method or derived constructor",
            )),
            Expr::Yield { .. } => Err(JsError::syntax_error(
                "`yield` is only valid inside a generator function",
            )),
            Expr::Await(_) => Err(JsError::syntax_error(
                "`await` is only valid inside an async function",
            )),
            Expr::OptionalChain(chain) => self.eval_optional_chain(chain),
            Expr::Spread(_) => Err(JsError::syntax_error(
                "spread is only valid in array literals and call arguments",
            )),
            Expr::ImportCall(argument) => {
                let specifier = self.eval_expr(argument)?;
                let specifier = crate::Runtime::import_specifier(&specifier)?;
                self.import_module(&specifier)
            }
            Expr::MetaProperty { meta, property } => {
                if meta != "import" || property != "meta" {
                    return Err(JsError::syntax_error(format!(
                        "unsupported meta property {meta}.{property}"
                    )));
                }
                let Some(url) = self.current_module_url().map(str::to_owned) else {
                    return Err(JsError::syntax_error(
                        "import.meta is only valid inside a module",
                    ));
                };
                let object = self.new_object();
                if let Value::Object(id) = object {
                    self.define_data_property(id, "url", Value::String(url.clone()));
                    let resolve = self.new_native_value(
                        "resolve",
                        1,
                        Box::new(move |_runtime, _this, args| {
                            let value = args.first().cloned().unwrap_or(Value::Undefined);
                            let specifier = crate::Runtime::import_specifier(&value)?;
                            let resolved =
                                crate::module::resolve_specifier(&specifier, Some(url.as_str()));
                            Ok(Value::String(resolved))
                        }),
                    );
                    self.define_data_property(id, "resolve", resolve);
                }
                Ok(object)
            }
        }
    }

    /// Evaluate `a?.b?.[c]?.(d)`, short circuiting the rest of the chain as
    /// soon as an optional step sees `null` or `undefined`.
    fn eval_optional_chain(&mut self, chain: &OptionalChain) -> Result<Value, JsError> {
        let mut value = self.eval_expr(&chain.base)?;
        for op in &chain.ops {
            match op {
                OptionalChainOp::Member {
                    property,
                    computed,
                    optional,
                } => {
                    if *optional && matches!(value, Value::Undefined | Value::Null) {
                        return Ok(Value::Undefined);
                    }
                    let key = self.property_key(property, *computed)?;
                    value = self.get_property_checked(&value, &key)?;
                }
                OptionalChainOp::Call {
                    arguments,
                    optional,
                } => {
                    if *optional && matches!(value, Value::Undefined | Value::Null) {
                        return Ok(Value::Undefined);
                    }
                    let args = self.eval_arguments(arguments)?;
                    value = self.call_function(&value, Value::Undefined, args)?;
                }
            }
        }
        Ok(value)
    }

    /// Evaluate call arguments, expanding `...spread` elements into their
    /// iterator values.
    fn eval_arguments(&mut self, arguments: &[Expr]) -> Result<Vec<Value>, JsError> {
        let mut args = Vec::new();
        for argument in arguments {
            if let Expr::Spread(inner) = argument {
                let value = self.eval_expr(inner)?;
                let items = self.iterate_values(&value)?;
                args.extend(items);
            } else {
                args.push(self.eval_expr(argument)?);
            }
        }
        Ok(args)
    }

    /// Materialise the values produced by an iterable using the
    /// `Symbol.iterator`/`next()` protocol.
    pub(crate) fn iterate_values(&mut self, value: &Value) -> Result<Vec<Value>, JsError> {
        let (iterator, next) = self.get_iterator(value)?;
        let mut items = Vec::new();
        loop {
            self.tick()?;
            let result = self.call_function(&next, iterator.clone(), Vec::new())?;
            if self.get_property_checked(&result, "done")?.is_truthy() {
                break;
            }
            items.push(self.get_property_checked(&result, "value")?);
        }
        Ok(items)
    }

    /// Obtain `(iterator, next)` for a value, raising `TypeError` when it does
    /// not implement the iteration protocol.
    pub(crate) fn get_iterator(&mut self, value: &Value) -> Result<(Value, Value), JsError> {
        let method = self.get_property_checked(value, SYMBOL_ITERATOR_KEY)?;
        if !self.is_callable(&method) {
            return Err(JsError::type_error("value is not iterable"));
        }
        let iterator = self.call_function(&method, value.clone(), Vec::new())?;
        let next = self.get_property_checked(&iterator, "next")?;
        if !self.is_callable(&next) {
            return Err(JsError::type_error("iterator next is not callable"));
        }
        Ok((iterator, next))
    }

    fn eval_array(&mut self, items: &[Option<Expr>]) -> Result<Value, JsError> {
        let mark = self.temp_roots.len();
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Some(Expr::Spread(inner)) => match self
                    .eval_expr(inner)
                    .and_then(|value| self.iterate_values(&value))
                {
                    Ok(expanded) => {
                        for value in expanded {
                            self.temp_roots.push(value.clone());
                            values.push(value);
                        }
                    }
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                },
                Some(expr) => match self.eval_expr(expr) {
                    Ok(value) => {
                        self.temp_roots.push(value.clone());
                        values.push(value);
                    }
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                },
                None => values.push(Value::Undefined),
            }
        }
        let array = self.new_array(values);
        self.temp_roots.truncate(mark);
        Ok(array)
    }

    fn eval_object(
        &mut self,
        properties: &[browser_js_parser::Property],
    ) -> Result<Value, JsError> {
        let object = self.new_object();
        let mark = self.temp_roots.len();
        self.temp_roots.push(object.clone());
        let outcome = (|| -> Result<(), JsError> {
            for property in properties {
                let key = self.property_key_string(&property.key)?;
                let value = self.eval_expr(&property.value)?;
                self.set_property_checked(&object, &key, value)?;
            }
            Ok(())
        })();
        self.temp_roots.truncate(mark);
        outcome?;
        Ok(object)
    }

    fn eval_member(
        &mut self,
        object: &Expr,
        property: &Expr,
        computed: bool,
    ) -> Result<Value, JsError> {
        let mark = self.temp_roots.len();
        let result = (|| {
            if matches!(object, Expr::Super) {
                let this = self.current_this();
                let home = self.read_binding(SUPER_HOME)?;
                let key = self.property_key(property, computed)?;
                return match home {
                    Value::Object(id) => {
                        let start = self.object(id).and_then(|object| object.prototype);
                        Ok(match start {
                            Some(start) => self
                                .lookup_property(&this, start, &key)?
                                .unwrap_or(Value::Undefined),
                            None => Value::Undefined,
                        })
                    }
                    _ => Ok(Value::Undefined),
                };
            }
            let base = self.eval_expr(object)?;
            self.temp_roots.push(base.clone());
            let key = self.property_key(property, computed)?;
            self.get_property_checked(&base, &key)
        })();
        self.temp_roots.truncate(mark);
        result
    }

    fn current_this(&mut self) -> Value {
        self.read_binding("this")
            .unwrap_or(Value::Object(self.global))
    }

    fn eval_call(&mut self, callee: &Expr, arguments: &[Expr]) -> Result<Value, JsError> {
        let mark = self.temp_roots.len();
        let outcome = (|| -> Result<Value, JsError> {
            let (function, this) = if matches!(callee, Expr::Super) {
                let constructor = self.read_binding(SUPER_CTOR)?;
                let this = self.current_this();
                (constructor, this)
            } else if let Expr::Member {
                object,
                property,
                computed,
            } = callee
            {
                if matches!(object.as_ref(), Expr::Super) {
                    let this = self.current_this();
                    let home = self.read_binding(SUPER_HOME)?;
                    let key = self.property_key(property, *computed)?;
                    let function = match home {
                        Value::Object(id) => {
                            let start = self.object(id).and_then(|object| object.prototype);
                            match start {
                                Some(start) => self
                                    .lookup_property(&this, start, &key)?
                                    .unwrap_or(Value::Undefined),
                                None => Value::Undefined,
                            }
                        }
                        _ => Value::Undefined,
                    };
                    (function, this)
                } else {
                    let base = self.eval_expr(object)?;
                    self.temp_roots.push(base.clone());
                    let key = self.property_key(property, *computed)?;
                    let function = self.get_property_checked(&base, &key)?;
                    (function, base)
                }
            } else {
                (self.eval_expr(callee)?, Value::Undefined)
            };
            self.temp_roots.push(function.clone());
            self.temp_roots.push(this.clone());
            let args = self.eval_arguments(arguments)?;
            for value in &args {
                self.temp_roots.push(value.clone());
            }
            self.call_function(&function, this, args)
        })();
        self.temp_roots.truncate(mark);
        outcome
    }

    fn eval_new(&mut self, callee: &Expr, arguments: &[Expr]) -> Result<Value, JsError> {
        let mark = self.temp_roots.len();
        let outcome = (|| -> Result<Value, JsError> {
            let constructor = self.eval_expr(callee)?;
            self.temp_roots.push(constructor.clone());
            let args = self.eval_arguments(arguments)?;
            for value in &args {
                self.temp_roots.push(value.clone());
            }
            self.construct_value(constructor.clone(), args, constructor)
        })();
        self.temp_roots.truncate(mark);
        outcome
    }

    /// Resolve a value used as a property key, translating well-known symbols
    /// to their private internal keys.
    fn key_from_value(&mut self, value: &Value) -> Result<String, JsError> {
        if let Value::Object(id) = value
            && let Some(Value::String(key)) = self.get_own_property(*id, "\u{0}symbol_key")
        {
            return Ok(key);
        }
        self.to_string_rt(value)
    }

    /// Resolve a property key from an AST key, evaluating computed keys.
    fn property_key_string(&mut self, key: &PropertyKey) -> Result<String, JsError> {
        match key {
            PropertyKey::Computed(expr) => {
                let value = self.eval_expr(expr)?;
                self.key_from_value(&value)
            }
            other => Ok(other.as_key_string()),
        }
    }

    fn property_key(&mut self, property: &Expr, computed: bool) -> Result<String, JsError> {
        if computed {
            let value = self.eval_expr(property)?;
            self.key_from_value(&value)
        } else {
            match property {
                Expr::Identifier(name) => Ok(name.clone()),
                Expr::String(name) => Ok(name.clone()),
                _ => Err(JsError::syntax_error("invalid property name")),
            }
        }
    }

    fn eval_unary(&mut self, op: UnaryOp, argument: &Expr) -> Result<Value, JsError> {
        match op {
            UnaryOp::TypeOf => {
                if let Expr::Identifier(name) = argument
                    && self.resolve_scope(name).is_none()
                    && !self.has_own_property(self.global, name)
                {
                    return Ok(Value::String("undefined".to_owned()));
                }
                let value = self.eval_expr(argument)?;
                Ok(Value::String(self.typeof_value(&value).to_owned()))
            }
            UnaryOp::Delete => match argument {
                Expr::Member { .. } => {
                    let mark = self.temp_roots.len();
                    let outcome = (|| -> Result<Value, JsError> {
                        let Expr::Member {
                            object,
                            property,
                            computed,
                        } = argument
                        else {
                            return Err(JsError::syntax_error("invalid delete target"));
                        };
                        let base = self.eval_expr(object)?;
                        self.temp_roots.push(base.clone());
                        let key = self.property_key(property, *computed)?;
                        Ok(Value::Bool(self.delete_property(base, &key)))
                    })();
                    self.temp_roots.truncate(mark);
                    outcome
                }
                Expr::Identifier(_) => Ok(Value::Bool(false)),
                _ => {
                    self.eval_expr(argument)?;
                    Ok(Value::Bool(true))
                }
            },
            UnaryOp::Void => {
                self.eval_expr(argument)?;
                Ok(Value::Undefined)
            }
            UnaryOp::Not => Ok(Value::Bool(!self.eval_expr(argument)?.is_truthy())),
            UnaryOp::Minus => {
                let value = self.eval_expr(argument)?;
                Ok(Value::Number(-self.to_number_rt(&value)?))
            }
            UnaryOp::Plus => {
                let value = self.eval_expr(argument)?;
                Ok(Value::Number(self.to_number_rt(&value)?))
            }
            UnaryOp::BitNot => {
                let value = self.eval_expr(argument)?;
                let number = self.to_number_rt(&value)?;
                Ok(Value::Number(f64::from(!to_int32(number))))
            }
        }
    }

    fn eval_update(
        &mut self,
        op: UpdateOp,
        prefix: bool,
        argument: &Expr,
    ) -> Result<Value, JsError> {
        let old_value = self.read_target(argument)?;
        let old_number = self.to_number_rt(&old_value)?;
        let new_number = match op {
            UpdateOp::Increment => old_number + 1.0,
            UpdateOp::Decrement => old_number - 1.0,
        };
        self.assign_target(argument, Value::Number(new_number))?;
        Ok(if prefix {
            Value::Number(new_number)
        } else {
            Value::Number(old_number)
        })
    }

    fn read_target(&mut self, target: &Expr) -> Result<Value, JsError> {
        match target {
            Expr::Identifier(name) => self.read_binding(name),
            Expr::Member {
                object,
                property,
                computed,
            } => self.eval_member(object, property, *computed),
            _ => Err(JsError::reference_error("invalid increment target")),
        }
    }

    fn eval_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value_expr: &Expr,
    ) -> Result<Value, JsError> {
        match target {
            Expr::Identifier(name) => {
                let old_value = if op == AssignOp::Assign {
                    None
                } else {
                    Some(self.read_binding(name)?)
                };
                let value = self.eval_expr(value_expr)?;
                let new_value = self.apply_assignment(op, old_value, &value)?;
                if !self.assign_binding(name, new_value.clone())? {
                    let global_scope = self.global_scope;
                    self.declare_binding(
                        global_scope,
                        name,
                        BindingKind::Var,
                        new_value.clone(),
                        true,
                    );
                }
                Ok(new_value)
            }
            Expr::Member {
                object,
                property,
                computed,
            } => {
                let mark = self.temp_roots.len();
                let base = match self.eval_expr(object) {
                    Ok(value) => value,
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                };
                self.temp_roots.push(base.clone());
                let key = match self.property_key(property, *computed) {
                    Ok(key) => key,
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                };
                let old_value = if op == AssignOp::Assign {
                    None
                } else {
                    match self.get_property_checked(&base, &key) {
                        Ok(value) => Some(value),
                        Err(error) => {
                            self.temp_roots.truncate(mark);
                            return Err(error);
                        }
                    }
                };
                let value = match self.eval_expr(value_expr) {
                    Ok(value) => value,
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                };
                let outcome = (|| -> Result<Value, JsError> {
                    let new_value = self.apply_assignment(op, old_value, &value)?;
                    self.set_property_checked(&base, &key, new_value.clone())?;
                    Ok(new_value)
                })();
                self.temp_roots.truncate(mark);
                outcome
            }
            _ => Err(JsError::reference_error("invalid assignment target")),
        }
    }

    fn apply_assignment(
        &mut self,
        op: AssignOp,
        old_value: Option<Value>,
        value: &Value,
    ) -> Result<Value, JsError> {
        let old_value = old_value.unwrap_or(Value::Undefined);
        let binary = match op {
            AssignOp::Assign => return Ok(value.clone()),
            AssignOp::Add => BinaryOp::Add,
            AssignOp::Sub => BinaryOp::Sub,
            AssignOp::Mul => BinaryOp::Mul,
            AssignOp::Div => BinaryOp::Div,
            AssignOp::Mod => BinaryOp::Mod,
            AssignOp::Exp => BinaryOp::Exp,
            AssignOp::Shl => BinaryOp::Shl,
            AssignOp::Shr => BinaryOp::Shr,
            AssignOp::UShr => BinaryOp::UShr,
        };
        self.apply_binary(binary, &old_value, value)
    }

    pub(crate) fn assign_target(&mut self, target: &Expr, value: Value) -> Result<(), JsError> {
        match target {
            Expr::Identifier(name) => {
                if !self.assign_binding(name, value.clone())? {
                    let global_scope = self.global_scope;
                    self.declare_binding(global_scope, name, BindingKind::Var, value, true);
                }
                Ok(())
            }
            Expr::Member {
                object,
                property,
                computed,
            } => {
                let mark = self.temp_roots.len();
                let base = match self.eval_expr(object) {
                    Ok(value) => value,
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                };
                self.temp_roots.push(base.clone());
                let key = match self.property_key(property, *computed) {
                    Ok(key) => key,
                    Err(error) => {
                        self.temp_roots.truncate(mark);
                        return Err(error);
                    }
                };
                let outcome = self.set_property_checked(&base, &key, value);
                self.temp_roots.truncate(mark);
                outcome
            }
            _ => Err(JsError::reference_error("invalid assignment target")),
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_primitive(&mut self, value: &Value) -> Result<Value, JsError> {
        let Value::Object(id) = value else {
            return Ok(value.clone());
        };
        let is_array = self
            .object(*id)
            .is_some_and(|object| matches!(&object.kind, ObjectKind::Array(_)));
        if is_array {
            let items = self.object_array_items(value).unwrap_or_default();
            let mut parts = Vec::with_capacity(items.len());
            for item in &items {
                match item {
                    Value::Undefined | Value::Null => parts.push(String::new()),
                    other => parts.push(self.to_string_rt(other)?),
                }
            }
            return Ok(Value::String(parts.join(",")));
        }

        let value_of = self.get_property_checked(value, "valueOf")?;
        if self.is_callable(&value_of) && !self.is_default_method(&value_of, "valueOf") {
            let result = self.call_function(&value_of, value.clone(), Vec::new())?;
            if !matches!(result, Value::Object(_)) {
                return Ok(result);
            }
        }
        let to_string = self.get_property_checked(value, "toString")?;
        if self.is_callable(&to_string) && !self.is_default_method(&to_string, "toString") {
            let result = self.call_function(&to_string, value.clone(), Vec::new())?;
            if !matches!(result, Value::Object(_)) {
                return Ok(result);
            }
        }
        Ok(Value::String("[object Object]".to_owned()))
    }

    fn is_default_method(&self, function: &Value, name: &str) -> bool {
        let Value::Object(function_id) = function else {
            return false;
        };
        let Some(default_value) = self.get_own_property(self.prototypes.object, name) else {
            return false;
        };
        matches!(default_value, Value::Object(id) if id == *function_id)
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_number_rt(&mut self, value: &Value) -> Result<f64, JsError> {
        if matches!(value, Value::Object(_)) {
            let primitive = self.to_primitive(value)?;
            Ok(primitive.to_number())
        } else {
            Ok(value.to_number())
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_string_rt(&mut self, value: &Value) -> Result<String, JsError> {
        if matches!(value, Value::Object(_)) {
            let primitive = self.to_primitive(value)?;
            Ok(primitive.to_string_value())
        } else {
            Ok(value.to_string_value())
        }
    }

    pub(crate) fn typeof_value(&self, value: &Value) -> &'static str {
        match value {
            Value::Object(id) => match self.object(*id).map(|object| &object.kind) {
                Some(ObjectKind::Function(_)) => "function",
                Some(ObjectKind::Symbol(_)) => "symbol",
                Some(ObjectKind::Generator(_)) => "object",
                Some(ObjectKind::Proxy { target, .. }) if self.is_callable(target) => "function",
                _ => "object",
            },
            other => other.type_of(),
        }
    }

    pub(crate) fn loose_equals_rt(&mut self, left: &Value, right: &Value) -> Result<bool, JsError> {
        if left.strict_equals(right) {
            return Ok(true);
        }
        match (left, right) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => Ok(true),
            (Value::Number(_), Value::String(_))
            | (Value::String(_), Value::Number(_))
            | (Value::Bool(_), _)
            | (_, Value::Bool(_)) => Ok(left.to_number() == right.to_number()),
            (Value::Object(_), Value::Undefined | Value::Null)
            | (Value::Undefined | Value::Null, Value::Object(_)) => Ok(false),
            (Value::Object(_), _) => {
                let primitive = self.to_primitive(left)?;
                self.loose_equals_rt(&primitive, right)
            }
            (_, Value::Object(_)) => {
                let primitive = self.to_primitive(right)?;
                self.loose_equals_rt(left, &primitive)
            }
            _ => Ok(false),
        }
    }

    pub(crate) fn apply_binary(
        &mut self,
        op: BinaryOp,
        left: &Value,
        right: &Value,
    ) -> Result<Value, JsError> {
        match op {
            BinaryOp::Add => {
                let left_primitive = self.to_primitive(left)?;
                let right_primitive = self.to_primitive(right)?;
                if matches!(left_primitive, Value::String(_))
                    || matches!(right_primitive, Value::String(_))
                {
                    Ok(Value::String(format!(
                        "{}{}",
                        left_primitive.to_string_value(),
                        right_primitive.to_string_value()
                    )))
                } else {
                    Ok(Value::Number(
                        left_primitive.to_number() + right_primitive.to_number(),
                    ))
                }
            }
            BinaryOp::Sub => Ok(Value::Number(
                self.to_number_rt(left)? - self.to_number_rt(right)?,
            )),
            BinaryOp::Mul => Ok(Value::Number(
                self.to_number_rt(left)? * self.to_number_rt(right)?,
            )),
            BinaryOp::Div => Ok(Value::Number(
                self.to_number_rt(left)? / self.to_number_rt(right)?,
            )),
            BinaryOp::Mod => Ok(Value::Number(
                self.to_number_rt(left)? % self.to_number_rt(right)?,
            )),
            BinaryOp::Exp => Ok(Value::Number(
                self.to_number_rt(left)?.powf(self.to_number_rt(right)?),
            )),
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let left_primitive = self.to_primitive(left)?;
                let right_primitive = self.to_primitive(right)?;
                let result = if let (Value::String(a), Value::String(b)) =
                    (&left_primitive, &right_primitive)
                {
                    match op {
                        BinaryOp::Lt => a < b,
                        BinaryOp::Le => a <= b,
                        BinaryOp::Gt => a > b,
                        _ => a >= b,
                    }
                } else {
                    let a = left_primitive.to_number();
                    let b = right_primitive.to_number();
                    match op {
                        BinaryOp::Lt => a < b,
                        BinaryOp::Le => a <= b,
                        BinaryOp::Gt => a > b,
                        _ => a >= b,
                    }
                };
                Ok(Value::Bool(result))
            }
            BinaryOp::Eq => Ok(Value::Bool(self.loose_equals_rt(left, right)?)),
            BinaryOp::Ne => Ok(Value::Bool(!self.loose_equals_rt(left, right)?)),
            BinaryOp::StrictEq => Ok(Value::Bool(left.strict_equals(right))),
            BinaryOp::StrictNe => Ok(Value::Bool(!left.strict_equals(right))),
            BinaryOp::BitAnd => {
                let a = to_int32(self.to_number_rt(left)?);
                let b = to_int32(self.to_number_rt(right)?);
                Ok(Value::Number(f64::from(a & b)))
            }
            BinaryOp::BitOr => {
                let a = to_int32(self.to_number_rt(left)?);
                let b = to_int32(self.to_number_rt(right)?);
                Ok(Value::Number(f64::from(a | b)))
            }
            BinaryOp::BitXor => {
                let a = to_int32(self.to_number_rt(left)?);
                let b = to_int32(self.to_number_rt(right)?);
                Ok(Value::Number(f64::from(a ^ b)))
            }
            BinaryOp::Shl => {
                let a = to_int32(self.to_number_rt(left)?);
                let shift = to_uint32(self.to_number_rt(right)?) & 31;
                Ok(Value::Number(f64::from(a.wrapping_shl(shift))))
            }
            BinaryOp::Shr => {
                let a = to_int32(self.to_number_rt(left)?);
                let shift = to_uint32(self.to_number_rt(right)?) & 31;
                Ok(Value::Number(f64::from(a.wrapping_shr(shift))))
            }
            BinaryOp::UShr => {
                let a = to_uint32(self.to_number_rt(left)?);
                let shift = to_uint32(self.to_number_rt(right)?) & 31;
                Ok(Value::Number(f64::from(a.wrapping_shr(shift))))
            }
            BinaryOp::In => {
                if !matches!(right, Value::Object(_)) {
                    return Err(JsError::type_error(
                        "right-hand side of 'in' is not an object",
                    ));
                }
                let key = self.to_string_rt(left)?;
                Ok(Value::Bool(self.object_has(right, &key)))
            }
            BinaryOp::InstanceOf => self.instance_of(left, right),
        }
    }

    fn instance_of(&mut self, left: &Value, right: &Value) -> Result<Value, JsError> {
        if !self.is_callable(right) {
            return Err(JsError::type_error(
                "right-hand side of 'instanceof' is not callable",
            ));
        }
        let prototype = self.get_property_checked(right, "prototype")?;
        let Value::Object(prototype_id) = prototype else {
            return Err(JsError::type_error(
                "function has non-object prototype in instanceof",
            ));
        };
        if !matches!(left, Value::Object(_)) {
            return Ok(Value::Bool(false));
        }
        let mut current = self.prototype_of(left);
        while let Some(id) = current {
            if id == prototype_id {
                return Ok(Value::Bool(true));
            }
            current = self.object(id).and_then(|object| object.prototype);
        }
        Ok(Value::Bool(false))
    }

    /// Build a class constructor, its prototype (with methods and accessors),
    /// and its static methods, wiring `super` for derived classes.
    fn create_class(&mut self, class: &Class) -> Result<Value, JsError> {
        let super_class = match &class.super_class {
            Some(expr) => Some(self.eval_expr(expr)?),
            None => None,
        };
        let super_prototype = match &super_class {
            Some(value) => match self.get_property_checked(value, "prototype")? {
                Value::Object(id) => Some(id),
                _ => Some(self.prototypes.object),
            },
            None => Some(self.prototypes.object),
        };
        let prototype_id = self.alloc_object(ObjectKind::Ordinary, super_prototype);
        let prototype = Value::Object(prototype_id);
        let env = self.current_scope();

        let empty_constructor = Function {
            name: class.name.clone(),
            params: Vec::new(),
            patterns: Vec::new(),
            body: Vec::new(),
            is_arrow: false,
            is_generator: false,
            is_async: false,
            pos: class.pos,
        };
        let constructor_ast = class
            .body
            .iter()
            .find_map(|member| match member {
                ClassMember::Method {
                    key,
                    function,
                    is_static: false,
                    ..
                } if key.as_key_string() == "constructor" => Some(function),
                _ => None,
            })
            .unwrap_or(&empty_constructor);
        let constructor = self.create_function_from_ast(constructor_ast, env);
        if let Some(value) = &super_class {
            self.set_function_super_ctor(constructor.clone(), value.clone());
            // Static members are inherited through the constructor's own
            // prototype chain.
            if let (Value::Object(constructor_id), Value::Object(super_id)) = (&constructor, value)
                && let Some(object) = self.object_mut(*constructor_id)
            {
                object.prototype = Some(*super_id);
            }
        }
        self.define_data_property(prototype_id, "constructor", constructor.clone());
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", prototype.clone());
        }

        for member in &class.body {
            let ClassMember::Method {
                key,
                function,
                is_static,
                kind,
            } = member;
            let name = self.property_key_string(key)?;
            if !is_static && name == "constructor" {
                continue;
            }
            let method = self.create_function_from_ast(function, env);
            let target = if *is_static {
                match &constructor {
                    Value::Object(id) => *id,
                    _ => continue,
                }
            } else {
                prototype_id
            };
            // `super` looks up members starting at the home object's
            // prototype: the superclass prototype for instance methods and
            // the superclass constructor for static methods.
            self.set_function_home(method.clone(), target);
            match kind {
                MethodKind::Method => self.define_data_property(target, &name, method),
                MethodKind::Getter | MethodKind::Setter => {
                    let (mut getter, mut setter) = self.accessor_pair(target, &name);
                    if *kind == MethodKind::Getter {
                        getter = Some(method);
                    } else {
                        setter = Some(method);
                    }
                    self.define_accessor_property(target, &name, getter, setter);
                }
            }
        }
        Ok(constructor)
    }

    pub(crate) fn error_from_throw(&mut self, value: Value) -> JsError {
        let message = match &value {
            Value::Object(_) => {
                let name = self
                    .get_property_checked(&value, "name")
                    .ok()
                    .map(|value| value.to_string_value());
                let message = self
                    .get_property_checked(&value, "message")
                    .ok()
                    .map(|value| value.to_string_value());
                match (name, message) {
                    (Some(name), Some(message)) if !message.is_empty() => {
                        format!("{name}: {message}")
                    }
                    (Some(name), Some(_)) => name,
                    (_, Some(message)) if !message.is_empty() => message,
                    _ => "[object Object]".to_owned(),
                }
            }
            other => other.to_string_value(),
        };
        JsError::new(message).with_value(value)
    }
}
