//! Validated, allocation-free policies for absolute threshold classification.

use super::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};

/// Direction in which a scalar becomes less healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Larger values cross warning and critical boundaries.
    HigherIsWorse,
    /// Smaller values cross warning and critical boundaries.
    LowerIsWorse,
}

/// How an exact numeric zero is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroDisposition {
    /// Apply ordinary boundaries and otherwise return [`Level::Ok`].
    Classify,
    /// Return [`Level::Inactive`] before testing boundaries.
    Inactive,
}

/// Operand shape required by a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// One scalar value.
    Scalar,
    /// A numerator and denominator.
    Fraction,
    /// A ratio and companion absolute count.
    RatioWithFloor,
    /// An epoch, caller-provided current time, and applicability gate.
    Age,
    /// Available and total capacity.
    FreeCapacity,
}

/// Invalid built-in or caller-created policy configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidPolicy {
    /// Neither a warning nor a critical boundary was provided.
    NoBoundary,
    /// A boundary is `NaN` or infinite.
    NonFiniteBoundary,
    /// A boundary is negative for a non-negative metric domain.
    NegativeBoundary,
    /// A comparison operator points opposite to the policy direction.
    DirectionMismatch,
    /// Warning and critical values are ordered inconsistently.
    BoundaryOrder,
    /// A ratio companion-count floor is invalid.
    InvalidFloor,
    /// A free-capacity absolute ceiling is invalid.
    InvalidCapacityCeiling,
}

/// Validated boundaries and zero behavior for one non-negative scalar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarPolicy {
    direction: Direction,
    warning: Option<Boundary>,
    critical: Option<Boundary>,
    zero: ZeroDisposition,
}

impl ScalarPolicy {
    /// Validate scalar boundaries for a non-negative metric.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPolicy`] when a boundary is missing, non-finite,
    /// negative, points opposite to `direction`, or is ordered inconsistently.
    pub const fn new(
        direction: Direction,
        warning: Option<Boundary>,
        critical: Option<Boundary>,
        zero: ZeroDisposition,
    ) -> Result<Self, InvalidPolicy> {
        if warning.is_none() && critical.is_none() {
            return Err(InvalidPolicy::NoBoundary);
        }
        if let Some(boundary) = warning
            && let Err(error) = validate_boundary(boundary, direction)
        {
            return Err(error);
        }
        if let Some(boundary) = critical
            && let Err(error) = validate_boundary(boundary, direction)
        {
            return Err(error);
        }
        if let (Some(warning), Some(critical)) = (warning, critical) {
            match direction {
                Direction::HigherIsWorse if warning.value > critical.value => {
                    return Err(InvalidPolicy::BoundaryOrder);
                }
                Direction::LowerIsWorse if warning.value < critical.value => {
                    return Err(InvalidPolicy::BoundaryOrder);
                }
                Direction::HigherIsWorse | Direction::LowerIsWorse => {}
            }
        }
        Ok(Self {
            direction,
            warning,
            critical,
            zero,
        })
    }

    /// Direction in which values become less healthy.
    #[must_use]
    pub const fn direction(self) -> Direction {
        self.direction
    }

    /// Warning boundary, when the policy has one.
    #[must_use]
    pub const fn warning(self) -> Option<Boundary> {
        self.warning
    }

    /// Critical boundary, when the policy has one.
    #[must_use]
    pub const fn critical(self) -> Option<Boundary> {
        self.critical
    }

    /// Exact-zero behavior.
    #[must_use]
    pub const fn zero_disposition(self) -> ZeroDisposition {
        self.zero
    }
}

/// A validated policy over a derived numerator-to-denominator fraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FractionPolicy {
    scalar: ScalarPolicy,
}

impl FractionPolicy {
    /// Wrap a validated scalar policy for the derived fraction.
    #[must_use]
    pub const fn new(scalar: ScalarPolicy) -> Self {
        Self { scalar }
    }

    /// Policy applied to the derived fraction.
    #[must_use]
    pub const fn scalar(self) -> ScalarPolicy {
        self.scalar
    }
}

/// A validated ratio policy gated by a companion absolute-count floor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RatioWithFloorPolicy {
    ratio: ScalarPolicy,
    floor: Boundary,
}

