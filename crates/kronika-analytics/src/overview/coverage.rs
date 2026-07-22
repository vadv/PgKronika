//! Half-open coverage intervals and the honest ratio derived from them.
//!
//! Coverage is a set of `[from_us, to_us)` spans over which a source actually
//! delivered samples. Spans combine by union, never by adding ratios: two
//! overlapping observations of the same interval cover it once, so a merge can
//! never report more than the real covered duration. The ratio against a bucket
//! stays in `[0, 1]` because the intersection is a subset of the bucket.
//!
//! A bucket with no covered duration is not implicitly healthy or zero — the
//! caller reads [`Coverage::is_empty`] and reports `Gap`/`Unknown` rather than a
//! measured value.

/// One half-open coverage span, `[from_us, to_us)` with `from_us < to_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoverageSpan {
    from_us: i64,
    to_us: i64,
}

impl CoverageSpan {
    /// Builds a span, rejecting a degenerate or reversed interval.
    #[must_use]
    pub fn new(from_us: i64, to_us: i64) -> Option<Self> {
        (from_us < to_us).then_some(Self { from_us, to_us })
    }

    /// Inclusive-exclusive start.
    #[must_use]
    pub const fn start_us(self) -> i64 {
        self.from_us
    }

    /// Exclusive end.
    #[must_use]
    pub const fn end_us(self) -> i64 {
        self.to_us
    }

    /// Span length in microseconds, always positive.
    #[must_use]
    pub const fn duration_us(self) -> u64 {
        self.to_us.wrapping_sub(self.from_us).cast_unsigned()
    }
}

/// A normalized coverage set: sorted, disjoint, non-adjacent spans.
///
/// The normal form makes covered duration exact and order-independent, so the
/// same spans built from any partition and merged in any order yield the same
/// set and the same duration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    spans: Vec<CoverageSpan>,
}

impl Coverage {
    /// The empty coverage set: no interval is covered.
    #[must_use]
    pub const fn empty() -> Self {
        Self { spans: Vec::new() }
    }

    /// Builds normalized coverage from raw spans, merging overlapping and
    /// adjacent intervals into their union. Input order does not matter.
    #[must_use]
    pub fn from_spans(mut spans: Vec<CoverageSpan>) -> Self {
        spans.sort_unstable();
        let mut merged: Vec<CoverageSpan> = Vec::with_capacity(spans.len());
        for span in spans {
            match merged.last_mut() {
                // Overlap or touch: extend the open span instead of adding a
                // second one, so the shared microseconds count once.
                Some(last) if span.from_us <= last.to_us => {
                    last.to_us = last.to_us.max(span.to_us);
                }
                _ => merged.push(span),
            }
        }
        Self { spans: merged }
    }

    /// The normalized spans, sorted and disjoint.
    #[must_use]
    pub fn spans(&self) -> &[CoverageSpan] {
        &self.spans
    }

    /// Whether no interval is covered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Total covered microseconds. Overlaps count once by construction.
    #[must_use]
    pub fn covered_duration_us(&self) -> u64 {
        self.spans.iter().map(|span| span.duration_us()).sum()
    }

    /// Union with another coverage set, staying normalized.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut spans = self.spans.clone();
        spans.extend_from_slice(&other.spans);
        Self::from_spans(spans)
    }

    /// Covered fraction of `bucket`, in `[0, 1]`.
    ///
    /// Only the part of the coverage intersecting the bucket counts, so the
    /// result is a true subset ratio and cannot exceed one.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a bucket duration stays far below 2^53 microseconds, so the \
                  f64 ratio is exact"
    )]
    pub fn covered_ratio(&self, bucket: CoverageSpan) -> f64 {
        let mut covered = 0_u64;
        for span in &self.spans {
            let lo = span.from_us.max(bucket.from_us);
            let hi = span.to_us.min(bucket.to_us);
            if lo < hi {
                covered += (hi - lo).cast_unsigned();
            }
        }
        covered as f64 / bucket.duration_us() as f64
    }
}

