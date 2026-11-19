//! ES module registry, linking and evaluation.
//!
//! Modules are keyed by their (resolved) URL. A module is parsed and registered
//! once, linked against its static dependencies once, and executed once. Export
//! accessors and imported bindings read through to the exporting module's
//! environment, so bindings stay live.

use browser_js_parser::{ExportKind, Pattern, Program, Stmt};

use crate::errors::JsError;
use crate::object::ObjectKind;
use crate::scope::{BindingKind, ScopeId};
use crate::value::{ObjectId, Value};

/// Host callback used to resolve and fetch a module's source text.
///
/// The first argument is the specifier as written in `import`/`export ... from`
/// (or as resolved relative to the referring module). The second argument is
/// the URL of the referring module, or `None` for a top level
/// `import_module`/`import()` call. The callback returns the resolved absolute
/// URL and the source text. Use [`crate::Runtime::set_module_loader`] to
/// install it.
pub type ModuleLoader = Box<dyn FnMut(&str, Option<&str>) -> Result<(String, String), JsError>>;

/// Lifecycle state of a registered module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModuleStatus {
    Uninstantiated,
    Instantiating,
    Instantiated,
    Evaluating,
    Evaluated,
    Errored,
}

/// Where the value of an exported name comes from.
#[derive(Clone, Debug)]
pub(crate) enum ExportBinding {
    /// A binding in the exporting module's own scope.
    Local(String),
    /// A re-export from another module: `(module URL, exported name)`.
    Indirect(String, String),
}

/// Everything the runtime needs to link and execute one module.
pub(crate) struct ModuleRecord {
    pub(crate) url: String,
    pub(crate) program: Program,
    pub(crate) status: ModuleStatus,
    pub(crate) scope: ScopeId,
    pub(crate) namespace: ObjectId,
    pub(crate) exports: Vec<(String, ExportBinding)>,
    pub(crate) star_exports: Vec<String>,
    pub(crate) imports: Vec<(String, String, String)>,
    pub(crate) dependencies: Vec<String>,
    pub(crate) error: Option<String>,
}

/// Split a URL into its origin prefix (including a trailing slash when
/// present) and the path below it.
fn split_url(url: &str) -> (&str, &str) {
    if let Some(position) = url.find("://") {
        let host_start = position + 3;
        if let Some(slash) = url[host_start..].find('/') {
            (
                &url[..host_start + slash + 1],
                &url[host_start + slash + 1..],
            )
        } else {
            (url, "")
        }
    } else if let Some(path) = url.strip_prefix('/') {
        ("/", path)
    } else {
        ("", url)
    }
}

/// Normalise `.` and `..` segments of a path.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

/// Resolve a module specifier against a referring module URL. Bare specifiers
/// are left untouched so a host loader can map them; relative and absolute
/// paths are joined and normalised here.
pub(crate) fn resolve_specifier(specifier: &str, referrer: Option<&str>) -> String {
    let Some(base) = referrer else {
        return specifier.to_owned();
    };
    if !specifier.starts_with('.') && !specifier.starts_with('/') {
        return specifier.to_owned();
    }
    if let Some(absolute) = specifier.strip_prefix('/') {
        let (prefix, _) = split_url(base);
        return format!("{prefix}{}", normalize_path(absolute));
    }
    let base_dir = base.rsplit_once('/').map_or("", |(directory, _)| directory);
    let combined = if base_dir.is_empty() {
        specifier.to_owned()
    } else {
        format!("{base_dir}/{specifier}")
    };
    let (prefix, path) = split_url(&combined);
    format!("{prefix}{}", normalize_path(path))
}

/// Bound names of a declaration pattern, used to collect `export` names.
fn pattern_names(pattern: &Pattern, out: &mut Vec<String>) {
    pattern.bound_names(out);
}

/// The name a default declaration binds, falling back to `default` for
/// anonymous functions and classes (the parser names those `default`).
fn declared_name(stmt: &Stmt) -> Option<String> {
    match stmt {
        Stmt::FunctionDecl(function) => function.name.clone(),
        Stmt::ClassDecl(class) => class.name.clone(),
        _ => None,
    }
}