impl RatioWithFloorPolicy {
    /// Validate a ratio policy and its non-negative count floor.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPolicy::InvalidFloor`] unless `floor` is finite,
    /// non-negative, and uses [`Comparison::Above`] or
    /// [`Comparison::AtLeast`].
    pub const fn new(ratio: ScalarPolicy, floor: Boundary) -> Result<Self, InvalidPolicy> {
        if !floor.value.is_finite()
            || floor.value < 0.0
            || !matches!(floor.operator, Comparison::Above | Comparison::AtLeast)
        {
            return Err(InvalidPolicy::InvalidFloor);
        }
        Ok(Self { ratio, floor })
    }

    /// Policy applied to the ratio after the floor is crossed.
    #[must_use]
    pub const fn ratio(self) -> ScalarPolicy {
        self.ratio
    }

    /// Absolute count floor gating the ratio.
    #[must_use]
    pub const fn floor(self) -> Boundary {
        self.floor
    }
}

/// A validated policy over an age derived from two caller-provided timestamps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgePolicy {
    age: ScalarPolicy,
}

impl AgePolicy {
    /// Validate a higher-is-worse age policy.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPolicy::DirectionMismatch`] for a lower-is-worse
    /// scalar policy.
    pub const fn new(age: ScalarPolicy) -> Result<Self, InvalidPolicy> {
        if matches!(age.direction, Direction::LowerIsWorse) {
            return Err(InvalidPolicy::DirectionMismatch);
        }
        Ok(Self { age })
    }

    /// Policy applied to the derived age in seconds.
    #[must_use]
    pub const fn scalar(self) -> ScalarPolicy {
        self.age
    }
}

/// A validated free-capacity policy requiring relative and absolute scarcity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeCapacityPolicy {
    available_fraction: ScalarPolicy,
    absolute_ceiling_bytes: Boundary,
}

impl FreeCapacityPolicy {
    /// Validate a lower-is-worse fraction policy and absolute byte ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPolicy::DirectionMismatch`] when the fraction is not
    /// lower-is-worse. Returns [`InvalidPolicy::InvalidCapacityCeiling`] unless
    /// the ceiling is finite, non-negative, and uses [`Comparison::Below`] or
    /// [`Comparison::AtMost`].
    pub const fn new(
        available_fraction: ScalarPolicy,
        absolute_ceiling_bytes: Boundary,
    ) -> Result<Self, InvalidPolicy> {
        if matches!(available_fraction.direction, Direction::HigherIsWorse) {
            return Err(InvalidPolicy::DirectionMismatch);
        }
        if !absolute_ceiling_bytes.value.is_finite()
            || absolute_ceiling_bytes.value < 0.0
            || !matches!(
                absolute_ceiling_bytes.operator,
                Comparison::Below | Comparison::AtMost
            )
        {
            return Err(InvalidPolicy::InvalidCapacityCeiling);
        }
        Ok(Self {
            available_fraction,
            absolute_ceiling_bytes,
        })
    }

    /// Policy applied to available bytes divided by total bytes.
    #[must_use]
    pub const fn available_fraction(self) -> ScalarPolicy {
        self.available_fraction
    }

    /// Absolute available-byte ceiling required for warning or critical.
    #[must_use]
    pub const fn absolute_ceiling_bytes(self) -> Boundary {
        self.absolute_ceiling_bytes
    }
}

/// One validated absolute-threshold policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Policy {
    /// A policy over one non-negative scalar.
    Scalar(ScalarPolicy),
    /// A scalar policy applied to a derived numerator-to-denominator fraction.
    Fraction(FractionPolicy),
    /// A ratio policy gated by an absolute companion-count floor.
    RatioWithFloor(RatioWithFloorPolicy),
    /// An age policy gated by caller-provided applicability.
    AgeGated(AgePolicy),
    /// A free-capacity policy requiring relative and absolute scarcity.
    FreeCapacity(FreeCapacityPolicy),
}

