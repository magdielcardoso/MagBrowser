//! Lexical environments used for variable bindings and closures.

use crate::errors::JsError;
use crate::value::Value;

/// Identifier of an environment record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ScopeId(pub usize);

/// How a binding came into existence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingKind {
    Var,
    Let,
    Const,
    Function,
    Param,
    This,
    Arguments,
}

/// A single variable binding.
#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub(crate) value: Value,
    pub(crate) kind: BindingKind,
    pub(crate) initialized: bool,
}

/// An environment record.
#[derive(Debug)]
pub(crate) struct Scope {
    pub(crate) parent: Option<ScopeId>,
    pub(crate) bindings: Vec<(String, Binding)>,
}

impl Binding {
    pub(crate) fn new(kind: BindingKind, value: Value, initialized: bool) -> Self {
        Self {
            value,
            kind,
            initialized,
        }
    }
}

impl crate::Runtime {
    pub(crate) fn create_scope(&mut self, parent: Option<ScopeId>, _is_function: bool) -> ScopeId {
        let scope = Scope {
            parent,
            bindings: Vec::new(),
        };
        if let Some(index) = self.scope_free.pop() {
            self.scopes[index] = Some(scope);
            ScopeId(index)
        } else {
            self.scopes.push(Some(scope));
            ScopeId(self.scopes.len() - 1)
        }
    }

