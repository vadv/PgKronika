//! Fixed-size inputs and explainable outcomes for absolute thresholds.

/// Health level assigned to one applicable observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The measured activity or event count is exactly zero where zero means inactivity.
    Inactive,
    /// The observation crossed no configured warning or critical boundary.
    Ok,
    /// The observation crossed the warning boundary but not the critical boundary.
    Warning,
    /// The observation crossed the critical boundary.
    Critical,
}

/// Result of applying one threshold policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Classified {
    /// The input was applicable and produced an explainable level.
    Verdict(Verdict),
    /// The input could not be classified for the exact stated reason.
    NotClassified(NotClassifiedReason),
}

/// Explainable result for one applicable observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    /// Assigned health level.
    pub level: Level,
    /// Boundary that selected warning or critical; absent for ok and inactive.
    pub boundary: Option<Boundary>,
    /// Fixed-size operands and derived value used by the policy.
    pub evidence: Evidence,
}

/// One numeric threshold and its exact comparison operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Boundary {
    /// Comparison applied to an observed or derived value.
    pub operator: Comparison,
    /// Finite, non-negative threshold value.
    pub value: f64,
}

/// Exact comparison used by a boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// The observation must be strictly greater than the boundary.
    Above,
    /// The observation may equal or exceed the boundary.
    AtLeast,
    /// The observation must be strictly less than the boundary.
    Below,
    /// The observation may equal or fall below the boundary.
    AtMost,
}

/// Why a policy produced no health level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotClassifiedReason {
    /// The caller has no observation for the metric.
    Missing,
    /// At least one supplied or derived number is `NaN` or infinite.
    NonFinite,
    /// A finite input violates the policy's numeric domain.
    OutOfDomain,
    /// A fraction or capacity denominator is zero or negative.
    InvalidDenominator,
    /// The metric does not apply in the caller-provided context.
    NotApplicable,
    /// The supplied input shape does not match the metric policy.
    InputShapeMismatch,
}

/// Fixed-size operands accepted by built-in threshold policies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricInput {
    /// No observation is available.
    Missing,
    /// The caller proved that the metric does not apply.
    NotApplicable,
    /// One already-normalized scalar.
    Scalar(f64),
    /// A numerator divided by a strictly positive denominator.
    Fraction {
        /// Non-negative numerator.
        numerator: f64,
        /// Strictly positive denominator.
        denominator: f64,
    },
    /// A ratio gated by an absolute count floor.
    RatioWithFloor {
        /// Non-negative ratio where `1.0` means 100 percent.
        ratio: f64,
        /// Non-negative companion count.
        count: f64,
    },
    /// Age derived from a caller-provided epoch and current time.
    Age {
        /// Event timestamp in seconds.
        epoch_seconds: f64,
        /// Caller-provided current time in seconds.
        now_seconds: f64,
        /// Whether the surrounding metric state makes the age applicable.
        gate: bool,
    },
    /// Available and total capacity in bytes.
    FreeCapacity {
        /// Non-negative available bytes.
        available_bytes: f64,
        /// Strictly positive total bytes.
        total_bytes: f64,
    },
}

/// Fixed-size operands and derived value retained in an explainable verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Evidence {
    /// One scalar observation.
    Scalar {
        /// Validated observation.
        observed: f64,
    },
    /// Fraction operands and their quotient.
    Fraction {
        /// Validated numerator.
        numerator: f64,
        /// Validated denominator.
        denominator: f64,
        /// Validated quotient.
        value: f64,
    },
    /// Ratio and absolute count evaluated against a floor.
    RatioWithFloor {
        /// Validated ratio.
        ratio: f64,
        /// Validated companion count.
        count: f64,
        /// Absolute floor applied to the count.
        floor: Boundary,
    },
    /// Epoch operands and their derived age.
    Age {
        /// Validated event timestamp.
        epoch_seconds: f64,
        /// Validated caller-provided current time.
        now_seconds: f64,
        /// Non-negative derived age.
        age_seconds: f64,
    },
    /// Capacity operands, their quotient, and the absolute free-space ceiling.
    FreeCapacity {
        /// Validated available bytes.
        available_bytes: f64,
        /// Validated total bytes.
        total_bytes: f64,
        /// Available bytes divided by total bytes.
        available_fraction: f64,
        /// Absolute available-byte ceiling required by the policy.
        absolute_ceiling_bytes: Boundary,
    },
}