impl Policy {
    /// Required operand shape.
    #[must_use]
    pub const fn input_kind(&self) -> InputKind {
        match self {
            Self::Scalar(_) => InputKind::Scalar,
            Self::Fraction(_) => InputKind::Fraction,
            Self::RatioWithFloor(_) => InputKind::RatioWithFloor,
            Self::AgeGated(_) => InputKind::Age,
            Self::FreeCapacity(_) => InputKind::FreeCapacity,
        }
    }

    /// Classify a fixed-size input without I/O, clock access, or allocation.
    #[must_use]
    pub fn classify(&self, input: MetricInput) -> Classified {
        match input {
            MetricInput::Missing => Classified::NotClassified(NotClassifiedReason::Missing),
            MetricInput::NotApplicable => {
                Classified::NotClassified(NotClassifiedReason::NotApplicable)
            }
            MetricInput::Scalar(value) => match self {
                Self::Scalar(policy) => classify_scalar(*policy, value),
                Self::Fraction(_)
                | Self::RatioWithFloor(_)
                | Self::AgeGated(_)
                | Self::FreeCapacity(_) => input_shape_mismatch(),
            },
            MetricInput::Fraction {
                numerator,
                denominator,
            } => match self {
                Self::Fraction(policy) => classify_fraction(*policy, numerator, denominator),
                Self::Scalar(_)
                | Self::RatioWithFloor(_)
                | Self::AgeGated(_)
                | Self::FreeCapacity(_) => input_shape_mismatch(),
            },
            MetricInput::RatioWithFloor { ratio, count } => match self {
                Self::RatioWithFloor(policy) => classify_ratio_with_floor(*policy, ratio, count),
                Self::Scalar(_) | Self::Fraction(_) | Self::AgeGated(_) | Self::FreeCapacity(_) => {
                    input_shape_mismatch()
                }
            },
            MetricInput::Age {
                epoch_seconds,
                now_seconds,
                gate,
            } => match self {
                Self::AgeGated(policy) => classify_age(*policy, epoch_seconds, now_seconds, gate),
                Self::Scalar(_)
                | Self::Fraction(_)
                | Self::RatioWithFloor(_)
                | Self::FreeCapacity(_) => input_shape_mismatch(),
            },
            MetricInput::FreeCapacity {
                available_bytes,
                total_bytes,
            } => match self {
                Self::FreeCapacity(policy) => {
                    classify_free_capacity(*policy, available_bytes, total_bytes)
                }
                Self::Scalar(_)
                | Self::Fraction(_)
                | Self::RatioWithFloor(_)
                | Self::AgeGated(_) => input_shape_mismatch(),
            },
        }
    }
}

const fn validate_boundary(boundary: Boundary, direction: Direction) -> Result<(), InvalidPolicy> {
    if !boundary.value.is_finite() {
        return Err(InvalidPolicy::NonFiniteBoundary);
    }
    if boundary.value < 0.0 {
        return Err(InvalidPolicy::NegativeBoundary);
    }
    match (direction, boundary.operator) {
        (Direction::HigherIsWorse, Comparison::Above | Comparison::AtLeast)
        | (Direction::LowerIsWorse, Comparison::Below | Comparison::AtMost) => Ok(()),
        _ => Err(InvalidPolicy::DirectionMismatch),
    }
}

fn classify_scalar(policy: ScalarPolicy, value: f64) -> Classified {
    if !value.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    if value < 0.0 {
        return Classified::NotClassified(NotClassifiedReason::OutOfDomain);
    }
    let observed = if value == 0.0 { 0.0 } else { value };
    classify_with_evidence(policy, observed, Evidence::Scalar { observed })
}

fn classify_fraction(policy: FractionPolicy, numerator: f64, denominator: f64) -> Classified {
    if !numerator.is_finite() || !denominator.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    if denominator <= 0.0 {
        return Classified::NotClassified(NotClassifiedReason::InvalidDenominator);
    }
    if numerator < 0.0 {
        return Classified::NotClassified(NotClassifiedReason::OutOfDomain);
    }
    let numerator = normalize_zero(numerator);
    let value = numerator / denominator;
    if !value.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    classify_with_evidence(
        policy.scalar,
        value,
        Evidence::Fraction {
            numerator,
            denominator,
            value,
        },
    )
}

