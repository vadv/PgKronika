//! Public contract tests for the built-in absolute-threshold catalog.

use kronika_analytics::threshold::{
    AgePolicy, Calibration, CatalogEntry, Direction, FractionPolicy, FreeCapacityPolicy, InputKind,
    MetricId, Policy, RatioWithFloorPolicy, ScalarPolicy, Unit, WarningLimitPolicy,
    ZeroDisposition, catalog, catalog_entry, classify,
};
use kronika_analytics::{
    Boundary, Classified, Comparison, Level, MetricInput, NotClassifiedReason,
};
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

const fn boundary(operator: Comparison, value: f64) -> Boundary {
    Boundary { operator, value }
}

fn scalar_entry(
    id: MetricId,
    unit: Unit,
    direction: Direction,
    warning: Option<Boundary>,
    critical: Option<Boundary>,
    zero: ZeroDisposition,
) -> CatalogEntry {
    CatalogEntry {
        id,
        policy: Policy::Scalar(
            ScalarPolicy::new(direction, warning, critical, zero)
                .expect("valid golden scalar policy"),
        ),
        unit,
        calibration: Calibration::Provisional,
    }
}

fn fraction_entry(id: MetricId, warning: Boundary, critical: Boundary) -> CatalogEntry {
    let scalar = ScalarPolicy::new(
        Direction::HigherIsWorse,
        Some(warning),
        Some(critical),
        ZeroDisposition::Classify,
    )
    .expect("valid golden fraction policy");
    CatalogEntry {
        id,
        policy: Policy::Fraction(FractionPolicy::new(scalar)),
        unit: Unit::Ratio,
        calibration: Calibration::Provisional,
    }
}

fn warning_limit_entry(id: MetricId) -> CatalogEntry {
    CatalogEntry {
        id,
        policy: Policy::WarningLimit(
            WarningLimitPolicy::new(Comparison::Above, ZeroDisposition::Classify)
                .expect("valid golden warning-limit policy"),
        ),
        unit: Unit::Count,
        calibration: Calibration::Provisional,
    }
}

fn ratio_with_floor_entry(
    id: MetricId,
    warning: Boundary,
    critical: Boundary,
    floor: Boundary,
) -> CatalogEntry {
    let scalar = ScalarPolicy::new(
        Direction::HigherIsWorse,
        Some(warning),
        Some(critical),
        ZeroDisposition::Classify,
    )
    .expect("valid golden ratio policy");
    CatalogEntry {
        id,
        policy: Policy::RatioWithFloor(
            RatioWithFloorPolicy::new(scalar, floor).expect("valid golden ratio floor"),
        ),
        unit: Unit::Ratio,
        calibration: Calibration::Provisional,
    }
}

fn age_entry(id: MetricId) -> CatalogEntry {
    let scalar = ScalarPolicy::new(
        Direction::HigherIsWorse,
        Some(boundary(Comparison::Above, 21_600.0)),
        Some(boundary(Comparison::Above, 86_400.0)),
        ZeroDisposition::Classify,
    )
    .expect("valid golden age policy");
    CatalogEntry {
        id,
        policy: Policy::AgeGated(AgePolicy::new(scalar).expect("valid golden age direction")),
        unit: Unit::Seconds,
        calibration: Calibration::Provisional,
    }
}

