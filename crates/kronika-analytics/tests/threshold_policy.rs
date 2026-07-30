//! Public contract tests for validated threshold policies.

use kronika_analytics::threshold::{
    AgePolicy, Direction, FractionPolicy, FreeCapacityPolicy, InputKind, InvalidPolicy, Policy,
    RatioWithFloorPolicy, ScalarPolicy, ZeroDisposition,
};
use kronika_analytics::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};
use proptest as _;
use sha2 as _;

const fn boundary(operator: Comparison, value: f64) -> Boundary {
    Boundary { operator, value }
}

#[expect(clippy::panic, reason = "a non-verdict must fail the contract test")]
fn level(classified: Classified) -> Level {
    match classified {
        Classified::Verdict(verdict) => verdict.level,
        Classified::NotClassified(reason) => {
            panic!("expected threshold verdict, got {reason:?}")
        }
    }
}

fn scalar(
    direction: Direction,
    warning: Option<Boundary>,
    critical: Option<Boundary>,
    zero: ZeroDisposition,
) -> Policy {
    Policy::Scalar(
        ScalarPolicy::new(direction, warning, critical, zero).expect("valid test policy"),
    )
}

fn scalar_policy(
    direction: Direction,
    warning: Option<Boundary>,
    critical: Option<Boundary>,
) -> ScalarPolicy {
    ScalarPolicy::new(direction, warning, critical, ZeroDisposition::Classify)
        .expect("valid test policy")
}

#[test]
fn scalar_boundaries_preserve_strictness_and_critical_priority() {
    let policy = scalar(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::AtLeast, 50.0)),
        Some(boundary(Comparison::AtLeast, 90.0)),
        ZeroDisposition::Classify,
    );

    for (value, expected) in [
        (0.0, Level::Ok),
        (49.999, Level::Ok),
        (50.0, Level::Warning),
        (89.999, Level::Warning),
        (90.0, Level::Critical),
    ] {
        assert_eq!(level(policy.classify(MetricInput::Scalar(value))), expected);
    }
}

#[test]
fn strict_boundaries_do_not_fire_on_equality() {
    let higher = scalar(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::Above, 0.0)),
        Some(boundary(Comparison::Above, 4.0)),
        ZeroDisposition::Classify,
    );
    assert_eq!(level(higher.classify(MetricInput::Scalar(0.0))), Level::Ok);
    assert_eq!(
        level(higher.classify(MetricInput::Scalar(f64::EPSILON))),
        Level::Warning
    );
    assert_eq!(
        level(higher.classify(MetricInput::Scalar(4.0))),
        Level::Warning
    );

    let lower = scalar(
        Direction::LowerIsWorse,
        Some(boundary(Comparison::Below, 30.0)),
        Some(boundary(Comparison::Below, 10.0)),
        ZeroDisposition::Classify,
    );
    assert_eq!(level(lower.classify(MetricInput::Scalar(30.0))), Level::Ok);
    assert_eq!(
        level(lower.classify(MetricInput::Scalar(10.0))),
        Level::Warning
    );
    assert_eq!(
        level(lower.classify(MetricInput::Scalar(9.999))),
        Level::Critical
    );

    let inclusive_lower = scalar(
        Direction::LowerIsWorse,
        Some(boundary(Comparison::AtMost, 30.0)),
        Some(boundary(Comparison::AtMost, 10.0)),
        ZeroDisposition::Classify,
    );
    assert_eq!(
        level(inclusive_lower.classify(MetricInput::Scalar(30.0))),
        Level::Warning
    );
    assert_eq!(
        level(inclusive_lower.classify(MetricInput::Scalar(10.0))),
        Level::Critical
    );
}

#[test]
fn zero_disposition_and_single_boundary_policies_are_explicit() {
    let only_warning = scalar(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::Above, 0.0)),
        None,
        ZeroDisposition::Inactive,
    );
    assert_eq!(
        level(only_warning.classify(MetricInput::Scalar(-0.0))),
        Level::Inactive
    );
    assert_eq!(
        level(only_warning.classify(MetricInput::Scalar(1.0))),
        Level::Warning
    );

    let only_critical = scalar(
        Direction::HigherIsWorse,
        None,
        Some(boundary(Comparison::Above, 0.0)),
        ZeroDisposition::Inactive,
    );
    assert_eq!(
        level(only_critical.classify(MetricInput::Scalar(1.0))),
        Level::Critical
    );
}

#[test]
fn invalid_scalar_policies_are_rejected_exactly() {
    assert_eq!(
        ScalarPolicy::new(
            Direction::HigherIsWorse,
            None,
            None,
            ZeroDisposition::Classify,
        ),
        Err(InvalidPolicy::NoBoundary)
    );
    assert_eq!(
        ScalarPolicy::new(
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Below, 10.0)),
            None,
            ZeroDisposition::Classify,
        ),
        Err(InvalidPolicy::DirectionMismatch)
    );
    assert_eq!(
        ScalarPolicy::new(
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 90.0)),
            Some(boundary(Comparison::AtLeast, 50.0)),
            ZeroDisposition::Classify,
        ),
        Err(InvalidPolicy::BoundaryOrder)
    );
    assert_eq!(
        ScalarPolicy::new(
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, f64::NAN)),
            None,
            ZeroDisposition::Classify,
        ),
        Err(InvalidPolicy::NonFiniteBoundary)
    );
    assert_eq!(
        ScalarPolicy::new(
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, -1.0)),
            None,
            ZeroDisposition::Classify,
        ),
        Err(InvalidPolicy::NegativeBoundary)
    );
}

