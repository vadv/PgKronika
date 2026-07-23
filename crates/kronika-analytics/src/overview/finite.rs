//! Finite floating-point values used by retained analytics contracts.

use std::fmt;

/// A finite `f64` with both signed zero encodings normalized to `0.0`.
///
/// The wrapper gives retained records stable equality while rejecting `NaN`
/// and infinities at the boundary.
#[derive(Clone, Copy, PartialEq)]
pub struct FiniteF64(f64);

impl FiniteF64 {
    /// Builds a finite value.
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        Some(Self(if value == 0.0 { 0.0 } else { value }))
    }

    /// Returns the wrapped value.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Eq for FiniteF64 {}

impl fmt::Debug for FiniteF64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FiniteF64").field(&self.0).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::FiniteF64;

    #[test]
    fn rejects_non_finite_values_and_normalizes_zero() {
        assert_eq!(FiniteF64::new(f64::NAN), None);
        assert_eq!(FiniteF64::new(f64::INFINITY), None);
        assert_eq!(FiniteF64::new(f64::NEG_INFINITY), None);
        assert_eq!(FiniteF64::new(-0.0), FiniteF64::new(0.0));
        assert_eq!(FiniteF64::new(2.5).map(FiniteF64::get), Some(2.5));
    }
}