    pub(crate) fn scope(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.0).and_then(Option::as_ref)
    }

    pub(crate) fn scope_mut(&mut self, id: ScopeId) -> Option<&mut Scope> {
        self.scopes.get_mut(id.0).and_then(Option::as_mut)
    }

    pub(crate) fn current_scope(&self) -> ScopeId {
        *self
            .env_stack
            .last()
            .expect("the interpreter always has an environment")
    }

    pub(crate) fn var_scope(&self) -> ScopeId {
        *self
            .var_scope_stack
            .last()
            .expect("the interpreter always has a var scope")
    }

    /// Walk the environment chain of the current scope looking for `name`.
    pub(crate) fn find_binding_scope(&self, name: &str) -> Option<ScopeId> {
        let mut current = self.env_stack.last().copied();
        while let Some(scope_id) = current {
            let scope = self.scope(scope_id)?;
            if scope.bindings.iter().any(|(bound, _)| bound == name) {
                return Some(scope_id);
            }
            current = scope.parent;
        }
        None
    }

    /// Find the nearest scope that binds `name`.
    pub(crate) fn resolve_scope(&self, name: &str) -> Option<ScopeId> {
        self.find_binding_scope(name)
    }

    /// Read a binding, honouring the temporal dead zone.
    pub(crate) fn read_binding(&mut self, name: &str) -> Result<Value, JsError> {
        let Some(scope_id) = self.find_binding_scope(name) else {
            // Properties of the global object are globals too, including
            // accessor properties installed by the embedding host.
            let global = Value::Object(self.global);
            if let Some(value) = self.lookup_property(&global, self.global, name)? {
                return Ok(value);
            }
            return Err(JsError::reference_error(format!("{name} is not defined")));
        };
        if let Some(scope) = self.scope(scope_id)
            && let Some((_, binding)) = scope.bindings.iter().find(|(bound, _)| bound == name)
        {
            if !binding.initialized {
                return Err(JsError::reference_error(format!(
                    "cannot access '{name}' before initialization"
                )));
            }
            return Ok(binding.value.clone());
        }
        Err(JsError::reference_error(format!("{name} is not defined")))
    }

    /// Declare a binding in `scope`.
    pub(crate) fn declare_binding(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: BindingKind,
        value: Value,
        initialized: bool,
    ) {
        let is_global = scope == self.global_scope;
        let mirrored = if is_global && initialized {
            Some(value.clone())
        } else {
            None
        };
        if let Some(target) = self.scope_mut(scope) {
            if let Some((_, binding)) = target.bindings.iter_mut().find(|(bound, _)| bound == name)
            {
                // `var` and function declarations may redeclare without losing data.
                if matches!(binding.kind, BindingKind::Var | BindingKind::Function)
                    && matches!(kind, BindingKind::Var | BindingKind::Function)
                    && initialized
                {
                    binding.value = value;
                    binding.kind = kind;
                    binding.initialized = true;
                } else if initialized && !binding.initialized {
                    binding.value = value;
                    binding.initialized = true;
                }
            } else {
                target
                    .bindings
                    .push((name.to_owned(), Binding::new(kind, value, initialized)));
            }
        }
        if let Some(value) = mirrored {
            let global = self.global;
            self.define_data_property(global, name, value);
        }
    }

    /// Assign to an existing binding; returns `false` when the name is unbound.
    pub(crate) fn assign_binding(&mut self, name: &str, value: Value) -> Result<bool, JsError> {
        let Some(scope_id) = self.find_binding_scope(name) else {
            if self.has_own_property(self.global, name) {
                let global = Value::Object(self.global);
                self.set_property_checked(&global, name, value)?;
                return Ok(true);
            }
            return Ok(false);
        };
        let is_global = scope_id == self.global_scope;
        if let Some(scope) = self.scope_mut(scope_id)
            && let Some((_, binding)) = scope.bindings.iter_mut().find(|(bound, _)| bound == name)
        {
            if binding.kind == BindingKind::Const && binding.initialized {
                return Err(JsError::type_error(format!(
                    "assignment to constant variable '{name}'"
                )));
            }
            binding.value = value.clone();
            binding.initialized = true;
        }
        if is_global {
            let global = self.global;
            self.define_data_property(global, name, value);
        }
        Ok(true)
    }

    /// Initialise an existing uninitialised binding (`let`/`const` declaration execution).
    pub(crate) fn initialize_binding(
        &mut self,
        scope: ScopeId,
        name: &str,
        value: Value,
    ) -> Result<(), JsError> {
        let is_global = scope == self.global_scope;
        if let Some(target) = self.scope_mut(scope)
            && let Some((_, binding)) = target.bindings.iter_mut().find(|(bound, _)| bound == name)
        {
            if binding.kind == BindingKind::Const && binding.initialized {
                return Err(JsError::type_error(format!(
                    "assignment to constant variable '{name}'"
                )));
            }
            binding.value = value.clone();
            binding.initialized = true;
        }
        if is_global {
            let global = self.global;
            self.define_data_property(global, name, value);
        }
        Ok(())
    }

    /// Declare a `var` binding in the current function scope.
    pub(crate) fn declare_var(&mut self, name: &str, value: Value) {
        let scope = self.var_scope();
        self.declare_binding(scope, name, BindingKind::Var, value, true);
    }

    /// Create a global binding, used by the embedding host.
    pub(crate) fn declare_global(&mut self, name: &str, value: Value) {
        let global_scope = self.global_scope;
        self.declare_binding(global_scope, name, BindingKind::Var, value, true);
    }
}

#[cfg(test)]
mod tests {
    use crate::Runtime;

    #[test]
    fn block_scopes_shadow_and_restore() {
        let mut runtime = Runtime::new();
        runtime
            .eval("var seen = []; { let x = 1; seen.push(x); } { let x = 2; seen.push(x); }")
            .expect("eval");
        assert_eq!(runtime.eval("seen.join(',')").expect("eval"), "1,2".into());
        assert!(
            runtime.eval("x").is_err(),
            "block scoped binding must not leak"
        );
    }

    #[test]
    fn const_cannot_be_reassigned() {
        let mut runtime = Runtime::new();
        let error = runtime
            .eval("const c = 1; c = 2;")
            .expect_err("should fail");
        assert!(error.message.contains("constant"));
    }

    #[test]
    fn let_temporal_dead_zone_errors() {
        let mut runtime = Runtime::new();
        let error = runtime.eval("{ x; let x = 1; }").expect_err("should fail");
        assert!(error.message.contains("before initialization"));
    }

    #[test]
    fn var_is_function_scoped() {
        let mut runtime = Runtime::new();
        runtime
            .eval("function f() { if (true) { var inner = 5; } return inner; }")
            .expect("eval");
        assert_eq!(runtime.eval("f()").expect("eval"), 5.0.into());
    }
}
