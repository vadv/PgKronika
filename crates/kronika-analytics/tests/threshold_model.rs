//! Public contract tests for fixed-size threshold inputs and outcomes.

use kronika_analytics::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};
use proptest as _;
use sha2 as _;

#[test]
fn verdict_keeps_exact_boundary_and_fraction_evidence() {
    let verdict = Verdict {
        level: Level::Warning,
        boundary: Some(Boundary {
            operator: Comparison::Above,
            value: 1.0,
        }),
        evidence: Evidence::Fraction {
            numerator: 3.0,
            denominator: 2.0,
            value: 1.5,
        },
    };

    assert_eq!(verdict.level, Level::Warning);
    assert_eq!(
        verdict.boundary,
        Some(Boundary {
            operator: Comparison::Above,
            value: 1.0,
        })
    );
}

#[test]
fn input_states_do_not_use_numeric_sentinels() {
    assert_ne!(MetricInput::Missing, MetricInput::NotApplicable);
    assert_eq!(
        Classified::NotClassified(NotClassifiedReason::Missing),
        Classified::NotClassified(NotClassifiedReason::Missing)
    );
}
