//! Specificity helpers.

pub use browser_selectors::Specificity;

use std::cmp::Ordering;

/// Compares two specificities, returning `Ordering::Greater` when `a` wins.
pub fn compare(a: Specificity, b: Specificity) -> Ordering {
    (a.a, a.b, a.c).cmp(&(b.a, b.b, b.c))
}

/// The specificity of an inline `style` attribute (models the `a` level).
pub fn inline_specificity() -> Specificity {
    Specificity::new(1, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_lexicographically() {
        assert_eq!(
            compare(Specificity::new(1, 0, 0), Specificity::new(0, 99, 99)),
            Ordering::Greater
        );
        assert_eq!(
            compare(Specificity::new(0, 1, 0), Specificity::new(0, 0, 5)),
            Ordering::Greater
        );
        assert_eq!(
            compare(Specificity::new(0, 0, 1), Specificity::new(0, 0, 1)),
            Ordering::Equal
        );
    }
}
