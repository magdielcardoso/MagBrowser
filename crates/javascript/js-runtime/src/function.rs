//! Function objects and calls.

use std::rc::Rc;

use browser_js_parser::Stmt;

use crate::errors::JsError;
use crate::object::{BuiltinFn, HostFunction, ObjectKind};
use crate::scope::{BindingKind, ScopeId};
use crate::value::{ObjectId, Value};

/// An interpreted (user written) function.
pub(crate) struct UserFunction {
    pub(crate) name: String,
    pub(crate) patterns: Vec<browser_js_parser::Pattern>,
    pub(crate) body: Vec<Stmt>,
    pub(crate) env: ScopeId,
    pub(crate) is_arrow: bool,
}

/// The implementation behind a function object.
pub(crate) enum FunctionImpl {
    /// A closure supplied by the embedding host.
    Native(HostFunction),
    /// A builtin implemented as a plain function pointer (reentrant).
    Builtin(BuiltinFn),
    /// A function written in JavaScript.
    User(Rc<UserFunction>),
    /// Placeholder used while a native closure is executing.
    Unavailable,
}

/// A callable heap object.
pub(crate) struct FunctionObject {
    pub(crate) name: String,
    pub(crate) length: usize,
    pub(crate) implementation: FunctionImpl,
}

impl crate::Runtime {
    /// Create a function object and return its identifier.
    pub(crate) fn new_function_object(
        &mut self,
        name: &str,
        length: usize,
        implementation: FunctionImpl,
    ) -> ObjectId {
        let prototype = self.prototypes.function;
        self.alloc_object(
            ObjectKind::Function(FunctionObject {
                name: name.to_owned(),
                length,
                implementation,
            }),
            Some(prototype),
        )
    }

    /// Create a builtin function value.
    pub(crate) fn new_builtin_value(&mut self, name: &str, length: usize, f: BuiltinFn) -> Value {
        Value::Object(self.new_function_object(name, length, FunctionImpl::Builtin(f)))
    }

    /// Create a host supplied function value.
    pub(crate) fn new_native_value(&mut self, name: &str, length: usize, f: HostFunction) -> Value {
        Value::Object(self.new_function_object(name, length, FunctionImpl::Native(f)))
    }

    /// Create a function from parsed JavaScript.
    pub(crate) fn new_user_function_value(
        &mut self,
        name: &str,
        length: usize,
        function: Rc<UserFunction>,
    ) -> Value {
        Value::Object(self.new_function_object(name, length, FunctionImpl::User(function)))
    }

    /// Whether a value can be called.
    pub(crate) fn is_callable(&self, value: &Value) -> bool {
        match value {
            Value::Object(id) => self
                .object(*id)
                .is_some_and(|object| matches!(&object.kind, ObjectKind::Function(_))),
            _ => false,
        }
    }

