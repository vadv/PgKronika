use std::{cmp::Ordering, hash::Hash};

#[derive(Debug, Clone, Copy)]
pub(crate) struct FiniteValue(f64);

impl FiniteValue {
    pub(crate) fn new(value: f64) -> Option<Self> {
        value
            .is_finite()
            .then_some(Self(if value == 0.0 { 0.0 } else { value }))
    }

    pub(crate) const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for FiniteValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for FiniteValue {}

impl PartialOrd for FiniteValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FiniteValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for FiniteValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GaugeUnit {
    Count,
    Bytes,
    Kibibytes,
    Microseconds,
    Milliseconds,
    Ratio,
    BytesPerSecond,
    MicrosecondsPerSecond,
    MillisecondsPerRead,
    MillisecondsPerCall,
    MillisecondsPerOperation,
    RowsPerCall,
    BlocksPerCall,
}

impl GaugeUnit {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Bytes => "bytes",
            Self::Kibibytes => "KiB",
            Self::Microseconds => "microseconds",
            Self::Milliseconds => "milliseconds",
            Self::Ratio => "ratio",
            Self::BytesPerSecond => "bytes_per_second",
            Self::MicrosecondsPerSecond => "microseconds_per_second",
            Self::MillisecondsPerRead => "milliseconds_per_read",
            Self::MillisecondsPerCall => "milliseconds_per_call",
            Self::MillisecondsPerOperation => "milliseconds_per_operation",
            Self::RowsPerCall => "rows_per_call",
            Self::BlocksPerCall => "blocks_per_call",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ThresholdKind {
    AtLeast,
    Above,
    Below,
}

impl ThresholdKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::AtLeast => "at_least",
            Self::Above => "above",
            Self::Below => "below",
        }
    }
}
