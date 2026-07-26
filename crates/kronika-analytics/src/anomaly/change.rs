//! Deterministic categorical-distribution and per-unit work comparisons.

use std::collections::BTreeMap;

/// One observed category and its non-negative count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryCount {
    /// Stable category identity.
    pub category: i64,
    /// Observed count assigned to the category.
    pub count: u64,
}

impl CategoryCount {
    /// Construct one category observation.
    #[must_use]
    pub const fn new(category: i64, count: u64) -> Self {
        Self { category, count }
    }
}

/// Minimum sample and effect gates for a categorical-distribution comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistributionParams {
    /// Minimum total reference count.
    pub reference_count: u64,
    /// Minimum total current count.
    pub current_count: u64,
    /// Minimum total-variation distance in the inclusive range `[0, 1]`.
    pub total_variation: f64,
}

impl DistributionParams {
    /// Clamp count and effect gates to valid, fail-closed values.
    #[must_use]
    pub fn new(min_reference_count: u64, min_current_count: u64, min_total_variation: f64) -> Self {
        let min_total_variation = if min_total_variation.is_finite() {
            min_total_variation.clamp(0.0, 1.0)
        } else {
            1.0
        };
        Self {
            reference_count: min_reference_count.max(1),
            current_count: min_current_count.max(1),
            total_variation: min_total_variation,
        }
    }
}

impl Default for DistributionParams {
    /// Require 20 observations on each side and a 20% distribution effect.
    fn default() -> Self {
        Self::new(20, 20, 0.20)
    }
}

/// Why a count-based comparison could not produce a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeNotEvaluatedReason {
    /// The reference side did not meet its minimum count.
    ReferenceTooSmall,
    /// The current side did not meet its minimum count.
    CurrentTooSmall,
    /// Summing input counts exceeded the representable range.
    CountOverflow,
}

/// Share evidence for one category, ordered by category identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CategoryShareChange {
    /// Stable category identity.
    pub category: i64,
    /// Reference count for this category.
    pub reference_count: u64,
    /// Current count for this category.
    pub current_count: u64,
    /// Reference count divided by the total reference count.
    pub reference_share: f64,
    /// Current count divided by the total current count.
    pub current_share: f64,
    /// `current_share - reference_share`.
    pub share_delta: f64,
}

/// Complete evidence for one categorical-distribution comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct DistributionEvidence {
    /// Total reference count.
    pub reference_total: u64,
    /// Total current count.
    pub current_total: u64,
    /// Half the L1 distance between normalized category shares, in `[0, 1]`.
    pub total_variation: f64,
    /// Largest absolute per-category share change.
    pub max_abs_share_delta: f64,
    /// Per-category evidence in ascending category order.
    pub categories: Vec<CategoryShareChange>,
}

/// Verdict for one categorical-distribution comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum DistributionOutcome {
    /// The sample gates passed and the effect met the configured threshold.
    Shift(DistributionEvidence),
    /// The sample gates passed but the effect stayed below the threshold.
    Stable(DistributionEvidence),
    /// No verdict was possible.
    NotEvaluated(ChangeNotEvaluatedReason),
}

impl DistributionOutcome {
    /// Return evaluated evidence regardless of whether it crossed the effect gate.
    #[must_use]
    pub const fn evidence(&self) -> Option<&DistributionEvidence> {
        match self {
            Self::Shift(evidence) | Self::Stable(evidence) => Some(evidence),
            Self::NotEvaluated(_) => None,
        }
    }
}