impl crate::Runtime {
    /// Parse and register a module under `url`, returning its namespace object.
    ///
    /// Static dependencies that are not already registered are obtained from
    /// the host loader installed with [`Self::set_module_loader`]. Each module
    /// is linked and executed at most once, even when reached through a cycle.
    pub fn evaluate_module(&mut self, url: &str, source: &str) -> Result<Value, JsError> {
        // One shared budget for the whole graph, so cyclic dependency chains
        // cannot reset the limit by re-entering this method.
        self.steps = 0;
        let id = self.register_module(url, source)?;
        self.instantiate_module(id)?;
        self.evaluate_module_record(id)?;
        Ok(self.module_namespace(id))
    }

    /// Register and evaluate a module, discarding its namespace object.
    pub fn load_module(&mut self, url: &str, source: &str) -> Result<Value, JsError> {
        self.evaluate_module(url, source)
    }

    /// Install the callback used to resolve and fetch unknown module sources.
    pub fn set_module_loader(&mut self, loader: ModuleLoader) {
        self.module_loader = Some(loader);
    }

    /// Import a module and return a promise for its namespace object.
    ///
    /// The promise is fulfilled synchronously when the module graph can be
    /// resolved; otherwise it is rejected with the resolution error. Call
    /// [`crate::Runtime::run_microtasks`] to run `.then` handlers.
    pub fn import_module(&mut self, specifier: &str) -> Result<Value, JsError> {
        let referrer = self.current_module.map(|id| self.module(id).url.clone());
        let key = resolve_specifier(specifier, referrer.as_deref());
        let outcome = (|| -> Result<Value, JsError> {
            let id = self.ensure_module(&key, specifier, referrer.as_deref())?;
            self.instantiate_module(id)?;
            self.evaluate_module_record(id)?;
            Ok(self.module_namespace(id))
        })();
        let (promise, _state) = self.new_promise();
        match outcome {
            Ok(namespace) => self.resolve_promise(promise, namespace),
            Err(error) => self.reject_promise(promise, error.thrown_value()),
        }
        Ok(Value::Object(promise))
    }

    /// Access a registered module by index.
    pub(crate) fn module(&self, id: usize) -> &ModuleRecord {
        &self.modules[id]
    }

    /// Mutable access to a registered module by index (used by the linker and
    /// evaluation state transitions below).
    pub(crate) fn module_mut(&mut self, id: usize) -> &mut ModuleRecord {
        &mut self.modules[id]
    }

    /// The namespace object of a registered module.
    pub(crate) fn module_namespace(&self, id: usize) -> Value {
        Value::Object(self.module(id).namespace)
    }

    /// Look up a registered module by its resolved URL.
    pub(crate) fn module_id(&self, url: &str) -> Option<usize> {
        self.module_urls.get(url).copied()
    }

    /// Fetch source through the host loader, restoring the callback even when
    /// it returns an error.
    fn fetch_module_source(
        &mut self,
        specifier: &str,
        referrer: Option<&str>,
    ) -> Result<(String, String), JsError> {
        let mut loader = self.module_loader.take();
        let result = match loader.as_mut() {
            Some(loader) => loader(specifier, referrer),
            None => Err(JsError::new(format!(
                "no module loader configured: cannot load module '{specifier}'"
            ))),
        };
        self.module_loader = loader;
        result
    }

    /// Return the id of a module, fetching and registering it if necessary.
    fn ensure_module(
        &mut self,
        key: &str,
        specifier: &str,
        referrer: Option<&str>,
    ) -> Result<usize, JsError> {
        if let Some(id) = self.module_id(key) {
            return Ok(id);
        }
        let (resolved, source) = self.fetch_module_source(specifier, referrer)?;
        if let Some(id) = self.module_id(&resolved) {
            return Ok(id);
        }
        self.register_module(&resolved, &source)
    }