    /// Call any callable value.
    pub fn call_function(
        &mut self,
        callee: &Value,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value, JsError> {
        let Value::Object(id) = callee else {
            return Err(JsError::type_error(format!(
                "{} is not a function",
                callee.to_string_value()
            )));
        };
        let is_function = self
            .object(*id)
            .is_some_and(|object| matches!(&object.kind, ObjectKind::Function(_)));
        if !is_function {
            return Err(JsError::type_error(format!(
                "{} is not a function",
                callee.to_string_value()
            )));
        }

        // Keep the callee (and `this`) alive across nested allocations.
        let root_mark = self.temp_roots.len();
        self.temp_roots.push(callee.clone());
        self.temp_roots.push(this.clone());
        let result = self.call_function_object(*id, this, args);
        self.temp_roots.truncate(root_mark);

        match result {
            Ok(value) => Ok(value),
            Err(mut error) => {
                error.ensure_stack(&self.call_stack);
                Err(error)
            }
        }
    }

    fn call_function_object(
        &mut self,
        id: ObjectId,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value, JsError> {
        let implementation = {
            let Some(object) = self.object_mut(id) else {
                return Err(JsError::type_error("called a collected function"));
            };
            let ObjectKind::Function(function) = &mut object.kind else {
                return Err(JsError::type_error("value is not a function"));
            };
            std::mem::replace(&mut function.implementation, FunctionImpl::Unavailable)
        };
        match implementation {
            FunctionImpl::Builtin(f) => {
                let result = f(self, this, args);
                self.restore_function(id, FunctionImpl::Builtin(f));
                result
            }
            FunctionImpl::Native(mut f) => {
                let result = f(self, this, args);
                self.restore_function(id, FunctionImpl::Native(f));
                result
            }
            FunctionImpl::User(function) => {
                self.restore_function(id, FunctionImpl::User(Rc::clone(&function)));
                self.call_user_function(&function, this, args)
            }
            FunctionImpl::Unavailable => Err(JsError::type_error(
                "function is already executing and cannot be re-entered",
            )),
        }
    }

    /// Bind the parameter patterns of a call to the argument list, applying
    /// default values and collecting rest parameters.
    fn bind_parameters(
        &mut self,
        patterns: &[browser_js_parser::Pattern],
        args: &[Value],
    ) -> Result<(), JsError> {
        for (index, pattern) in patterns.iter().enumerate() {
            if let browser_js_parser::Pattern::Rest(target) = pattern {
                let rest = args.get(index..).map(<[Value]>::to_vec).unwrap_or_default();
                let mark = self.temp_roots.len();
                let array = self.new_array(rest);
                self.temp_roots.push(array.clone());
                let result = self.bind_param_pattern(target, array);
                self.temp_roots.truncate(mark);
                result?;
                return Ok(());
            }
            let value = args.get(index).cloned().unwrap_or(Value::Undefined);
            self.bind_param_pattern(pattern, value)?;
        }
        Ok(())
    }

    fn restore_function(&mut self, id: ObjectId, implementation: FunctionImpl) {
        if let Some(object) = self.object_mut(id)
            && let ObjectKind::Function(function) = &mut object.kind
        {
            function.implementation = implementation;
        }
    }

    fn call_user_function(
        &mut self,
        function: &UserFunction,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value, JsError> {
        if self.call_depth >= self.max_call_depth {
            return Err(JsError::range_error("maximum call stack size exceeded"));
        }
        self.call_depth += 1;
        let name = if function.name.is_empty() {
            "anonymous".to_owned()
        } else {
            function.name.clone()
        };
        self.call_stack.push(name);

        // Values in flight are only reachable through these roots until they are
        // installed in the call scope.
        let root_mark = self.temp_roots.len();
        self.temp_roots.push(this.clone());
        for arg in &args {
            self.temp_roots.push(arg.clone());
        }

        let scope = self.create_scope(Some(function.env), true);
        self.env_stack.push(scope);
        self.var_scope_stack.push(scope);

        if !function.is_arrow {
            // Sloppy mode: a plain call gets the global object as `this`.
            let this = if matches!(this, Value::Undefined) {
                Value::Object(self.global)
            } else {
                this
            };
            self.declare_binding(scope, "this", BindingKind::This, this, true);
        }
        let arguments = self.new_array(args.clone());
        self.declare_binding(scope, "arguments", BindingKind::Arguments, arguments, true);
        self.bind_parameters(&function.patterns, &args)?;
        self.hoist_vars(&function.body);
        self.hoist_functions(&function.body, scope);

        let completion = self.exec_stmts(&function.body).map_err(|mut error| {
            error.ensure_stack(&self.call_stack);
            error
        });

        self.var_scope_stack.pop();
        self.env_stack.pop();
        self.call_stack.pop();
        self.call_depth -= 1;
        self.temp_roots.truncate(root_mark);

        match completion {
            Ok(crate::execution::Completion::Normal) => Ok(Value::Undefined),
            Ok(crate::execution::Completion::Return(value)) => Ok(value),
            Ok(crate::execution::Completion::Break(_))
            | Ok(crate::execution::Completion::Continue(_)) => {
                Err(JsError::syntax_error("illegal break or continue statement"))
            }
            Err(error) => Err(error),
        }
    }
}