/// Compare two categorical count distributions using total-variation distance.
///
/// Duplicate category rows are summed with overflow checks. Zero-count rows
/// have no effect. Normalizing each side independently means a uniform change
/// in observation frequency is stable; only the category mixture changes the
/// verdict. Output is ordered by category identity, independent of input order.
///
/// Memory is `O(reference categories + current categories)`. Adapters must
/// bound input categories before calling this kernel.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "u64 counts are normalized to finite f64 shares; exact integer totals remain in evidence"
)]
pub fn compare_distributions(
    reference: &[CategoryCount],
    current: &[CategoryCount],
    params: &DistributionParams,
) -> DistributionOutcome {
    let mut counts = BTreeMap::<i64, (u64, u64)>::new();
    let reference_total = match accumulate_categories(reference, &mut counts, Side::Reference) {
        Ok(total) => total,
        Err(reason) => return DistributionOutcome::NotEvaluated(reason),
    };
    let current_total = match accumulate_categories(current, &mut counts, Side::Current) {
        Ok(total) => total,
        Err(reason) => return DistributionOutcome::NotEvaluated(reason),
    };

    if reference_total < params.reference_count {
        return DistributionOutcome::NotEvaluated(ChangeNotEvaluatedReason::ReferenceTooSmall);
    }
    if current_total < params.current_count {
        return DistributionOutcome::NotEvaluated(ChangeNotEvaluatedReason::CurrentTooSmall);
    }

    let reference_denominator = reference_total as f64;
    let current_denominator = current_total as f64;
    let mut l1_distance = 0.0;
    let mut max_abs_share_delta: f64 = 0.0;
    let mut categories = Vec::with_capacity(counts.len());

    for (category, (reference_count, current_count)) in counts {
        let reference_share = reference_count as f64 / reference_denominator;
        let current_share = current_count as f64 / current_denominator;
        let share_delta = current_share - reference_share;
        let abs_share_delta = share_delta.abs();
        l1_distance += abs_share_delta;
        max_abs_share_delta = max_abs_share_delta.max(abs_share_delta);
        categories.push(CategoryShareChange {
            category,
            reference_count,
            current_count,
            reference_share,
            current_share,
            share_delta,
        });
    }

    let total_variation = (l1_distance / 2.0).clamp(0.0, 1.0);
    let evidence = DistributionEvidence {
        reference_total,
        current_total,
        total_variation,
        max_abs_share_delta,
        categories,
    };
    if total_variation > 0.0 && total_variation >= params.total_variation {
        DistributionOutcome::Shift(evidence)
    } else {
        DistributionOutcome::Stable(evidence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Reference,
    Current,
}

fn accumulate_categories(
    input: &[CategoryCount],
    counts: &mut BTreeMap<i64, (u64, u64)>,
    side: Side,
) -> Result<u64, ChangeNotEvaluatedReason> {
    let mut total = 0_u64;
    for sample in input.iter().filter(|sample| sample.count != 0) {
        total = total
            .checked_add(sample.count)
            .ok_or(ChangeNotEvaluatedReason::CountOverflow)?;
        let entry = counts.entry(sample.category).or_default();
        let destination = match side {
            Side::Reference => &mut entry.0,
            Side::Current => &mut entry.1,
        };
        *destination = destination
            .checked_add(sample.count)
            .ok_or(ChangeNotEvaluatedReason::CountOverflow)?;
    }
    Ok(total)
}

/// Aggregated operation count and non-negative work performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkTotals {
    /// Operations that performed the work.
    pub operations: u64,
    /// Work units performed by those operations.
    pub work: u64,
}

impl WorkTotals {
    /// Construct aggregated operation and work totals.
    #[must_use]
    pub const fn new(operations: u64, work: u64) -> Self {
        Self { operations, work }
    }
}

/// Minimum sample and effect gates for a per-unit work comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerUnitParams {
    /// Minimum reference operations.
    pub reference_operations: u64,
    /// Minimum current operations.
    pub current_operations: u64,
    /// Minimum absolute increase in work per operation.
    pub absolute_increase: f64,
    /// Minimum relative increase as a fraction of the reference value.
    pub relative_increase: f64,
}

impl PerUnitParams {
    /// Clamp count and effect gates to valid, fail-closed values.
    #[must_use]
    pub fn new(
        min_reference_operations: u64,
        min_current_operations: u64,
        min_absolute_increase: f64,
        min_relative_increase: f64,
    ) -> Self {
        Self {
            reference_operations: min_reference_operations.max(1),
            current_operations: min_current_operations.max(1),
            absolute_increase: finite_non_negative_or_max(min_absolute_increase),
            relative_increase: finite_non_negative_or_max(min_relative_increase),
        }
    }
}

impl Default for PerUnitParams {
    /// Require 20 operations on each side, one work unit and a 50% increase.
    fn default() -> Self {
        Self::new(20, 20, 1.0, 0.50)
    }
}

const fn finite_non_negative_or_max(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        f64::MAX
    }
}