    /// Parse `source` and create an uninstantiated module record.
    fn register_module(&mut self, url: &str, source: &str) -> Result<usize, JsError> {
        if let Some(id) = self.module_id(url) {
            return Ok(id);
        }
        let program = browser_js_parser::parse(source)
            .map_err(|error| JsError::syntax_error(error.to_string()))?;

        let mut dependencies: Vec<String> = Vec::new();
        let mut imports: Vec<(String, String, String)> = Vec::new();
        let mut exports: Vec<(String, ExportBinding)> = Vec::new();
        let mut star_exports: Vec<String> = Vec::new();

        let add_dependency = |dependencies: &mut Vec<String>, url: String| {
            if !dependencies.contains(&url) {
                dependencies.push(url);
            }
        };

        for stmt in &program.body {
            match stmt {
                Stmt::Import(declaration) => {
                    let resolved = resolve_specifier(&declaration.source, Some(url));
                    add_dependency(&mut dependencies, resolved.clone());
                    if let Some(local) = &declaration.default {
                        imports.push((local.clone(), resolved.clone(), "default".to_owned()));
                    }
                    if let Some(local) = &declaration.namespace {
                        imports.push((local.clone(), resolved.clone(), "*".to_owned()));
                    }
                    for specifier in &declaration.named {
                        imports.push((
                            specifier.local.clone(),
                            resolved.clone(),
                            specifier.imported.clone(),
                        ));
                    }
                }
                Stmt::Export(export) => match &export.kind {
                    ExportKind::Named {
                        specifiers,
                        source: Some(from),
                    } => {
                        let resolved = resolve_specifier(from, Some(url));
                        add_dependency(&mut dependencies, resolved.clone());
                        for specifier in specifiers {
                            exports.push((
                                specifier.exported.clone(),
                                ExportBinding::Indirect(resolved.clone(), specifier.local.clone()),
                            ));
                        }
                    }
                    ExportKind::Named {
                        specifiers,
                        source: None,
                    } => {
                        for specifier in specifiers {
                            exports.push((
                                specifier.exported.clone(),
                                ExportBinding::Local(specifier.local.clone()),
                            ));
                        }
                    }
                    ExportKind::All { source: from } => {
                        let resolved = resolve_specifier(from, Some(url));
                        add_dependency(&mut dependencies, resolved.clone());
                        star_exports.push(resolved);
                    }
                    ExportKind::DefaultExpr(_) => {
                        exports.push((
                            "default".to_owned(),
                            ExportBinding::Local("default".to_owned()),
                        ));
                    }
                    ExportKind::DefaultDecl(inner) => {
                        let name = declared_name(inner).unwrap_or_else(|| "default".to_owned());
                        exports.push(("default".to_owned(), ExportBinding::Local(name)));
                    }
                    ExportKind::Decl(inner) => match inner.as_ref() {
                        Stmt::VarDecl(declaration) => {
                            for declarator in &declaration.declarations {
                                let mut names = Vec::new();
                                pattern_names(&declarator.pattern, &mut names);
                                for name in names {
                                    exports.push((name.clone(), ExportBinding::Local(name)));
                                }
                            }
                        }
                        other => {
                            if let Some(name) = declared_name(other) {
                                exports.push((name.clone(), ExportBinding::Local(name)));
                            }
                        }
                    },
                },
                _ => {}
            }
        }

        let scope = self.create_scope(Some(self.global_scope), false);
        self.gc_scope_roots.push(scope);
        let namespace = self.alloc_object(ObjectKind::Ordinary, Some(self.prototypes.object));
        self.gc_roots.push(Value::Object(namespace));

        let id = self.modules.len();
        self.modules.push(ModuleRecord {
            url: url.to_owned(),
            program,
            status: ModuleStatus::Uninstantiated,
            scope,
            namespace,
            exports,
            star_exports,
            imports,
            dependencies,
            error: None,
        });
        self.module_urls.insert(url.to_owned(), id);
        Ok(id)
    }

