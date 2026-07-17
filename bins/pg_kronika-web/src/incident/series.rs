//! Preloaded numeric series a lens reads by reference.
//!
//! The request decodes each series once into an owned buffer; lenses borrow
//! slices and never copy. A series is validated at the boundary: parallel
//! arrays, ascending timestamps, finite values — so a lens never scores a `NaN`.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::model::{EpisodeRefV1, IdentityValue};

/// One series' timeline: parallel time-ordered arrays. Cumulative columns carry
/// diff rates, gauges carry raw readings.
pub(crate) struct Series {
    ts: Vec<i64>,
    values: Vec<f64>,
}

impl Series {
    /// Build a series from parallel arrays, rejecting a length mismatch,
    /// out-of-order timestamps, or a non-finite value.
    pub(crate) fn new(ts: Vec<i64>, values: Vec<f64>) -> Option<Self> {
        if ts.len() != values.len() {
            return None;
        }
        if !ts.windows(2).all(|pair| pair[0] <= pair[1]) {
            return None;
        }
        if !values.iter().all(|value| value.is_finite()) {
            return None;
        }
        Some(Self { ts, values })
    }

    pub(crate) fn ts(&self) -> &[i64] {
        &self.ts
    }

    pub(crate) fn values(&self) -> &[f64] {
        &self.values
    }

    pub(crate) const fn len(&self) -> usize {
        self.ts.len()
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
}

/// Identity of a series inside a request: section, column, and entity identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesId {
    section: &'static str,
    column: &'static str,
    identity: Arc<[IdentityValue]>,
}

/// The series decoded for one request, owned for its lifetime. Lenses look up a
/// series by the episode reference that names it.
#[derive(Default)]
pub(crate) struct SeriesSet {
    series: BTreeMap<SeriesId, Series>,
}

impl SeriesSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(
        &mut self,
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
        series: Series,
    ) {
        self.series.insert(
            SeriesId {
                section,
                column,
                identity,
            },
            series,
        );
    }

    /// The series for an episode's `(section, column, identity)`, if present.
    pub(crate) fn get(&self, reference: &EpisodeRefV1) -> Option<&Series> {
        self.series.get(&SeriesId {
            section: reference.logical_section,
            column: reference.column,
            identity: Arc::clone(&reference.identity),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(section: &'static str, column: &'static str, id: i64) -> EpisodeRefV1 {
        EpisodeRefV1 {
            logical_section: section,
            column,
            identity: Arc::from(vec![IdentityValue::I64(id)]),
            start_us: 0,
            end_us: 1,
        }
    }

    #[test]
    fn parallel_finite_ascending_arrays_build_a_series() {
        let series = Series::new(vec![1, 2, 3], vec![10.0, 20.0, 30.0]).expect("valid series");
        assert_eq!(series.len(), 3);
        assert_eq!(series.ts(), &[1, 2, 3]);
        assert_eq!(series.values(), &[10.0, 20.0, 30.0]);
    }

    #[test]
    fn an_empty_series_is_valid() {
        let series = Series::new(vec![], vec![]).expect("empty is valid");
        assert!(series.is_empty());
    }

    #[test]
    fn a_length_mismatch_is_rejected() {
        assert!(Series::new(vec![1, 2], vec![1.0]).is_none());
    }

    #[test]
    fn out_of_order_timestamps_are_rejected() {
        assert!(Series::new(vec![2, 1], vec![1.0, 2.0]).is_none());
    }

    #[test]
    fn a_non_finite_value_is_rejected() {
        assert!(Series::new(vec![1], vec![f64::NAN]).is_none());
        assert!(Series::new(vec![1], vec![f64::INFINITY]).is_none());
    }

    #[test]
    fn an_empty_set_finds_nothing() {
        let set = SeriesSet::new();
        assert!(set.get(&reference("s", "c", 1)).is_none());
    }

    #[test]
    fn a_series_is_found_by_its_reference() {
        let mut set = SeriesSet::new();
        let key = reference("s", "c", 1);
        set.insert(
            "s",
            "c",
            Arc::clone(&key.identity),
            Series::new(vec![1], vec![9.0]).expect("valid"),
        );
        assert_eq!(set.get(&key).map(Series::len), Some(1));
    }

    #[test]
    fn a_different_section_identity_or_column_is_a_miss() {
        let mut set = SeriesSet::new();
        set.insert(
            "s",
            "c",
            Arc::from(vec![IdentityValue::I64(1)]),
            Series::new(vec![1], vec![9.0]).expect("valid"),
        );
        assert!(
            set.get(&reference("t", "c", 1)).is_none(),
            "section differs"
        );
        assert!(
            set.get(&reference("s", "c", 2)).is_none(),
            "identity differs"
        );
        assert!(set.get(&reference("s", "d", 1)).is_none(), "column differs");
    }
}