#[test]
fn scalar_input_failures_keep_exact_reasons() {
    let policy = scalar(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::Above, 1.0)),
        None,
        ZeroDisposition::Classify,
    );

    for (input, reason) in [
        (MetricInput::Missing, NotClassifiedReason::Missing),
        (
            MetricInput::NotApplicable,
            NotClassifiedReason::NotApplicable,
        ),
        (
            MetricInput::Scalar(f64::INFINITY),
            NotClassifiedReason::NonFinite,
        ),
        (MetricInput::Scalar(-1.0), NotClassifiedReason::OutOfDomain),
        (
            MetricInput::Fraction {
                numerator: 1.0,
                denominator: 1.0,
            },
            NotClassifiedReason::InputShapeMismatch,
        ),
    ] {
        assert_eq!(policy.classify(input), Classified::NotClassified(reason),);
    }
}

#[test]
fn fraction_policy_retains_operands_and_rejects_invalid_denominators() {
    let policy = Policy::Fraction(FractionPolicy::new(scalar_policy(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::Above, 1.0)),
        Some(boundary(Comparison::Above, 2.0)),
    )));

    assert_eq!(policy.input_kind(), InputKind::Fraction);
    assert_eq!(
        policy.classify(MetricInput::Fraction {
            numerator: 6.0,
            denominator: 2.0,
        }),
        Classified::Verdict(Verdict {
            level: Level::Critical,
            boundary: Some(boundary(Comparison::Above, 2.0)),
            evidence: Evidence::Fraction {
                numerator: 6.0,
                denominator: 2.0,
                value: 3.0,
            },
        })
    );
    for denominator in [0.0, -1.0] {
        assert_eq!(
            policy.classify(MetricInput::Fraction {
                numerator: 1.0,
                denominator,
            }),
            Classified::NotClassified(NotClassifiedReason::InvalidDenominator)
        );
    }
    assert_eq!(
        policy.classify(MetricInput::Fraction {
            numerator: -1.0,
            denominator: 1.0,
        }),
        Classified::NotClassified(NotClassifiedReason::OutOfDomain)
    );
}

#[test]
fn ratio_floor_must_be_crossed_before_ratio_boundaries_apply() {
    let floor = boundary(Comparison::Above, 10_000.0);
    let policy = Policy::RatioWithFloor(
        RatioWithFloorPolicy::new(
            scalar_policy(
                Direction::HigherIsWorse,
                Some(boundary(Comparison::AtLeast, 0.10)),
                Some(boundary(Comparison::AtLeast, 0.20)),
            ),
            floor,
        )
        .expect("valid ratio-with-floor policy"),
    );

    assert_eq!(policy.input_kind(), InputKind::RatioWithFloor);
    assert_eq!(
        level(policy.classify(MetricInput::RatioWithFloor {
            ratio: 0.50,
            count: 10_000.0,
        })),
        Level::Ok
    );
    assert_eq!(
        policy.classify(MetricInput::RatioWithFloor {
            ratio: 0.20,
            count: 10_001.0,
        }),
        Classified::Verdict(Verdict {
            level: Level::Critical,
            boundary: Some(boundary(Comparison::AtLeast, 0.20)),
            evidence: Evidence::RatioWithFloor {
                ratio: 0.20,
                count: 10_001.0,
                floor,
            },
        })
    );
}

#[test]
fn age_policy_applies_gate_before_validating_and_deriving_age() {
    let policy = Policy::AgeGated(
        AgePolicy::new(scalar_policy(
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 21_600.0)),
            Some(boundary(Comparison::Above, 86_400.0)),
        ))
        .expect("valid age policy"),
    );

    assert_eq!(policy.input_kind(), InputKind::Age);
    assert_eq!(
        policy.classify(MetricInput::Age {
            epoch_seconds: f64::NAN,
            now_seconds: f64::NAN,
            gate: false,
        }),
        Classified::NotClassified(NotClassifiedReason::NotApplicable)
    );
    assert_eq!(
        policy.classify(MetricInput::Age {
            epoch_seconds: 101.0,
            now_seconds: 100.0,
            gate: true,
        }),
        Classified::NotClassified(NotClassifiedReason::OutOfDomain)
    );
    assert_eq!(
        policy.classify(MetricInput::Age {
            epoch_seconds: 10_000.0,
            now_seconds: 100_001.0,
            gate: true,
        }),
        Classified::Verdict(Verdict {
            level: Level::Critical,
            boundary: Some(boundary(Comparison::Above, 86_400.0)),
            evidence: Evidence::Age {
                epoch_seconds: 10_000.0,
                now_seconds: 100_001.0,
                age_seconds: 90_001.0,
            },
        })
    );
}