    /// Link a module to its dependencies. Cycles are handled by marking a
    /// module as instantiating before descending into its dependencies.
    fn instantiate_module(&mut self, id: usize) -> Result<(), JsError> {
        match self.module(id).status {
            ModuleStatus::Instantiating
            | ModuleStatus::Instantiated
            | ModuleStatus::Evaluating
            | ModuleStatus::Evaluated => return Ok(()),
            ModuleStatus::Errored => {
                return Err(JsError::new(self.module(id).error.clone().unwrap_or_else(
                    || format!("module '{}' failed", self.module(id).url),
                )));
            }
            ModuleStatus::Uninstantiated => {}
        }
        self.module_mut(id).status = ModuleStatus::Instantiating;
        let result = (|| -> Result<(), JsError> {
            let url = self.module(id).url.clone();
            let dependencies = self.module(id).dependencies.clone();
            for dependency in dependencies {
                let dep_id = self.ensure_module(&dependency, &dependency, Some(&url))?;
                self.instantiate_module(dep_id)?;
            }

            let (scope, exports, star_exports, imports, program) = {
                let record = self.module(id);
                (
                    record.scope,
                    record.exports.clone(),
                    record.star_exports.clone(),
                    record.imports.clone(),
                    record.program.clone(),
                )
            };
            let namespace = self.module(id).namespace;

            // Local export names become live accessor properties on the namespace.
            for (exported, _binding) in &exports {
                let name = exported.clone();
                let getter = {
                    let module_id = id;
                    self.new_native_value(
                        &format!("get {name}"),
                        0,
                        Box::new(move |runtime, _this, _args| {
                            let mut visiting = Vec::new();
                            Ok(runtime
                                .resolve_module_export(module_id, &name, &mut visiting)?
                                .unwrap_or(Value::Undefined))
                        }),
                    )
                };
                self.define_accessor_property(namespace, exported, Some(getter), None);
            }

            // `export * from` forwards every name the dependency exposes.
            for dependency in &star_exports {
                let Some(dep_id) = self.module_id(dependency) else {
                    continue;
                };
                let dep_namespace = self.module_namespace(dep_id);
                let names = self.object_keys(&dep_namespace);
                for name in names {
                    if self
                        .module(id)
                        .exports
                        .iter()
                        .any(|(exported, _)| exported == &name)
                    {
                        continue;
                    }
                    let getter = {
                        let module_id = id;
                        let export_name = name.clone();
                        self.new_native_value(
                            &format!("get {name}"),
                            0,
                            Box::new(move |runtime, _this, _args| {
                                let mut visiting = Vec::new();
                                Ok(runtime
                                    .resolve_module_export(module_id, &export_name, &mut visiting)?
                                    .unwrap_or(Value::Undefined))
                            }),
                        )
                    };
                    self.define_accessor_property(namespace, &name, Some(getter), None);
                }
            }

            // Imported bindings are backed by getters reading the exporting
            // module's export, so they observe later writes to that binding.
            for (local, dependency, imported) in imports {
                if imported == "*" {
                    if let Some(dep_id) = self.module_id(&dependency) {
                        let value = self.module_namespace(dep_id);
                        self.declare_binding(scope, &local, BindingKind::Let, value, true);
                    }
                    continue;
                }
                let Some(dep_id) = self.module_id(&dependency) else {
                    continue;
                };
                let getter = self.new_native_value(
                    &format!("get import {local}"),
                    0,
                    Box::new(move |runtime, _this, _args| {
                        let mut visiting = Vec::new();
                        Ok(runtime
                            .resolve_module_export(dep_id, &imported, &mut visiting)?
                            .unwrap_or(Value::Undefined))
                    }),
                );
                self.declare_binding(scope, &local, BindingKind::Import, getter, true);
            }

            // Hoist declarations before evaluating dependencies so cycles can
            // call exported functions while a module's body is still running.
            self.env_stack.push(scope);
            self.var_scope_stack.push(scope);
            self.prepare_lexical(&program.body, scope);
            self.hoist_vars(&program.body);
            self.hoist_functions(&program.body, scope);
            self.env_stack.pop();
            self.var_scope_stack.pop();

            Ok(())
        })();
        match result {
            Ok(()) => {
                self.module_mut(id).status = ModuleStatus::Instantiated;
                Ok(())
            }
            Err(error) => {
                self.module_mut(id).status = ModuleStatus::Errored;
                self.module_mut(id).error = Some(error.message.clone());
                Err(error)
            }
        }
    }

