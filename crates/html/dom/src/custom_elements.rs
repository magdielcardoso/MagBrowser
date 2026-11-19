//! The custom element registry.
//!
//! This is a JS-agnostic, Rust-side mirror of the browser's `customElements`
//! object. It does not store JavaScript values: instead it hands out opaque
//! [`CallbackHandle`]s that the JS host maps to its own function objects. The
//! host calls [`CustomElementRegistry::allocate_handle`] (or supplies its own
//! ids), registers the mapping, and looks the handle back up through
//! [`CustomElementRegistry::get`] when the DOM needs to construct or upgrade an
//! element.
//!
//! Lifecycle callback slots ([`LifecycleCallbacks`]) and the callbacks queued
//! by `whenDefined` ([`WhenDefinedCallback`]) follow the same pattern: the
//! host receives a handle it can invoke, or a Rust closure it can use to
//! resolve a JS promise.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Opaque handle for a host-side callback (usually a JS function object).
///
/// The DOM never interprets this value; it is only stored and handed back. A
/// value of `0` is reserved to mean "no callback".
pub type CallbackHandle = u64;

/// Names reserved by the HTML specification; custom elements may not use them.
pub const RESERVED_CUSTOM_ELEMENT_NAMES: [&str; 8] = [
    "annotation-xml",
    "color-profile",
    "font-face",
    "font-face-src",
    "font-face-uri",
    "font-face-format",
    "font-face-name",
    "missing-glyph",
];

/// Lifecycle callbacks of a custom element definition.
///
/// Every field is an opaque handle into the JS host. `None` means the class
/// does not define that callback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct LifecycleCallbacks {
    /// Called when the element is connected to a document.
    pub connected: Option<CallbackHandle>,
    /// Called when the element is disconnected from a document.
    pub disconnected: Option<CallbackHandle>,
    /// Called when the element is moved into a new document.
    pub adopted: Option<CallbackHandle>,
    /// Called when one of the observed attributes changes.
    pub attribute_changed: Option<CallbackHandle>,
}

/// A registered custom element definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomElementDefinition {
    /// The registered (lowercased) custom element name.
    pub name: String,
    /// Opaque handle for the constructor function owned by the JS host.
    pub constructor: CallbackHandle,
    /// Attributes that trigger `attributeChangedCallback` when changed.
    pub observed_attributes: Vec<String>,
    /// Lifecycle callback handles.
    pub callbacks: LifecycleCallbacks,
}

/// Callback registered through [`CustomElementRegistry::when_defined`].
///
/// It receives the definition once it becomes available. Use a closure that
/// resolves the corresponding JS promise.
pub type WhenDefinedCallback = Arc<dyn Fn(&CustomElementDefinition) + Send + Sync>;

/// Why a registry operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomElementError {
    /// The supplied name is not a valid custom element name.
    InvalidName {
        /// The offending name.
        name: String,
        /// Short explanation of the rule that was violated.
        reason: &'static str,
    },
    /// A definition with this name is already registered.
    AlreadyDefined(String),
}

impl fmt::Display for CustomElementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(formatter, "invalid custom element name '{name}': {reason}")
            }
            Self::AlreadyDefined(name) => {
                write!(formatter, "custom element '{name}' is already defined")
            }
        }
    }
}

impl std::error::Error for CustomElementError {}

/// Per-document custom element registry.
#[derive(Clone, Default)]
pub struct CustomElementRegistry {
    definitions: HashMap<String, CustomElementDefinition>,
    pending: HashMap<String, Vec<WhenDefinedCallback>>,
    next_handle: CallbackHandle,
}

impl fmt::Debug for CustomElementRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CustomElementRegistry")
            .field("definitions", &self.definitions)
            .field("pending", &self.pending.len())
            .field("next_handle", &self.next_handle)
            .finish()
    }
}