fn free_capacity_entry() -> CatalogEntry {
    let scalar = ScalarPolicy::new(
        Direction::LowerIsWorse,
        Some(boundary(Comparison::Below, 0.20)),
        Some(boundary(Comparison::Below, 0.10)),
        ZeroDisposition::Classify,
    )
    .expect("valid golden capacity fraction");
    CatalogEntry {
        id: MetricId::OsFilesystemFreeCapacity,
        policy: Policy::FreeCapacity(
            FreeCapacityPolicy::new(scalar, boundary(Comparison::Below, 16_106_127_360.0))
                .expect("valid golden capacity ceiling"),
        ),
        unit: Unit::Bytes,
        calibration: Calibration::Provisional,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the independent golden table intentionally lists all 69 entries"
)]
fn golden_catalog() -> Vec<CatalogEntry> {
    vec![
        scalar_entry(
            MetricId::OsProcessCpuPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 50.0)),
            Some(boundary(Comparison::AtLeast, 90.0)),
            ZeroDisposition::Classify,
        ),
        fraction_entry(
            MetricId::OsLoadAvg1PerCore,
            boundary(Comparison::Above, 1.0),
            boundary(Comparison::Above, 2.0),
        ),
        scalar_entry(
            MetricId::OsCpuIdlePercent,
            Unit::Percent,
            Direction::LowerIsWorse,
            Some(boundary(Comparison::Below, 30.0)),
            Some(boundary(Comparison::Below, 10.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsCpuIoWaitPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 5.0)),
            Some(boundary(Comparison::Above, 15.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsCpuStealPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 3.0)),
            Some(boundary(Comparison::Above, 10.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsLoadProcsBlocked,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::Above, 4.0)),
            ZeroDisposition::Classify,
        ),
        fraction_entry(
            MetricId::PgActivityBackendLoadPerCore,
            boundary(Comparison::AtLeast, 0.25),
            boundary(Comparison::AtLeast, 0.50),
        ),
        scalar_entry(
            MetricId::OsMemoryUsedPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 70.0)),
            Some(boundary(Comparison::AtLeast, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsProcessVirtualGrowthKib,
            Unit::Kibibytes,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 102_400.0)),
            Some(boundary(Comparison::Above, 1_048_576.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsProcessResidentGrowthKib,
            Unit::Kibibytes,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 102_400.0)),
            Some(boundary(Comparison::Above, 1_048_576.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsProcessVirtualSwapKib,
            Unit::Kibibytes,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::Above, 102_400.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsMemorySwapUsedKib,
            Unit::Kibibytes,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::Above, 1_048_576.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsVmstatSwapInPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            None,
            Some(boundary(Comparison::Above, 0.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsVmstatSwapOutPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            None,
            Some(boundary(Comparison::Above, 0.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsProcessMajorFaultsDelta,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 100.0)),
            Some(boundary(Comparison::Above, 10_000.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsProcessRssKib,
            Unit::Kibibytes,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 1_048_576.0)),
            Some(boundary(Comparison::Above, 4_194_304.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsPsiCpuSomePercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 5.0)),
            Some(boundary(Comparison::AtLeast, 25.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsPsiMemorySomePercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 5.0)),
            Some(boundary(Comparison::AtLeast, 25.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsPsiIoSomePercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 10.0)),
            Some(boundary(Comparison::AtLeast, 40.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsCgroupCpuUsedPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 70.0)),
            Some(boundary(Comparison::AtLeast, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsCgroupCpuThrottledMillisecondsDelta,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::Above, 100.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsCgroupCpuThrottleEventsDelta,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsCgroupMemoryAnonPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 70.0)),
            Some(boundary(Comparison::AtLeast, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsCgroupMemoryHeadroomPercent,
            Unit::Percent,
            Direction::LowerIsWorse,
            Some(boundary(Comparison::Below, 20.0)),
            Some(boundary(Comparison::Below, 10.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsCgroupMemoryOomKillsDelta,
            Unit::Count,
            Direction::HigherIsWorse,
            None,
            Some(boundary(Comparison::Above, 0.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsDiskUtilPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 60.0)),
            Some(boundary(Comparison::AtLeast, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsDiskMaxAwaitMilliseconds,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 2.0)),
            Some(boundary(Comparison::AtLeast, 10.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsDiskReadAwaitMilliseconds,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 2.0)),
            Some(boundary(Comparison::AtLeast, 10.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::OsDiskWriteAwaitMilliseconds,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 2.0)),
            Some(boundary(Comparison::AtLeast, 10.0)),
            ZeroDisposition::Classify,
        ),
        free_capacity_entry(),
        scalar_entry(
            MetricId::OsProcessBlockDelaySecondsDelta,
            Unit::Seconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 10.0)),
            Some(boundary(Comparison::Above, 50.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsDiskBlocksReadPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsNetworkErrorsPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::Above, 10.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::OsNetworkDropsPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::Above, 10.0)),
            ZeroDisposition::Inactive,
        ),
        ratio_with_floor_entry(
            MetricId::PgTablesDeadTuplePercent,
            boundary(Comparison::AtLeast, 0.10),
            boundary(Comparison::AtLeast, 0.20),
            boundary(Comparison::Above, 10_000.0),
        ),
        scalar_entry(
            MetricId::PgTablesDeadTuples,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 1_000.0)),
            Some(boundary(Comparison::AtLeast, 100_000.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgTablesSequentialScanPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 30.0)),
            Some(boundary(Comparison::AtLeast, 80.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgTablesModifiedSinceAnalyze,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 100_000.0)),
            Some(boundary(Comparison::AtLeast, 1_000_000.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgTablesInsertedSinceVacuum,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 100_000.0)),
            Some(boundary(Comparison::AtLeast, 1_000_000.0)),
            ZeroDisposition::Classify,
        ),
        age_entry(MetricId::PgTablesAutovacuumAgeSeconds),
        age_entry(MetricId::PgTablesAutoanalyzeAgeSeconds),
        scalar_entry(
            MetricId::PgTablesTempBytesPerSecond,
            Unit::BytesPerSecond,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgActivityQueryDurationSeconds,
            Unit::Seconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 1.0)),
            Some(boundary(Comparison::AtLeast, 30.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgActivityTransactionDurationSeconds,
            Unit::Seconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 5.0)),
            Some(boundary(Comparison::AtLeast, 60.0)),
            ZeroDisposition::Classify,
        ),
        fraction_entry(
            MetricId::PgActivityClientBackendCapacity,
            boundary(Comparison::AtLeast, 0.70),
            boundary(Comparison::AtLeast, 0.90),
        ),
        scalar_entry(
            MetricId::PgActivityIdleInTransactionSessions,
            Unit::Count,
            Direction::HigherIsWorse,
            None,
            Some(boundary(Comparison::Above, 0.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgActivityBlockedSessions,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::AtLeast, 5.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgActivityLongQueries,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::AtLeast, 5.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgActivityLongTransactions,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::AtLeast, 3.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgDatabaseRollbackPercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 3.0)),
            Some(boundary(Comparison::Above, 10.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgDatabaseDeadlocksDelta,
            Unit::Count,
            Direction::HigherIsWorse,
            None,
            Some(boundary(Comparison::Above, 0.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgDatabaseCacheHitPercent,
            Unit::Percent,
            Direction::LowerIsWorse,
            Some(boundary(Comparison::Below, 99.0)),
            Some(boundary(Comparison::Below, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgDatabaseIoCacheHitPercent,
            Unit::Percent,
            Direction::LowerIsWorse,
            Some(boundary(Comparison::Below, 99.0)),
            Some(boundary(Comparison::Below, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgDatabaseEffectiveCacheHitPercent,
            Unit::Percent,
            Direction::LowerIsWorse,
            Some(boundary(Comparison::Below, 99.0)),
            Some(boundary(Comparison::Below, 90.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgCheckpointerCheckpointsPerMinute,
            Unit::CountPerMinute,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 2.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgCheckpointerWriteTimeMillisecondsDelta,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 30_000.0)),
            Some(boundary(Comparison::Above, 120_000.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgBgwriterBuffersBackendPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgBgwriterMaxwrittenCleanDelta,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgBgwriterClientEvictionsPerSecond,
            Unit::CountPerSecond,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            Some(boundary(Comparison::AtLeast, 10.0)),
            ZeroDisposition::Inactive,
        ),
        scalar_entry(
            MetricId::PgStatementsMillisecondsPerRow,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 10.0)),
            Some(boundary(Comparison::AtLeast, 100.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgStatementsMeanTimeMilliseconds,
            Unit::Milliseconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 10.0)),
            Some(boundary(Comparison::AtLeast, 100.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgStatementsTimePercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 20.0)),
            Some(boundary(Comparison::AtLeast, 50.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgStatementsPlanTimePercent,
            Unit::Percent,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::AtLeast, 50.0)),
            Some(boundary(Comparison::AtLeast, 80.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgStatementsPlanCount,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 1.0)),
            Some(boundary(Comparison::Above, 3.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgReplicationReplayLagSeconds,
            Unit::Seconds,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 10.0)),
            Some(boundary(Comparison::Above, 60.0)),
            ZeroDisposition::Classify,
        ),
        scalar_entry(
            MetricId::PgDatabaseRecoveryConflictsDelta,
            Unit::Count,
            Direction::HigherIsWorse,
            Some(boundary(Comparison::Above, 0.0)),
            None,
            ZeroDisposition::Inactive,
        ),
        warning_limit_entry(MetricId::PgTablesVacuumThresholdExceeded),
        warning_limit_entry(MetricId::PgTablesAnalyzeThresholdExceeded),
        warning_limit_entry(MetricId::PgTablesInsertVacuumThresholdExceeded),
    ]
}

const fn valid_input(kind: InputKind) -> MetricInput {
    match kind {
        InputKind::Scalar => MetricInput::Scalar(0.0),
        InputKind::Fraction => MetricInput::Fraction {
            numerator: 0.0,
            denominator: 1.0,
        },
        InputKind::Limit => MetricInput::Limit {
            observed: 0.0,
            limit: 0.0,
        },
        InputKind::RatioWithFloor => MetricInput::RatioWithFloor {
            ratio: 0.0,
            count: 0.0,
        },
        InputKind::Age => MetricInput::Age {
            epoch_seconds: 0.0,
            now_seconds: 0.0,
            gate: true,
        },
        InputKind::FreeCapacity => MetricInput::FreeCapacity {
            available_bytes: 0.0,
            total_bytes: 1.0,
        },
    }
}

const fn negative_zero_input(kind: InputKind) -> MetricInput {
    match kind {
        InputKind::Scalar => MetricInput::Scalar(-0.0),
        InputKind::Fraction => MetricInput::Fraction {
            numerator: -0.0,
            denominator: 1.0,
        },
        InputKind::Limit => MetricInput::Limit {
            observed: -0.0,
            limit: -0.0,
        },
        InputKind::RatioWithFloor => MetricInput::RatioWithFloor {
            ratio: -0.0,
            count: -0.0,
        },
        InputKind::Age => MetricInput::Age {
            epoch_seconds: -0.0,
            now_seconds: 0.0,
            gate: true,
        },
        InputKind::FreeCapacity => MetricInput::FreeCapacity {
            available_bytes: -0.0,
            total_bytes: 1.0,
        },
    }
}

const fn wrong_input(kind: InputKind) -> MetricInput {
    match kind {
        InputKind::Scalar => MetricInput::Fraction {
            numerator: 0.0,
            denominator: 1.0,
        },
        InputKind::Fraction
        | InputKind::Limit
        | InputKind::RatioWithFloor
        | InputKind::Age
        | InputKind::FreeCapacity => MetricInput::Scalar(0.0),
    }
}

#[test]
fn complete_catalog_matches_the_public_golden_table() {
    let expected = golden_catalog();

    assert_eq!(catalog(), expected.as_slice());
    assert_eq!(catalog().len(), 69);
    assert_eq!(MetricId::ALL.len(), 69);
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
fn every_entry_accepts_only_its_declared_input_shape() {
    for entry in catalog() {
        let kind = entry.policy.input_kind();
        assert_ne!(
            classify(entry.id, valid_input(kind)),
            Classified::NotClassified(NotClassifiedReason::InputShapeMismatch)
        );
        assert_eq!(
            classify(entry.id, wrong_input(kind)),
            Classified::NotClassified(NotClassifiedReason::InputShapeMismatch)
        );
        assert_eq!(
            classify(entry.id, valid_input(kind)),
            classify(entry.id, negative_zero_input(kind))
        );
    }
}

#[test]
fn representative_invalid_numbers_keep_exact_reasons() {
    for input in [
        MetricInput::Scalar(f64::NAN),
        MetricInput::Scalar(f64::INFINITY),
    ] {
        assert_eq!(
            classify(MetricId::OsProcessCpuPercent, input),
            Classified::NotClassified(NotClassifiedReason::NonFinite)
        );
    }
    assert_eq!(
        classify(MetricId::OsProcessCpuPercent, MetricInput::Scalar(-1.0),),
        Classified::NotClassified(NotClassifiedReason::OutOfDomain)
    );
    assert_eq!(
        classify(
            MetricId::OsLoadAvg1PerCore,
            MetricInput::Fraction {
                numerator: 1.0,
                denominator: 0.0,
            },
        ),
        Classified::NotClassified(NotClassifiedReason::InvalidDenominator)
    );
    assert_eq!(
        classify(
            MetricId::OsFilesystemFreeCapacity,
            MetricInput::FreeCapacity {
                available_bytes: 101.0,
                total_bytes: 100.0,
            },
        ),
        Classified::NotClassified(NotClassifiedReason::OutOfDomain)
    );
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

#[test]
fn free_capacity_requires_fraction_and_absolute_conditions() {
    const GIB: f64 = 1_073_741_824.0;

    assert_eq!(
        level(classify(
            MetricId::OsFilesystemFreeCapacity,
            MetricInput::FreeCapacity {
                available_bytes: 14.0 * GIB,
                total_bytes: 100.0 * GIB,
            },
        )),
        Level::Warning
    );
    assert_eq!(
        level(classify(
            MetricId::OsFilesystemFreeCapacity,
            MetricInput::FreeCapacity {
                available_bytes: 15.0 * GIB,
                total_bytes: 100.0 * GIB,
            },
        )),
        Level::Ok
    );
}

#[test]
fn dead_tuple_floor_and_age_gate_are_preserved() {
    assert_eq!(
        level(classify(
            MetricId::PgTablesDeadTuplePercent,
            MetricInput::RatioWithFloor {
                ratio: 0.20,
                count: 10_000.0,
            },
        )),
        Level::Ok
    );
    assert_eq!(
        classify(
            MetricId::PgTablesAutovacuumAgeSeconds,
            MetricInput::Age {
                epoch_seconds: 0.0,
                now_seconds: 90_000.0,
                gate: false,
            },
        ),
        Classified::NotClassified(NotClassifiedReason::NotApplicable)
    );
}

#[test]
fn connection_capacity_uses_max_connections_as_its_denominator() {
    for (numerator, denominator, expected) in [
        (5.0, 35.0, Level::Ok),
        (70.0, 100.0, Level::Warning),
        (90.0, 100.0, Level::Critical),
    ] {
        assert_eq!(
            level(classify(
                MetricId::PgActivityClientBackendCapacity,
                MetricInput::Fraction {
                    numerator,
                    denominator,
                },
            )),
            expected
        );
    }

    assert_eq!(
        classify(
            MetricId::PgActivityClientBackendCapacity,
            MetricInput::Fraction {
                numerator: 5.0,
                denominator: 0.0,
            },
        ),
        Classified::NotClassified(NotClassifiedReason::InvalidDenominator)
    );
}

#[test]
fn new_postgres_boundaries_preserve_strict_and_inclusive_operators() {
    for (id, value, expected) in [
        (
            MetricId::PgActivityTransactionDurationSeconds,
            5.0,
            Level::Warning,
        ),
        (MetricId::PgActivityBlockedSessions, 5.0, Level::Critical),
        (MetricId::PgDatabaseRollbackPercent, 3.0, Level::Ok),
        (MetricId::PgDatabaseCacheHitPercent, 90.0, Level::Warning),
        (MetricId::PgCheckpointerCheckpointsPerMinute, 2.0, Level::Ok),
        (
            MetricId::PgCheckpointerWriteTimeMillisecondsDelta,
            120_000.0,
            Level::Warning,
        ),
        (
            MetricId::PgStatementsMillisecondsPerRow,
            100.0,
            Level::Critical,
        ),
        (MetricId::PgStatementsPlanCount, 3.0, Level::Warning),
        (
            MetricId::PgReplicationReplayLagSeconds,
            60.0,
            Level::Warning,
        ),
    ] {
        assert_eq!(level(classify(id, MetricInput::Scalar(value))), expected);
    }
}

#[test]
fn event_indicators_and_config_bound_limits_keep_zero_semantics() {
    assert_eq!(
        level(classify(
            MetricId::PgBgwriterClientEvictionsPerSecond,
            MetricInput::Scalar(0.0),
        )),
        Level::Inactive
    );
    assert_eq!(
        level(classify(
            MetricId::PgBgwriterClientEvictionsPerSecond,
            MetricInput::Scalar(f64::EPSILON),
        )),
        Level::Warning
    );
    assert_eq!(
        level(classify(
            MetricId::PgBgwriterClientEvictionsPerSecond,
            MetricInput::Scalar(10.0),
        )),
        Level::Critical
    );

    for id in [
        MetricId::PgTablesVacuumThresholdExceeded,
        MetricId::PgTablesAnalyzeThresholdExceeded,
        MetricId::PgTablesInsertVacuumThresholdExceeded,
    ] {
        assert_eq!(
            level(classify(
                id,
                MetricInput::Limit {
                    observed: 0.0,
                    limit: 0.0,
                },
            )),
            Level::Ok
        );
        assert_eq!(
            level(classify(
                id,
                MetricInput::Limit {
                    observed: f64::EPSILON,
                    limit: 0.0,
                },
            )),
            Level::Warning
        );
    }
}
