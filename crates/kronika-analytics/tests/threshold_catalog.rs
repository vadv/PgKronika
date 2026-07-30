//! Public contract tests for the built-in absolute-threshold catalog.

use kronika_analytics::threshold::{Calibration, MetricId, catalog, catalog_entry, classify};
use kronika_analytics::{Classified, Level, MetricInput};
use proptest as _;
use sha2 as _;

#[expect(clippy::panic, reason = "a non-verdict must fail the contract test")]
fn level(classified: Classified) -> Level {
    match classified {
        Classified::Verdict(verdict) => verdict.level,
        Classified::NotClassified(reason) => {
            panic!("expected threshold verdict, got {reason:?}")
        }
    }
}

#[test]
fn first_domain_batch_is_unique_ordered_and_provisional() {
    assert_eq!(catalog().len(), 25);
    assert_eq!(
        catalog().iter().map(|entry| entry.id).collect::<Vec<_>>(),
        MetricId::ALL
    );
    assert!(
        catalog()
            .iter()
            .all(|entry| entry.calibration == Calibration::Provisional)
    );

    let mut codes = catalog()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), catalog().len());

    for (index, id) in MetricId::ALL.iter().copied().enumerate() {
        assert_eq!(catalog_entry(id), &catalog()[index]);
    }
}

#[test]
fn first_domain_boundaries_preserve_the_research_operators() {
    for (id, value, expected) in [
        (MetricId::OsProcessCpuPercent, 50.0, Level::Warning),
        (MetricId::OsProcessCpuPercent, 90.0, Level::Critical),
        (MetricId::OsCpuIdlePercent, 30.0, Level::Ok),
        (MetricId::OsCpuIdlePercent, 29.999, Level::Warning),
        (MetricId::OsPsiIoSomePercent, 10.0, Level::Warning),
        (MetricId::OsPsiIoSomePercent, 40.0, Level::Critical),
    ] {
        assert_eq!(level(classify(id, MetricInput::Scalar(value))), expected);
    }
}

#[test]
fn explicit_inactive_and_critical_only_policies_are_preserved() {
    assert_eq!(
        level(classify(
            MetricId::OsProcessVirtualSwapKib,
            MetricInput::Scalar(0.0),
        )),
        Level::Inactive
    );
    assert_eq!(
        level(classify(
            MetricId::OsCgroupMemoryOomKillsDelta,
            MetricInput::Scalar(0.0),
        )),
        Level::Inactive
    );
    assert_eq!(
        level(classify(
            MetricId::OsCgroupMemoryOomKillsDelta,
            MetricInput::Scalar(f64::EPSILON),
        )),
        Level::Critical
    );
}