/// Complete evidence for one per-unit work comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerUnitEvidence {
    /// Aggregated reference inputs.
    pub reference: WorkTotals,
    /// Aggregated current inputs.
    pub current: WorkTotals,
    /// Reference work divided by reference operations.
    pub reference_per_unit: f64,
    /// Current work divided by current operations.
    pub current_per_unit: f64,
    /// `current_per_unit - reference_per_unit`.
    pub delta_per_unit: f64,
    /// Relative delta from the reference value, or `None` for a zero reference.
    pub relative_delta: Option<f64>,
    /// Whether the configured absolute effect gate passed.
    pub absolute_effect_met: bool,
    /// Whether the configured relative effect gate passed.
    pub relative_effect_met: bool,
}

/// Verdict for one per-unit work comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PerUnitOutcome {
    /// Work per operation increased through both configured effect gates.
    Increase(PerUnitEvidence),
    /// Sample gates passed, but there was no qualifying increase.
    Stable(PerUnitEvidence),
    /// No verdict was possible.
    NotEvaluated(ChangeNotEvaluatedReason),
}

impl PerUnitOutcome {
    /// Return evaluated evidence regardless of whether it crossed the effect gates.
    #[must_use]
    pub const fn evidence(&self) -> Option<&PerUnitEvidence> {
        match self {
            Self::Increase(evidence) | Self::Stable(evidence) => Some(evidence),
            Self::NotEvaluated(_) => None,
        }
    }
}

