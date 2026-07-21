use std::sync::Arc;

use super::super::model::IdentityValue;
use super::coverage::SourceWindow;
use super::value::{FiniteValue, GaugeUnit, ThresholdKind};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GaugeMeasurement {
    Value {
        operand: &'static str,
        value: FiniteValue,
    },
    Ratio {
        numerator_name: &'static str,
        numerator: FiniteValue,
        numerator_unit: GaugeUnit,
        denominator_name: &'static str,
        denominator: FiniteValue,
        denominator_unit: GaugeUnit,
    },
    Trend {
        operand: &'static str,
        first: FiniteValue,
        last: FiniteValue,
        elapsed_us: u64,
        operand_unit: GaugeUnit,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GaugeRatio {
    numerator_name: &'static str,
    numerator: f64,
    numerator_unit: GaugeUnit,
    denominator_name: &'static str,
    denominator: f64,
    denominator_unit: GaugeUnit,
    result_unit: GaugeUnit,
}

pub(crate) struct GaugeTrendInput {
    pub operand: &'static str,
    pub first: f64,
    pub last: f64,
    pub operand_unit: GaugeUnit,
    pub threshold_per_second: f64,
    pub threshold_kind: ThresholdKind,
    pub first_at_us: i64,
    pub last_at_us: i64,
    pub samples: usize,
    pub source_window: SourceWindow,
    pub entity: GaugeEntity,
}

pub(crate) struct GaugeValueInput {
    pub operand: &'static str,
    pub value: f64,
    pub unit: GaugeUnit,
    pub threshold: f64,
    pub threshold_kind: ThresholdKind,
    pub observed_at_us: i64,
    pub samples: usize,
    pub source_window: SourceWindow,
    pub entity: GaugeEntity,
}

impl GaugeRatio {
    pub(crate) const fn new(
        numerator_name: &'static str,
        numerator: f64,
        denominator_name: &'static str,
        denominator: f64,
        operand_unit: GaugeUnit,
    ) -> Self {
        Self {
            numerator_name,
            numerator,
            numerator_unit: operand_unit,
            denominator_name,
            denominator,
            denominator_unit: operand_unit,
            result_unit: GaugeUnit::Ratio,
        }
    }

    pub(crate) const fn with_units(
        numerator_name: &'static str,
        numerator: f64,
        numerator_unit: GaugeUnit,
        denominator_name: &'static str,
        denominator: f64,
        denominator_unit: GaugeUnit,
        result_unit: GaugeUnit,
    ) -> Self {
        Self {
            numerator_name,
            numerator,
            numerator_unit,
            denominator_name,
            denominator,
            denominator_unit,
            result_unit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GaugeEntity {
    section: &'static str,
    identity: Arc<[IdentityValue]>,
}

impl GaugeEntity {
    pub(crate) const fn new(section: &'static str, identity: Arc<[IdentityValue]>) -> Self {
        Self { section, identity }
    }

    pub(crate) const fn section(&self) -> &'static str {
        self.section
    }

    pub(crate) fn identity(&self) -> &[IdentityValue] {
        &self.identity
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GaugeEvidence {
    measurement: GaugeMeasurement,
    unit: GaugeUnit,
    threshold: FiniteValue,
    threshold_kind: ThresholdKind,
    observed_at_us: i64,
    samples: u64,
    source_window: SourceWindow,
    entity: GaugeEntity,
}

impl GaugeEvidence {
    pub(crate) fn value(input: GaugeValueInput) -> Option<Self> {
        let GaugeValueInput {
            operand,
            value,
            unit,
            threshold,
            threshold_kind,
            observed_at_us,
            samples,
            source_window,
            entity,
        } = input;
        let samples = u64::try_from(samples).ok()?;
        (samples > 0 && !operand.is_empty() && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Value {
                operand,
                value: FiniteValue::new(value)?,
            },
            unit,
            threshold: FiniteValue::new(threshold)?,
            threshold_kind,
            observed_at_us,
            samples,
            source_window,
            entity,
        })
    }

    pub(crate) fn ratio(
        ratio: GaugeRatio,
        threshold: f64,
        threshold_kind: ThresholdKind,
        observed_at_us: i64,
        samples: usize,
        source_window: SourceWindow,
        entity: GaugeEntity,
    ) -> Option<Self> {
        (ratio.denominator > 0.0
            && !ratio.numerator_name.is_empty()
            && !ratio.denominator_name.is_empty())
        .then_some(())?;
        FiniteValue::new(ratio.numerator / ratio.denominator)?;
        let samples = u64::try_from(samples).ok()?;
        (samples > 0 && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Ratio {
                numerator_name: ratio.numerator_name,
                numerator: FiniteValue::new(ratio.numerator)?,
                numerator_unit: ratio.numerator_unit,
                denominator_name: ratio.denominator_name,
                denominator: FiniteValue::new(ratio.denominator)?,
                denominator_unit: ratio.denominator_unit,
            },
            unit: ratio.result_unit,
            threshold: FiniteValue::new(threshold)?,
            threshold_kind,
            observed_at_us,
            samples,
            source_window,
            entity,
        })
    }

    pub(crate) fn trend(input: GaugeTrendInput) -> Option<Self> {
        let GaugeTrendInput {
            operand,
            first,
            last,
            operand_unit,
            threshold_per_second,
            threshold_kind,
            first_at_us,
            last_at_us,
            samples,
            source_window,
            entity,
        } = input;
        let elapsed_us = u64::try_from(last_at_us.checked_sub(first_at_us)?).ok()?;
        (elapsed_us > 0).then_some(())?;
        let elapsed_seconds = std::time::Duration::from_micros(elapsed_us).as_secs_f64();
        FiniteValue::new((last - first) / elapsed_seconds)?;
        let samples = u64::try_from(samples).ok()?;
        (samples >= 2 && !operand.is_empty() && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Trend {
                operand,
                first: FiniteValue::new(first)?,
                last: FiniteValue::new(last)?,
                elapsed_us,
                operand_unit,
            },
            unit: GaugeUnit::BytesPerSecond,
            threshold: FiniteValue::new(threshold_per_second)?,
            threshold_kind,
            observed_at_us: last_at_us,
            samples,
            source_window,
            entity,
        })
    }

    pub(crate) const fn measurement(&self) -> &GaugeMeasurement {
        &self.measurement
    }

    pub(crate) const fn unit(&self) -> GaugeUnit {
        self.unit
    }

    pub(crate) const fn threshold(&self) -> FiniteValue {
        self.threshold
    }

    pub(crate) const fn threshold_kind(&self) -> ThresholdKind {
        self.threshold_kind
    }

    pub(crate) const fn observed_at_us(&self) -> i64 {
        self.observed_at_us
    }

    pub(crate) const fn samples(&self) -> u64 {
        self.samples
    }

    pub(crate) const fn source_window(&self) -> SourceWindow {
        self.source_window
    }

    pub(crate) const fn entity(&self) -> &GaugeEntity {
        &self.entity
    }

    pub(super) fn operand_name_bytes(&self) -> u64 {
        let length = match &self.measurement {
            GaugeMeasurement::Value { operand, .. } | GaugeMeasurement::Trend { operand, .. } => {
                operand.len()
            }
            GaugeMeasurement::Ratio {
                numerator_name,
                denominator_name,
                ..
            } => numerator_name.len().saturating_add(denominator_name.len()),
        };
        u64::try_from(length).unwrap_or(u64::MAX)
    }
}