    /// Execute a module and its static dependencies in dependency order.
    fn evaluate_module_record(&mut self, id: usize) -> Result<(), JsError> {
        match self.module(id).status {
            ModuleStatus::Evaluated => return Ok(()),
            // A dependency that is already executing is part of a cycle; its
            // body will finish on the outer call.
            ModuleStatus::Evaluating => return Ok(()),
            ModuleStatus::Errored => {
                return Err(JsError::new(self.module(id).error.clone().unwrap_or_else(
                    || format!("module '{}' failed", self.module(id).url),
                )));
            }
            _ => {}
        }
        self.instantiate_module(id)?;
        self.module_mut(id).status = ModuleStatus::Evaluating;

        let dependencies = self.module(id).dependencies.clone();
        let mut result = Ok(());
        for dependency in dependencies {
            if let Some(dep_id) = self.module_id(&dependency) {
                result = self.evaluate_module_record(dep_id);
                if result.is_err() {
                    break;
                }
            }
        }
        if result.is_ok() {
            result = self.exec_module_body(id).map(|_| ());
        }

        match result {
            Ok(()) => {
                self.module_mut(id).status = ModuleStatus::Evaluated;
                Ok(())
            }
            Err(error) => {
                self.module_mut(id).status = ModuleStatus::Errored;
                self.module_mut(id).error = Some(error.message.clone());
                Err(error)
            }
        }
    }

    /// Resolve an exported name, following re-exports and `export *`.
    pub(crate) fn resolve_module_export(
        &mut self,
        id: usize,
        name: &str,
        visiting: &mut Vec<usize>,
    ) -> Result<Option<Value>, JsError> {
        if visiting.contains(&id) {
            return Ok(None);
        }
        visiting.push(id);
        let result = (|| -> Result<Option<Value>, JsError> {
            let (scope, exports, stars) = {
                let record = self.module(id);
                (
                    record.scope,
                    record
                        .exports
                        .iter()
                        .filter(|(exported, _)| exported == name)
                        .cloned()
                        .collect::<Vec<_>>(),
                    record.star_exports.clone(),
                )
            };
            for (_, binding) in exports {
                match binding {
                    ExportBinding::Local(local) => {
                        return Ok(Some(self.read_binding_from_scope(scope, &local)?));
                    }
                    ExportBinding::Indirect(url, exported) => {
                        if let Some(dep_id) = self.module_id(&url)
                            && let Some(value) =
                                self.resolve_module_export(dep_id, &exported, visiting)?
                        {
                            return Ok(Some(value));
                        }
                    }
                }
            }
            for url in stars {
                if let Some(dep_id) = self.module_id(&url)
                    && let Some(value) = self.resolve_module_export(dep_id, name, visiting)?
                {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        })();
        visiting.pop();
        result
    }

    /// The scope holding a module's bindings, when executing a module.
    pub(crate) fn current_module_scope(&self) -> Option<ScopeId> {
        self.current_module.map(|id| self.module(id).scope)
    }

    /// The URL of the module currently executing.
    pub(crate) fn current_module_url(&self) -> Option<&str> {
        self.current_module.map(|id| self.module(id).url.as_str())
    }

    /// Validate that a dynamic import specifier is a string.
    pub(crate) fn import_specifier(value: &Value) -> Result<String, JsError> {
        match value {
            Value::String(specifier) => Ok(specifier.clone()),
            other => Err(JsError::type_error(format!(
                "import() specifier must be a string, got {}",
                other.to_string_value()
            ))),
        }
    }

    /// Whether a module with this URL has been registered.
    pub fn has_module(&self, url: &str) -> bool {
        self.module_id(url).is_some()
    }
}