/// Compare work per operation between reference and current aggregates.
///
/// The comparison uses rates rather than cumulative totals. A higher operation
/// frequency with unchanged work per operation is stable. The relative delta
/// is absent when reference work is zero; any positive current rate then
/// passes the relative gate, while the absolute gate still applies.
///
/// This function is allocation-free. It reports association between the two
/// observed windows and does not assign causality.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "u64 inputs become finite f64 rates while exact totals remain in evidence"
)]
pub fn compare_per_unit(
    reference: WorkTotals,
    current: WorkTotals,
    params: &PerUnitParams,
) -> PerUnitOutcome {
    if reference.operations < params.reference_operations {
        return PerUnitOutcome::NotEvaluated(ChangeNotEvaluatedReason::ReferenceTooSmall);
    }
    if current.operations < params.current_operations {
        return PerUnitOutcome::NotEvaluated(ChangeNotEvaluatedReason::CurrentTooSmall);
    }

    let reference_per_unit = reference.work as f64 / reference.operations as f64;
    let current_per_unit = current.work as f64 / current.operations as f64;
    let delta_per_unit = current_per_unit - reference_per_unit;
    let relative_delta = (reference.work != 0).then(|| delta_per_unit / reference_per_unit);
    let absolute_effect_met = delta_per_unit >= params.absolute_increase;
    let relative_effect_met =
        relative_delta.map_or(current.work != 0, |delta| delta >= params.relative_increase);

    let reference_cross = u128::from(reference.work) * u128::from(current.operations);
    let current_cross = u128::from(current.work) * u128::from(reference.operations);
    let strictly_increased = current_cross > reference_cross;
    let evidence = PerUnitEvidence {
        reference,
        current,
        reference_per_unit,
        current_per_unit,
        delta_per_unit,
        relative_delta,
        absolute_effect_met,
        relative_effect_met,
    };
    if strictly_increased && absolute_effect_met && relative_effect_met {
        PerUnitOutcome::Increase(evidence)
    } else {
        PerUnitOutcome::Stable(evidence)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{
        CategoryCount, ChangeNotEvaluatedReason, DistributionEvidence, DistributionOutcome,
        DistributionParams, PerUnitOutcome, PerUnitParams, WorkTotals, compare_distributions,
        compare_per_unit,
    };

    fn distribution_evidence(outcome: &DistributionOutcome) -> &DistributionEvidence {
        outcome
            .evidence()
            .expect("test input must pass distribution sample gates")
    }

    #[test]
    fn changed_category_set_crosses_total_variation_gate() {
        let reference = [CategoryCount::new(10, 80), CategoryCount::new(20, 20)];
        let current = [CategoryCount::new(10, 50), CategoryCount::new(30, 50)];
        let outcome = compare_distributions(&reference, &current, &DistributionParams::default());
        let DistributionOutcome::Shift(evidence) = outcome else {
            panic!("expected a distribution shift");
        };
        assert!((evidence.total_variation - 0.50).abs() < 1e-12);
        assert_eq!(
            evidence
                .categories
                .iter()
                .map(|category| category.category)
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
    }

    #[test]
    fn execution_frequency_change_with_same_mix_is_stable() {
        let reference = [CategoryCount::new(10, 15), CategoryCount::new(20, 5)];
        let current = [CategoryCount::new(10, 1_500), CategoryCount::new(20, 500)];
        let outcome = compare_distributions(&reference, &current, &DistributionParams::default());
        let DistributionOutcome::Stable(evidence) = outcome else {
            panic!("same normalized mixture must be stable");
        };
        assert!(evidence.total_variation.abs() < 1e-12);
    }

    #[test]
    fn duplicate_rows_are_summed_and_zero_rows_have_no_effect() {
        let reference = [
            CategoryCount::new(2, 10),
            CategoryCount::new(1, 10),
            CategoryCount::new(2, 10),
            CategoryCount::new(3, 0),
        ];
        let current = [
            CategoryCount::new(2, 20),
            CategoryCount::new(1, 10),
            CategoryCount::new(1, 10),
        ];
        let evidence = distribution_evidence(&compare_distributions(
            &reference,
            &current,
            &DistributionParams::new(1, 1, 0.0),
        ))
        .clone();
        assert_eq!(evidence.reference_total, 30);
        assert_eq!(evidence.current_total, 40);
        assert_eq!(evidence.categories.len(), 2);
        assert_eq!(evidence.categories[0].category, 1);
        assert_eq!(evidence.categories[1].category, 2);
    }

    #[test]
    fn minimum_effect_boundary_is_inclusive_but_zero_is_stable() {
        let reference = [CategoryCount::new(1, 80), CategoryCount::new(2, 20)];
        let current = [CategoryCount::new(1, 60), CategoryCount::new(2, 40)];
        assert!(matches!(
            compare_distributions(&reference, &current, &DistributionParams::new(20, 20, 0.20)),
            DistributionOutcome::Shift(_)
        ));
        assert!(matches!(
            compare_distributions(
                &reference,
                &reference,
                &DistributionParams::new(20, 20, 0.0)
            ),
            DistributionOutcome::Stable(_)
        ));
    }

    #[test]
    fn undersized_and_overflowing_distributions_have_exact_reasons() {
        let enough = [CategoryCount::new(1, 20)];
        assert_eq!(
            compare_distributions(&[], &enough, &DistributionParams::default()),
            DistributionOutcome::NotEvaluated(ChangeNotEvaluatedReason::ReferenceTooSmall)
        );
        assert_eq!(
            compare_distributions(&enough, &[], &DistributionParams::default()),
            DistributionOutcome::NotEvaluated(ChangeNotEvaluatedReason::CurrentTooSmall)
        );
        let overflow = [CategoryCount::new(1, u64::MAX), CategoryCount::new(1, 1)];
        assert_eq!(
            compare_distributions(&overflow, &enough, &DistributionParams::default()),
            DistributionOutcome::NotEvaluated(ChangeNotEvaluatedReason::CountOverflow)
        );
    }

    #[test]
    fn higher_totals_with_unchanged_per_unit_work_are_stable() {
        let params = PerUnitParams::default();
        let outcome = compare_per_unit(
            WorkTotals::new(20, 200),
            WorkTotals::new(2_000, 20_000),
            &params,
        );
        let PerUnitOutcome::Stable(evidence) = outcome else {
            panic!("frequency alone must not qualify as increased work per operation");
        };
        assert!(evidence.delta_per_unit.abs() < 1e-12);
    }

    #[test]
    fn per_unit_increase_must_cross_both_effect_gates() {
        let params = PerUnitParams::new(20, 20, 1.0, 0.50);
        let outcome = compare_per_unit(WorkTotals::new(20, 200), WorkTotals::new(20, 320), &params);
        let PerUnitOutcome::Increase(evidence) = outcome else {
            panic!("60% and six units per operation must qualify");
        };
        assert!((evidence.reference_per_unit - 10.0).abs() < 1e-12);
        assert!((evidence.current_per_unit - 16.0).abs() < 1e-12);
        assert!((evidence.relative_delta.expect("non-zero reference") - 0.60).abs() < 1e-12);

        assert!(matches!(
            compare_per_unit(WorkTotals::new(20, 200), WorkTotals::new(20, 218), &params),
            PerUnitOutcome::Stable(_)
        ));
    }

    #[test]
    fn zero_reference_is_explainable_and_still_needs_absolute_effect() {
        let params = PerUnitParams::new(20, 20, 1.0, 100.0);
        let PerUnitOutcome::Increase(evidence) =
            compare_per_unit(WorkTotals::new(20, 0), WorkTotals::new(20, 20), &params)
        else {
            panic!("zero-to-one work per operation crosses the absolute gate");
        };
        assert_eq!(evidence.relative_delta, None);
        assert!(evidence.relative_effect_met);

        assert!(matches!(
            compare_per_unit(WorkTotals::new(20, 0), WorkTotals::new(40, 20), &params),
            PerUnitOutcome::Stable(_)
        ));
    }

    #[test]
    fn low_operation_counts_are_not_evaluated() {
        let params = PerUnitParams::default();
        assert_eq!(
            compare_per_unit(
                WorkTotals::new(19, 1_000),
                WorkTotals::new(20, 2_000),
                &params
            ),
            PerUnitOutcome::NotEvaluated(ChangeNotEvaluatedReason::ReferenceTooSmall)
        );
        assert_eq!(
            compare_per_unit(
                WorkTotals::new(20, 1_000),
                WorkTotals::new(19, 2_000),
                &params
            ),
            PerUnitOutcome::NotEvaluated(ChangeNotEvaluatedReason::CurrentTooSmall)
        );
    }

    proptest! {
        #[test]
        fn total_variation_is_symmetric_and_bounded(
            a1 in 1_u64..10_000,
            a2 in 1_u64..10_000,
            b1 in 1_u64..10_000,
            b2 in 1_u64..10_000,
        ) {
            let a = [CategoryCount::new(1, a1), CategoryCount::new(2, a2)];
            let b = [CategoryCount::new(1, b1), CategoryCount::new(2, b2)];
            let params = DistributionParams::new(1, 1, 0.5);
            let ab = distribution_evidence(&compare_distributions(&a, &b, &params))
                .total_variation;
            let ba = distribution_evidence(&compare_distributions(&b, &a, &params))
                .total_variation;
            prop_assert!((ab - ba).abs() < 1e-12);
            prop_assert!((0.0..=1.0).contains(&ab));
        }

        #[test]
        fn scaling_both_category_counts_preserves_distribution(
            a1 in 1_u64..1_000,
            a2 in 1_u64..1_000,
            b1 in 1_u64..1_000,
            b2 in 1_u64..1_000,
            scale in 1_u64..1_000,
        ) {
            let reference = [CategoryCount::new(1, a1), CategoryCount::new(2, a2)];
            let current = [CategoryCount::new(1, b1), CategoryCount::new(2, b2)];
            let scaled_reference = [
                CategoryCount::new(1, a1 * scale),
                CategoryCount::new(2, a2 * scale),
            ];
            let scaled_current = [
                CategoryCount::new(1, b1 * scale),
                CategoryCount::new(2, b2 * scale),
            ];
            let params = DistributionParams::new(1, 1, 0.5);
            let original =
                distribution_evidence(&compare_distributions(&reference, &current, &params))
                    .total_variation;
            let scaled = distribution_evidence(&compare_distributions(
                &scaled_reference,
                &scaled_current,
                &params,
            ))
            .total_variation;
            prop_assert!((original - scaled).abs() < 1e-12);
        }

        #[test]
        fn equal_exact_per_unit_rates_never_increase(
            reference_operations in 20_u64..1_000,
            current_operations in 20_u64..1_000,
            work_per_operation in 0_u64..10_000,
        ) {
            let reference =
                WorkTotals::new(reference_operations, reference_operations * work_per_operation);
            let current =
                WorkTotals::new(current_operations, current_operations * work_per_operation);
            prop_assert!(matches!(
                compare_per_unit(reference, current, &PerUnitParams::new(20, 20, 0.0, 0.0)),
                PerUnitOutcome::Stable(_)
            ));
        }
    }
}