impl CustomElementRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a fresh opaque callback handle.
    ///
    /// The JS host calls this (or uses its own ids) before registering a
    /// constructor or lifecycle callback so it can keep a handle-to-function
    /// map. Handles are never reused.
    pub fn allocate_handle(&mut self) -> CallbackHandle {
        self.next_handle = self.next_handle.saturating_add(1);
        self.next_handle
    }

    /// Registers a custom element with a constructor and no callbacks.
    ///
    /// Maps to `customElements.define(name, constructor)`. The name is
    /// validated with [`CustomElementRegistry::validate_name`].
    ///
    /// # Errors
    /// Returns [`CustomElementError::InvalidName`] for a bad name and
    /// [`CustomElementError::AlreadyDefined`] when `name` is registered.
    pub fn define(
        &mut self,
        name: &str,
        constructor: CallbackHandle,
    ) -> Result<&CustomElementDefinition, CustomElementError> {
        self.define_with(name, constructor, Vec::new(), LifecycleCallbacks::default())
    }

    /// Registers a custom element with observed attributes and lifecycle
    /// callbacks.
    ///
    /// # Errors
    /// Same as [`CustomElementRegistry::define`].
    pub fn define_with(
        &mut self,
        name: &str,
        constructor: CallbackHandle,
        observed_attributes: Vec<String>,
        callbacks: LifecycleCallbacks,
    ) -> Result<&CustomElementDefinition, CustomElementError> {
        Self::validate_name(name)?;
        if self.definitions.contains_key(name) {
            return Err(CustomElementError::AlreadyDefined(name.to_string()));
        }
        let definition = CustomElementDefinition {
            name: name.to_string(),
            constructor,
            observed_attributes,
            callbacks,
        };
        self.definitions.insert(name.to_string(), definition);

        // Resolve every `whenDefined` callback registered before this point.
        if let Some(waiters) = self.pending.remove(name) {
            let registered = &self.definitions[name];
            for callback in &waiters {
                callback(registered);
            }
        }
        Ok(&self.definitions[name])
    }

    /// Looks up a registered definition. Maps to `customElements.get(name)`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CustomElementDefinition> {
        self.definitions.get(name)
    }

    /// True when `name` has been registered.
    #[must_use]
    pub fn is_defined(&self, name: &str) -> bool {
        self.definitions.contains_key(name)
    }

    /// Number of registered definitions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// True when no definitions are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Iterates over registered names in unspecified order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.definitions.keys().map(String::as_str)
    }

    /// Registers a callback to run once `name` is defined.
    ///
    /// Maps to `customElements.whenDefined(name)`. Returns `Ok(true)` and
    /// invokes `callback` immediately when the element is already defined;
    /// `Ok(false)` queues it for a later [`CustomElementRegistry::define`].
    ///
    /// # Errors
    /// Returns [`CustomElementError::InvalidName`] for a bad name.
    pub fn when_defined<F>(&mut self, name: &str, callback: F) -> Result<bool, CustomElementError>
    where
        F: Fn(&CustomElementDefinition) + Send + Sync + 'static,
    {
        Self::validate_name(name)?;
        if let Some(definition) = self.definitions.get(name) {
            callback(definition);
            return Ok(true);
        }
        self.pending
            .entry(name.to_string())
            .or_default()
            .push(Arc::new(callback));
        Ok(false)
    }

    /// Number of callbacks currently waiting for `name`, for diagnostics.
    #[must_use]
    pub fn pending_count(&self, name: &str) -> usize {
        self.pending.get(name).map_or(0, Vec::len)
    }

    /// Validates a custom element name.
    ///
    /// A valid name contains a hyphen, starts with a lowercase ASCII letter or
    /// a non-ASCII code point, uses only lowercase ASCII letters, digits,
    /// `-`, `.`, `_` or non-ASCII code points, and is not one of the
    /// [reserved names](RESERVED_CUSTOM_ELEMENT_NAMES).
    #[must_use]
    pub fn is_valid_name(name: &str) -> bool {
        Self::validate_name(name).is_ok()
    }

    /// Returns the reason a name is invalid, or `Ok(())`.
    ///
    /// # Errors
    /// Returns [`CustomElementError::InvalidName`] describing the first broken
    /// rule.
    pub fn validate_name(name: &str) -> Result<(), CustomElementError> {
        let invalid = |reason: &'static str| {
            Err(CustomElementError::InvalidName {
                name: name.to_string(),
                reason,
            })
        };
        let Some(first) = name.chars().next() else {
            return invalid("name must not be empty");
        };
        if !(first.is_ascii_lowercase() || !first.is_ascii()) {
            return invalid("name must start with a lowercase letter");
        }
        if !name.contains('-') {
            return invalid("name must contain a hyphen");
        }
        if RESERVED_CUSTOM_ELEMENT_NAMES.contains(&name) {
            return invalid("name is reserved by the HTML specification");
        }
        if name.chars().any(|character| {
            !(character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '.' | '_')
                || !character.is_ascii())
        }) {
            return invalid("name contains a disallowed character");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn validates_names() {
        for name in ["x-foo", "my-element", "a-b", "x-1.2_3", "é-x"] {
            assert!(CustomElementRegistry::is_valid_name(name), "{name}");
        }
        for name in [
            "",
            "div",
            "X-Foo",
            "x-Foo",
            "1-x",
            "-x-",
            ".x-",
            "_x-",
            "x foo",
            "x@foo",
            "annotation-xml",
            "color-profile",
            "font-face",
            "font-face-src",
            "font-face-uri",
            "font-face-format",
            "font-face-name",
            "missing-glyph",
        ] {
            assert!(!CustomElementRegistry::is_valid_name(name), "{name}");
        }
    }

    #[test]
    fn define_get_and_reject_duplicates() {
        let mut registry = CustomElementRegistry::new();
        let constructor = registry.allocate_handle();
        assert_eq!(constructor, 1);
        assert!(registry.define("x-card", constructor).is_ok());
        assert_eq!(registry.get("x-card").unwrap().constructor, constructor);
        assert_eq!(registry.get("x-card").unwrap().name, "x-card");
        assert!(registry.is_defined("x-card"));
        assert!(registry.get("x-other").is_none());
        assert_eq!(
            registry.define("x-card", constructor),
            Err(CustomElementError::AlreadyDefined("x-card".into()))
        );
        assert_eq!(
            registry.define("plain", constructor),
            Err(CustomElementError::InvalidName {
                name: "plain".into(),
                reason: "name must contain a hyphen"
            })
        );
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.names().collect::<Vec<_>>(), vec!["x-card"]);
    }

    #[test]
    fn define_with_stores_callbacks_and_observed_attributes() {
        let mut registry = CustomElementRegistry::new();
        let callbacks = LifecycleCallbacks {
            connected: Some(10),
            disconnected: Some(11),
            attribute_changed: Some(12),
            adopted: None,
        };
        registry
            .define_with(
                "x-input",
                7,
                vec!["value".into(), "disabled".into()],
                callbacks,
            )
            .unwrap();
        let definition = registry.get("x-input").unwrap();
        assert_eq!(definition.constructor, 7);
        assert_eq!(definition.observed_attributes, ["value", "disabled"]);
        assert_eq!(definition.callbacks.connected, Some(10));
        assert_eq!(definition.callbacks.adopted, None);
    }

    #[test]
    fn when_defined_invokes_immediately_or_when_registered() {
        let mut registry = CustomElementRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let immediate_calls = Arc::clone(&calls);
        registry.define("x-ready", 1).unwrap();
        assert!(
            registry
                .when_defined("x-ready", move |_| {
                    immediate_calls.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let queued_calls = Arc::clone(&calls);
        assert!(
            !registry
                .when_defined("x-later", move |definition| {
                    assert_eq!(definition.name, "x-later");
                    queued_calls.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap()
        );
        assert_eq!(registry.pending_count("x-later"), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        registry.define("x-later", 2).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(registry.pending_count("x-later"), 0);

        assert!(registry.when_defined("notvalid", |_| {}).is_err());
    }
}