/// Whether a factor even applies to a source in a window.
///
/// Distinct from coverage: an inapplicable factor is not a gap, and an
/// unsupported one is not a measured zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// The factor applies and a value is expected.
    Applicable,
    /// The factor does not apply to this source or configuration.
    NotApplicable,
    /// The current extractor cannot read this factor's source layout.
    Unsupported,
}

/// The coverage state of a factor over a bucket.
///
/// A closed set that keeps missing, gap, and not-collected distinct from a
/// measured value, so none collapses into a false zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageState {
    /// Every expected sample is present.
    Complete,
    /// Some but not all expected samples are present.
    Partial,
    /// A coverage gap: no readable samples span part of the bucket.
    Gap,
    /// Applicability or presence cannot be determined.
    Unknown,
    /// The source is gated off, so absence is not a measured zero.
    NotCollected,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "ratios asserted are exact dyadic values (0.0, 0.5, 1.0)"
    )]

    use super::*;

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid span in fixture")
    }

    #[test]
    fn a_degenerate_or_reversed_span_is_rejected() {
        assert_eq!(CoverageSpan::new(5, 5), None);
        assert_eq!(CoverageSpan::new(6, 5), None);
        assert!(CoverageSpan::new(5, 6).is_some());
    }

    #[test]
    fn overlapping_spans_count_shared_microseconds_once() {
        // [0,10) ∪ [5,15) = [0,15): duration 15, not 20.
        let c = Coverage::from_spans(vec![span(0, 10), span(5, 15)]);
        assert_eq!(c.spans(), &[span(0, 15)]);
        assert_eq!(c.covered_duration_us(), 15);
    }

    #[test]
    fn adjacent_spans_merge_but_gapped_spans_stay_separate() {
        let touching = Coverage::from_spans(vec![span(0, 5), span(5, 10)]);
        assert_eq!(touching.spans(), &[span(0, 10)]);

        let gapped = Coverage::from_spans(vec![span(0, 5), span(6, 10)]);
        assert_eq!(gapped.spans(), &[span(0, 5), span(6, 10)]);
        assert_eq!(gapped.covered_duration_us(), 9);
    }

    #[test]
    fn from_spans_is_independent_of_input_order() {
        let forward = Coverage::from_spans(vec![span(0, 5), span(10, 20), span(4, 12)]);
        let shuffled = Coverage::from_spans(vec![span(10, 20), span(4, 12), span(0, 5)]);
        assert_eq!(forward, shuffled);
        assert_eq!(forward.spans(), &[span(0, 20)]);
    }

    #[test]
    fn union_is_commutative_and_idempotent() {
        let a = Coverage::from_spans(vec![span(0, 10)]);
        let b = Coverage::from_spans(vec![span(8, 20)]);
        assert_eq!(a.union(&b), b.union(&a));
        assert_eq!(a.union(&a), a);
    }

    #[test]
    fn union_is_associative() {
        let a = Coverage::from_spans(vec![span(0, 5)]);
        let b = Coverage::from_spans(vec![span(10, 15)]);
        let c = Coverage::from_spans(vec![span(4, 11)]);
        let left = a.union(&b).union(&c);
        let right = a.union(&b.union(&c));
        assert_eq!(left, right);
        // All three chain into one span [0,15).
        assert_eq!(left.spans(), &[span(0, 15)]);
    }

    #[test]
    fn covered_ratio_stays_within_zero_and_one() {
        let bucket = span(0, 100);
        assert_eq!(Coverage::empty().covered_ratio(bucket), 0.0);

        let full = Coverage::from_spans(vec![span(0, 100)]);
        assert_eq!(full.covered_ratio(bucket), 1.0);

        // Coverage overflowing the bucket on both sides still ratios to 1.0.
        let overflow = Coverage::from_spans(vec![span(-50, 150)]);
        assert_eq!(overflow.covered_ratio(bucket), 1.0);

        let half = Coverage::from_spans(vec![span(0, 50)]);
        assert_eq!(half.covered_ratio(bucket), 0.5);
    }

    #[test]
    fn covered_ratio_ignores_coverage_outside_the_bucket() {
        // Coverage entirely before the bucket contributes nothing.
        let outside = Coverage::from_spans(vec![span(-100, -10)]);
        assert_eq!(outside.covered_ratio(span(0, 100)), 0.0);
    }
}