#[test]
fn free_capacity_requires_fraction_and_absolute_ceiling() {
    const GIB: f64 = 1_073_741_824.0;
    let absolute_ceiling = boundary(Comparison::Below, 15.0 * GIB);
    let policy = Policy::FreeCapacity(
        FreeCapacityPolicy::new(
            scalar_policy(
                Direction::LowerIsWorse,
                Some(boundary(Comparison::Below, 0.20)),
                Some(boundary(Comparison::Below, 0.10)),
            ),
            absolute_ceiling,
        )
        .expect("valid free-capacity policy"),
    );

    assert_eq!(policy.input_kind(), InputKind::FreeCapacity);
    assert_eq!(
        level(policy.classify(MetricInput::FreeCapacity {
            available_bytes: 15.0 * GIB,
            total_bytes: 100.0 * GIB,
        })),
        Level::Ok
    );
    assert_eq!(
        level(policy.classify(MetricInput::FreeCapacity {
            available_bytes: 14.0 * GIB,
            total_bytes: 100.0 * GIB,
        })),
        Level::Warning
    );
    assert_eq!(
        policy.classify(MetricInput::FreeCapacity {
            available_bytes: 9.0 * GIB,
            total_bytes: 100.0 * GIB,
        }),
        Classified::Verdict(Verdict {
            level: Level::Critical,
            boundary: Some(boundary(Comparison::Below, 0.10)),
            evidence: Evidence::FreeCapacity {
                available_bytes: 9.0 * GIB,
                total_bytes: 100.0 * GIB,
                available_fraction: 0.09,
                absolute_ceiling_bytes: absolute_ceiling,
            },
        })
    );
}

#[test]
fn composite_policies_report_exact_input_failures() {
    let ratio = Policy::RatioWithFloor(
        RatioWithFloorPolicy::new(
            scalar_policy(
                Direction::HigherIsWorse,
                Some(boundary(Comparison::Above, 0.1)),
                None,
            ),
            boundary(Comparison::Above, 1.0),
        )
        .expect("valid ratio-with-floor policy"),
    );
    for (input, reason) in [
        (
            MetricInput::RatioWithFloor {
                ratio: f64::NAN,
                count: 2.0,
            },
            NotClassifiedReason::NonFinite,
        ),
        (
            MetricInput::RatioWithFloor {
                ratio: -1.0,
                count: 2.0,
            },
            NotClassifiedReason::OutOfDomain,
        ),
        (
            MetricInput::RatioWithFloor {
                ratio: 0.2,
                count: -1.0,
            },
            NotClassifiedReason::OutOfDomain,
        ),
        (
            MetricInput::Scalar(1.0),
            NotClassifiedReason::InputShapeMismatch,
        ),
    ] {
        assert_eq!(ratio.classify(input), Classified::NotClassified(reason));
    }

    let capacity = Policy::FreeCapacity(
        FreeCapacityPolicy::new(
            scalar_policy(
                Direction::LowerIsWorse,
                Some(boundary(Comparison::Below, 0.2)),
                None,
            ),
            boundary(Comparison::Below, 100.0),
        )
        .expect("valid free-capacity policy"),
    );
    for (input, reason) in [
        (
            MetricInput::FreeCapacity {
                available_bytes: f64::INFINITY,
                total_bytes: 100.0,
            },
            NotClassifiedReason::NonFinite,
        ),
        (
            MetricInput::FreeCapacity {
                available_bytes: 1.0,
                total_bytes: 0.0,
            },
            NotClassifiedReason::InvalidDenominator,
        ),
        (
            MetricInput::FreeCapacity {
                available_bytes: 101.0,
                total_bytes: 100.0,
            },
            NotClassifiedReason::OutOfDomain,
        ),
    ] {
        assert_eq!(capacity.classify(input), Classified::NotClassified(reason));
    }
}

#[test]
fn invalid_composite_policies_are_rejected_exactly() {
    let higher = scalar_policy(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::Above, 1.0)),
        None,
    );
    let lower = scalar_policy(
        Direction::LowerIsWorse,
        Some(boundary(Comparison::Below, 1.0)),
        None,
    );

    assert_eq!(
        RatioWithFloorPolicy::new(higher, boundary(Comparison::Below, 1.0)),
        Err(InvalidPolicy::InvalidFloor)
    );
    assert_eq!(
        RatioWithFloorPolicy::new(higher, boundary(Comparison::Above, f64::NAN)),
        Err(InvalidPolicy::InvalidFloor)
    );
    assert_eq!(AgePolicy::new(lower), Err(InvalidPolicy::DirectionMismatch));
    assert_eq!(
        FreeCapacityPolicy::new(higher, boundary(Comparison::Below, 1.0)),
        Err(InvalidPolicy::DirectionMismatch)
    );
    assert_eq!(
        FreeCapacityPolicy::new(lower, boundary(Comparison::Above, 1.0)),
        Err(InvalidPolicy::InvalidCapacityCeiling)
    );
}
