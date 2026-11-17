//! JavaScript error values.

use std::fmt;

use crate::value::Value;

/// An error thrown by JavaScript code or raised by the runtime.
#[derive(Clone, Debug, Default)]
pub struct JsError {
    /// Human readable message.
    pub message: String,
    /// The thrown value, when the error came from a `throw` statement.
    pub value: Option<Value>,
    /// Stack frames, outermost last.
    pub stack: Vec<String>,
}

impl JsError {
    /// Create a generic error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            value: None,
            stack: Vec::new(),
        }
    }

    /// Create a `TypeError`.
    pub fn type_error(message: impl Into<String>) -> Self {
        let message = format!("TypeError: {}", message.into());
        Self::new(message)
    }

    /// Create a `ReferenceError`.
    pub fn reference_error(message: impl Into<String>) -> Self {
        let message = format!("ReferenceError: {}", message.into());
        Self::new(message)
    }

    /// Create a `SyntaxError`.
    pub fn syntax_error(message: impl Into<String>) -> Self {
        let message = format!("SyntaxError: {}", message.into());
        Self::new(message)
    }

    /// Create a `RangeError`.
    pub fn range_error(message: impl Into<String>) -> Self {
        let message = format!("RangeError: {}", message.into());
        Self::new(message)
    }

    /// Attach a thrown value.
    pub fn with_value(mut self, value: Value) -> Self {
        self.value = Some(value);
        self
    }

    /// The value seen by a `catch` clause.
    pub fn thrown_value(&self) -> Value {
        self.value
            .clone()
            .unwrap_or_else(|| Value::String(self.message.clone()))
    }

    /// Attach the stack of a call chain when no stack is recorded yet.
    pub fn ensure_stack(&mut self, frames: &[String]) {
        if self.stack.is_empty() {
            self.stack = frames.iter().rev().cloned().collect();
        }
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        for frame in &self.stack {
            write!(f, "\n    at {frame}")?;
        }
        Ok(())
    }
}

impl std::error::Error for JsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_prefixed_messages() {
        assert_eq!(JsError::type_error("boom").message, "TypeError: boom");
        assert_eq!(
            JsError::reference_error("missing").message,
            "ReferenceError: missing"
        );
        assert_eq!(JsError::syntax_error("bad").message, "SyntaxError: bad");
        assert_eq!(JsError::range_error("big").message, "RangeError: big");
    }

    #[test]
    fn thrown_value_falls_back_to_message() {
        let error = JsError::new("nope");
        assert_eq!(error.thrown_value(), Value::String("nope".to_owned()));
        let error = JsError::new("nope").with_value(Value::Number(3.0));
        assert_eq!(error.thrown_value(), Value::Number(3.0));
    }

    #[test]
    fn records_stack_once() {
        let mut error = JsError::new("x");
        error.ensure_stack(&["a".to_owned(), "b".to_owned()]);
        assert_eq!(error.stack, vec!["b".to_owned(), "a".to_owned()]);
        error.ensure_stack(&["c".to_owned()]);
        assert_eq!(error.stack.len(), 2);
    }
}