fn classify_ratio_with_floor(policy: RatioWithFloorPolicy, ratio: f64, count: f64) -> Classified {
    if !ratio.is_finite() || !count.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    if ratio < 0.0 || count < 0.0 {
        return Classified::NotClassified(NotClassifiedReason::OutOfDomain);
    }
    let ratio = normalize_zero(ratio);
    let count = normalize_zero(count);
    let evidence = Evidence::RatioWithFloor {
        ratio,
        count,
        floor: policy.floor,
    };
    if !policy.floor.matches(count) {
        return verdict(Level::Ok, None, evidence);
    }
    classify_with_evidence(policy.ratio, ratio, evidence)
}

fn classify_age(policy: AgePolicy, epoch_seconds: f64, now_seconds: f64, gate: bool) -> Classified {
    if !gate {
        return Classified::NotClassified(NotClassifiedReason::NotApplicable);
    }
    if !epoch_seconds.is_finite() || !now_seconds.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    if epoch_seconds < 0.0 || now_seconds < 0.0 || epoch_seconds > now_seconds {
        return Classified::NotClassified(NotClassifiedReason::OutOfDomain);
    }
    let epoch_seconds = normalize_zero(epoch_seconds);
    let now_seconds = normalize_zero(now_seconds);
    let age_seconds = now_seconds - epoch_seconds;
    classify_with_evidence(
        policy.age,
        age_seconds,
        Evidence::Age {
            epoch_seconds,
            now_seconds,
            age_seconds,
        },
    )
}

fn classify_free_capacity(
    policy: FreeCapacityPolicy,
    available_bytes: f64,
    total_bytes: f64,
) -> Classified {
    if !available_bytes.is_finite() || !total_bytes.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    if total_bytes <= 0.0 {
        return Classified::NotClassified(NotClassifiedReason::InvalidDenominator);
    }
    if available_bytes < 0.0 || available_bytes > total_bytes {
        return Classified::NotClassified(NotClassifiedReason::OutOfDomain);
    }
    let available_bytes = normalize_zero(available_bytes);
    let available_fraction = available_bytes / total_bytes;
    if !available_fraction.is_finite() {
        return Classified::NotClassified(NotClassifiedReason::NonFinite);
    }
    let evidence = Evidence::FreeCapacity {
        available_bytes,
        total_bytes,
        available_fraction,
        absolute_ceiling_bytes: policy.absolute_ceiling_bytes,
    };
    if available_fraction == 0.0 && policy.available_fraction.zero == ZeroDisposition::Inactive {
        return verdict(Level::Inactive, None, evidence);
    }
    if !policy.absolute_ceiling_bytes.matches(available_bytes) {
        return verdict(Level::Ok, None, evidence);
    }
    let (level, boundary) =
        classify_validated_scalar(policy.available_fraction, available_fraction);
    verdict(level, boundary, evidence)
}

fn classify_with_evidence(policy: ScalarPolicy, value: f64, evidence: Evidence) -> Classified {
    if value == 0.0 && policy.zero == ZeroDisposition::Inactive {
        return verdict(Level::Inactive, None, evidence);
    }
    let (level, boundary) = classify_validated_scalar(policy, value);
    verdict(level, boundary, evidence)
}

const fn classify_validated_scalar(policy: ScalarPolicy, value: f64) -> (Level, Option<Boundary>) {
    if let Some(boundary) = policy.critical
        && boundary.matches(value)
    {
        return (Level::Critical, Some(boundary));
    }
    if let Some(boundary) = policy.warning
        && boundary.matches(value)
    {
        return (Level::Warning, Some(boundary));
    }
    (Level::Ok, None)
}

const fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

const fn input_shape_mismatch() -> Classified {
    Classified::NotClassified(NotClassifiedReason::InputShapeMismatch)
}

const fn verdict(level: Level, boundary: Option<Boundary>, evidence: Evidence) -> Classified {
    Classified::Verdict(Verdict {
        level,
        boundary,
        evidence,
    })
}

impl Boundary {
    const fn matches(self, observed: f64) -> bool {
        match self.operator {
            Comparison::Above => observed > self.value,
            Comparison::AtLeast => observed >= self.value,
            Comparison::Below => observed < self.value,
            Comparison::AtMost => observed <= self.value,
        }
    }
}
