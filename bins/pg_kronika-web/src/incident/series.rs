//! Validated numeric series held for one analysis request.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::dispatch::LimitHit;
use super::evidence::sink::FindingSink;
use super::model::{EpisodeRefV1, IdentityValue};

pub(crate) struct Series {
    ts: Vec<i64>,
    values: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeriesError {
    LengthMismatch,
    TimestampsNotStrictlyIncreasing,
    NonFiniteValue,
}

impl Series {
    pub(crate) fn new(ts: Vec<i64>, values: Vec<f64>) -> Result<Self, SeriesError> {
        if ts.len() != values.len() {
            return Err(SeriesError::LengthMismatch);
        }
        if !ts.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(SeriesError::TimestampsNotStrictlyIncreasing);
        }
        if !values.iter().all(|value| value.is_finite()) {
            return Err(SeriesError::NonFiniteValue);
        }
        Ok(Self { ts, values })
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesId {
    section: &'static str,
    column: &'static str,
    identity: Arc<[IdentityValue]>,
}

pub(crate) struct SeriesSet {
    series: BTreeMap<SeriesId, Series>,
    points: usize,
    point_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeriesInsertError {
    Duplicate,
    PointLimit { observed: usize, limit: usize },
}

impl SeriesSet {
    #[cfg(test)]
    pub(crate) const fn for_test(point_limit: usize) -> Self {
        Self {
            series: BTreeMap::new(),
            points: 0,
            point_limit,
        }
    }

    pub(crate) fn insert(
        &mut self,
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
        series: Series,
    ) -> Result<(), SeriesInsertError> {
        let id = SeriesId {
            section,
            column,
            identity,
        };
        if self.series.contains_key(&id) {
            return Err(SeriesInsertError::Duplicate);
        }

        let observed =
            self.points
                .checked_add(series.len())
                .ok_or(SeriesInsertError::PointLimit {
                    observed: usize::MAX,
                    limit: self.point_limit,
                })?;
        if observed > self.point_limit {
            return Err(SeriesInsertError::PointLimit {
                observed,
                limit: self.point_limit,
            });
        }

        self.series.insert(id, series);
        self.points = observed;
        Ok(())
    }

    pub(crate) fn get<'a>(
        &'a self,
        reference: &EpisodeRefV1,
        sink: &mut FindingSink<'_>,
    ) -> Result<Option<&'a Series>, LimitHit> {
        let series = self.lookup(reference);
        if let Some(series) = series {
            sink.charge_points(series.len())?;
        }
        Ok(series)
    }

    fn lookup(&self, reference: &EpisodeRefV1) -> Option<&Series> {
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
        assert_eq!(
            Series::new(vec![1, 2], vec![1.0]).err(),
            Some(SeriesError::LengthMismatch)
        );
    }

    #[test]
    fn out_of_order_timestamps_are_rejected() {
        assert_eq!(
            Series::new(vec![2, 1], vec![1.0, 2.0]).err(),
            Some(SeriesError::TimestampsNotStrictlyIncreasing)
        );
    }

    #[test]
    fn duplicate_timestamps_are_rejected() {
        assert_eq!(
            Series::new(vec![1, 1], vec![1.0, 2.0]).err(),
            Some(SeriesError::TimestampsNotStrictlyIncreasing)
        );
    }

    #[test]
    fn a_non_finite_value_is_rejected() {
        assert_eq!(
            Series::new(vec![1], vec![f64::NAN]).err(),
            Some(SeriesError::NonFiniteValue)
        );
        assert_eq!(
            Series::new(vec![1], vec![f64::INFINITY]).err(),
            Some(SeriesError::NonFiniteValue)
        );
    }

    #[test]
    fn an_empty_set_finds_nothing() {
        let set = SeriesSet::for_test(10);
        assert!(set.lookup(&reference("s", "c", 1)).is_none());
    }

    #[test]
    fn a_series_is_found_by_its_reference() {
        let mut set = SeriesSet::for_test(10);
        let key = reference("s", "c", 1);
        set.insert(
            "s",
            "c",
            Arc::clone(&key.identity),
            Series::new(vec![1], vec![9.0]).expect("valid"),
        )
        .expect("unique series within limit");
        assert_eq!(set.lookup(&key).map(Series::len), Some(1));
    }

    #[test]
    fn a_different_section_identity_or_column_is_a_miss() {
        let mut set = SeriesSet::for_test(10);
        set.insert(
            "s",
            "c",
            Arc::from(vec![IdentityValue::I64(1)]),
            Series::new(vec![1], vec![9.0]).expect("valid"),
        )
        .expect("unique series within limit");
        assert!(
            set.lookup(&reference("t", "c", 1)).is_none(),
            "section differs"
        );
        assert!(
            set.lookup(&reference("s", "c", 2)).is_none(),
            "identity differs"
        );
        assert!(
            set.lookup(&reference("s", "d", 1)).is_none(),
            "column differs"
        );
    }

    #[test]
    fn duplicate_series_is_rejected_without_overwrite() {
        let mut set = SeriesSet::for_test(10);
        let identity = Arc::from(vec![IdentityValue::I64(1)]);
        set.insert(
            "s",
            "c",
            Arc::clone(&identity),
            Series::new(vec![1], vec![1.0]).expect("valid"),
        )
        .expect("first insert");
        assert_eq!(
            set.insert(
                "s",
                "c",
                identity,
                Series::new(vec![2], vec![2.0]).expect("valid"),
            ),
            Err(SeriesInsertError::Duplicate)
        );
        assert_eq!(
            set.lookup(&reference("s", "c", 1)).map(Series::values),
            Some(&[1.0][..])
        );
    }

    #[test]
    fn total_points_are_bounded() {
        let mut set = SeriesSet::for_test(1);
        assert_eq!(
            set.insert(
                "s",
                "c",
                Arc::from(vec![IdentityValue::I64(1)]),
                Series::new(vec![1, 2], vec![1.0, 2.0]).expect("valid"),
            ),
            Err(SeriesInsertError::PointLimit {
                observed: 2,
                limit: 1,
            })
        );
    }

    #[test]
    fn series_lookup_charges_every_point_before_returning_data() {
        let key = reference("s", "c", 1);
        let mut set = SeriesSet::for_test(2);
        set.insert(
            "s",
            "c",
            Arc::clone(&key.identity),
            Series::new(vec![1, 2], vec![1.0, 2.0]).expect("valid"),
        )
        .expect("within point limit");

        let mut findings = Vec::new();
        let mut budget = super::super::dispatch::WorkBudget::new(1);
        let mut counts = super::super::evidence::sink::OutputCounts::new();
        let mut sink = FindingSink::new(
            &mut findings,
            &mut budget,
            &mut counts,
            super::super::evidence::sink::OutputLimits::new(0, 0),
            "TEST",
            super::super::evidence::ConfidenceCap::Low,
        );
        assert_eq!(
            set.get(&key, &mut sink).err(),
            Some(LimitHit {
                axis: super::super::dispatch::LimitAxis::Work,
                observed: 2,
                limit: 1,
            })
        );
    }
}
