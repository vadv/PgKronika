# Typed Threshold Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an allocation-free Class 1 threshold kernel and a typed catalog of 42 provisional metric policies to `kronika-analytics`, without a web consumer.

**Architecture:** A closed `MetricId` enum indexes one static `CatalogEntry` array. Each entry contains a typed `Policy`; `Policy::classify` validates a matching fixed-size `MetricInput` and returns an explainable `Classified` result. Domain modules own built-in metric values, while `model.rs` and `policy.rs` remain source- and transport-independent.

**Tech Stack:** Rust 2024, Rust 1.96, workspace lint profile, built-in test framework; no new crate dependencies.

## Global Constraints

- Implement exactly 42 catalog entries from `docs/superpowers/specs/2026-07-29-absolute-threshold-catalog-design.md`.
- Every built-in policy is `Calibration::Provisional`.
- One classification is deterministic, performs no I/O, reads no clock, allocates no heap memory, and uses O(1) work and memory.
- Preserve strict versus inclusive comparisons: `>`, `>=`, `<`, and `<=` are distinct.
- Percent scalars use `0..=100`; `Fraction` and `RatioWithFloor` use `1.0 == 100 %`.
- Missing, not-applicable, non-finite, out-of-domain, invalid-denominator, and input-shape states remain distinct.
- Do not change JSON schemas, PGM/OVF formats, web handlers, anomaly scoring, or incident lenses.
- Add no dependency to any Cargo manifest.
- Keep public rustdoc contractual and update the English and Russian crate READMEs together.
- Run the standing memory-bounds and comment-quality review before each commit.

---

## File Map

- `crates/kronika-analytics/src/threshold/model.rs`: fixed-size public inputs, verdicts, evidence, boundaries, levels, and failure reasons.
- `crates/kronika-analytics/src/threshold/policy.rs`: validated policy types and allocation-free classification.
- `crates/kronika-analytics/src/threshold/catalog/mod.rs`: `MetricId`, metadata, the canonical 42-entry array, lookup, and catalog invariants.
- `crates/kronika-analytics/src/threshold/catalog/cpu.rs`: seven CPU/load entries.
- `crates/kronika-analytics/src/threshold/catalog/memory.rs`: nine memory/swap entries.
- `crates/kronika-analytics/src/threshold/catalog/pressure.rs`: three PSI entries.
- `crates/kronika-analytics/src/threshold/catalog/cgroup.rs`: six cgroup entries.
- `crates/kronika-analytics/src/threshold/catalog/storage.rs`: nine disk/network entries.
- `crates/kronika-analytics/src/threshold/catalog/postgres_tables.rs`: eight PostgreSQL table/vacuum entries.
- `crates/kronika-analytics/src/threshold/mod.rs`: module facade and `classify(MetricId, MetricInput)`.
- `crates/kronika-analytics/src/lib.rs`: public module and root re-exports.
- `crates/kronika-analytics/tests/threshold_catalog.rs`: public consumer and exact 42-entry golden table.
- `crates/kronika-analytics/README.md`: English contract and current integration status.
- `crates/kronika-analytics/README.ru.md`: Russian mirror.

### Task 1: Fixed-Size Public Model

**Files:**
- Create: `crates/kronika-analytics/src/threshold/model.rs`
- Create: `crates/kronika-analytics/src/threshold/mod.rs`
- Modify: `crates/kronika-analytics/src/lib.rs`
- Test: `crates/kronika-analytics/src/threshold/model.rs`

**Interfaces:**
- Produces: `Level`, `Classified`, `Verdict`, `Boundary`, `Comparison`, `Evidence`, `MetricInput`, and `NotClassifiedReason`.
- Consumes: no new project types or dependencies.

- [ ] **Step 1: Write model tests before exposing the module**

Add `#[cfg(test)] mod tests` to the new `model.rs` with exact construction and equality checks:

```rust
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
```

- [ ] **Step 2: Run the focused test and verify the red state**

Run:

```bash
cargo test -p kronika-analytics threshold::model::tests
```

Expected: compilation fails because the model types and module do not exist.

- [ ] **Step 3: Implement the model types**

Define these exact shapes with contractual rustdoc and `Debug`, `Clone`, `Copy`, and `PartialEq`; also derive `Eq` where no `f64` is present:

```rust
pub enum Level {
    Inactive,
    Ok,
    Warning,
    Critical,
}

pub enum Classified {
    Verdict(Verdict),
    NotClassified(NotClassifiedReason),
}

pub struct Verdict {
    pub level: Level,
    pub boundary: Option<Boundary>,
    pub evidence: Evidence,
}

pub struct Boundary {
    pub operator: Comparison,
    pub value: f64,
}

pub enum Comparison {
    Above,
    AtLeast,
    Below,
    AtMost,
}

pub enum NotClassifiedReason {
    Missing,
    NonFinite,
    OutOfDomain,
    InvalidDenominator,
    NotApplicable,
    InputShapeMismatch,
}
```

Define `MetricInput` exactly as the approved spec:

```rust
pub enum MetricInput {
    Missing,
    NotApplicable,
    Scalar(f64),
    Fraction {
        numerator: f64,
        denominator: f64,
    },
    RatioWithFloor {
        ratio: f64,
        count: f64,
    },
    Age {
        epoch_seconds: f64,
        now_seconds: f64,
        gate: bool,
    },
    FreeCapacity {
        available_bytes: f64,
        total_bytes: f64,
    },
}
```

Define the fixed-size evidence variants:

```rust
pub enum Evidence {
    Scalar {
        observed: f64,
    },
    Fraction {
        numerator: f64,
        denominator: f64,
        value: f64,
    },
    RatioWithFloor {
        ratio: f64,
        count: f64,
        floor: Boundary,
    },
    Age {
        epoch_seconds: f64,
        now_seconds: f64,
        age_seconds: f64,
    },
    FreeCapacity {
        available_bytes: f64,
        total_bytes: f64,
        available_fraction: f64,
        absolute_ceiling_bytes: Boundary,
    },
}
```

Normalize no values in constructors at this layer. `model.rs` is a data contract; validation belongs to `policy.rs`.

Create `threshold/mod.rs` with `mod model; pub use model::{...};`, declare `pub mod threshold;` in `lib.rs`, and root-re-export the model types consistently with the existing `anomaly` exports.

- [ ] **Step 4: Run model tests and crate checks**

Run:

```bash
cargo test -p kronika-analytics threshold::model::tests
cargo fmt --all --check
cargo clippy -p kronika-analytics --all-targets -- -D warnings
```

Expected: all commands pass. Review the diff for heap-backed fields and narration-only comments; the model must contain neither.

- [ ] **Step 5: Commit the public model**

```bash
git add crates/kronika-analytics/src/threshold crates/kronika-analytics/src/lib.rs
git commit -m "feat: add threshold classification model"
```

### Task 2: Validated Policy Engine

**Files:**
- Create: `crates/kronika-analytics/src/threshold/policy.rs`
- Modify: `crates/kronika-analytics/src/threshold/mod.rs`
- Test: `crates/kronika-analytics/src/threshold/policy.rs`

**Interfaces:**
- Consumes: `Boundary`, `Classified`, `Comparison`, `Evidence`, `Level`, `MetricInput`, `NotClassifiedReason`, and `Verdict` from Task 1.
- Produces: `Policy`, `ScalarPolicy`, `FractionPolicy`, `RatioWithFloorPolicy`, `AgePolicy`, `FreeCapacityPolicy`, `InputKind`, `Direction`, `ZeroDisposition`, and `InvalidPolicy`.
- Produces: `Policy::classify(&self, MetricInput) -> Classified` and `Policy::input_kind(&self) -> InputKind`.
- Produces these validated constructors:

```rust
ScalarPolicy::new(
    Direction,
    Option<Boundary>,
    Option<Boundary>,
    ZeroDisposition,
) -> Result<ScalarPolicy, InvalidPolicy>
FractionPolicy::new(ScalarPolicy) -> FractionPolicy
RatioWithFloorPolicy::new(
    ScalarPolicy,
    Boundary,
) -> Result<RatioWithFloorPolicy, InvalidPolicy>
AgePolicy::new(ScalarPolicy) -> Result<AgePolicy, InvalidPolicy>
FreeCapacityPolicy::new(
    ScalarPolicy,
    Boundary,
) -> Result<FreeCapacityPolicy, InvalidPolicy>
```

- [ ] **Step 1: Write red tests for comparison and scalar semantics**

Add table tests that cover every boundary operator and priority:

```rust
#[test]
fn scalar_boundaries_preserve_strictness_and_critical_priority() {
    let policy = Policy::Scalar(
        ScalarPolicy::new(
            Direction::HigherIsWorse,
            Some(Boundary {
                operator: Comparison::AtLeast,
                value: 50.0,
            }),
            Some(Boundary {
                operator: Comparison::AtLeast,
                value: 90.0,
            }),
            ZeroDisposition::Classify,
        )
        .expect("valid fixture"),
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
fn strict_above_does_not_fire_on_the_boundary() {
    let policy = scalar_higher(
        Some((Comparison::Above, 0.0)),
        Some((Comparison::Above, 4.0)),
        ZeroDisposition::Classify,
    );
    assert_eq!(level(policy.classify(MetricInput::Scalar(0.0))), Level::Ok);
    assert_eq!(
        level(policy.classify(MetricInput::Scalar(f64::EPSILON))),
        Level::Warning
    );
    assert_eq!(level(policy.classify(MetricInput::Scalar(4.0))), Level::Warning);
}
```

Add lower-is-worse cases for `Below` and `AtMost`, plus policies with only warning and only critical.

- [ ] **Step 2: Run the scalar tests and verify the red state**

Run:

```bash
cargo test -p kronika-analytics threshold::policy::tests::scalar
```

Expected: compilation fails because policy types and `Policy::classify` are absent.

- [ ] **Step 3: Implement validated scalar policies**

Implement:

```rust
pub enum Direction {
    HigherIsWorse,
    LowerIsWorse,
}

pub enum ZeroDisposition {
    Classify,
    Inactive,
}

pub enum InputKind {
    Scalar,
    Fraction,
    RatioWithFloor,
    Age,
    FreeCapacity,
}
```

`ScalarPolicy::new` returns `Result<Self, InvalidPolicy>` and rejects:

- no warning and no critical boundary;
- non-finite or negative boundary values;
- `Below`/`AtMost` on `HigherIsWorse`;
- `Above`/`AtLeast` on `LowerIsWorse`;
- warning above critical for `HigherIsWorse`;
- warning below critical for `LowerIsWorse`.

Define `InvalidPolicy` with exact variants `NoBoundary`,
`NonFiniteBoundary`, `NegativeBoundary`, `DirectionMismatch`,
`BoundaryOrder`, `InvalidFloor`, and `InvalidCapacityCeiling`.

Expose read-only accessors for direction, warning, critical, and zero disposition. Add one crate-private `const fn catalog(...) -> Self` for built-in constants. It enforces the same invariants with const assertions; add a narrowly scoped lint allowance explaining that an invalid built-in catalog is a compile-time programming error.

Implement scalar classification in this order:

1. Return `Missing` or `NotApplicable` for the two state inputs before shape matching.
2. Return `InputShapeMismatch` for a non-scalar shape.
3. Reject non-finite and negative values.
4. Normalize `-0.0` to `0.0`.
5. Return `Inactive` when zero disposition requires it.
6. Test critical, then warning, then return `Ok`.

Each verdict uses `Evidence::Scalar` and includes only the boundary that selected warning or critical.

- [ ] **Step 4: Write red tests for composite policies**

Add tests for these exact cases:

```rust
#[test]
fn fraction_reports_operands_and_rejects_denominators() {
    let policy = load_per_core_policy();
    let classified = policy.classify(MetricInput::Fraction {
        numerator: 12.0,
        denominator: 4.0,
    });
    assert_eq!(level(classified), Level::Critical);
    assert_eq!(
        policy.classify(MetricInput::Fraction {
            numerator: 1.0,
            denominator: 0.0,
        }),
        Classified::NotClassified(NotClassifiedReason::InvalidDenominator)
    );
}

#[test]
fn ratio_floor_is_ok_until_both_conditions_cross() {
    let policy = dead_tuple_policy();
    assert_eq!(
        level(policy.classify(MetricInput::RatioWithFloor {
            ratio: 0.50,
            count: 10_000.0,
        })),
        Level::Ok
    );
    assert_eq!(
        level(policy.classify(MetricInput::RatioWithFloor {
            ratio: 0.20,
            count: 10_001.0,
        })),
        Level::Critical
    );
}

#[test]
fn age_gate_and_future_epoch_do_not_produce_health_verdicts() {
    let policy = age_policy();
    assert_eq!(
        policy.classify(MetricInput::Age {
            epoch_seconds: 1.0,
            now_seconds: 90_000.0,
            gate: false,
        }),
        Classified::NotClassified(NotClassifiedReason::NotApplicable)
    );
    assert_eq!(
        policy.classify(MetricInput::Age {
            epoch_seconds: 2.0,
            now_seconds: 1.0,
            gate: true,
        }),
        Classified::NotClassified(NotClassifiedReason::OutOfDomain)
    );
}
```

Add a free-capacity table that proves both conditions are required and equality at `15 GiB` remains `Ok`.

- [ ] **Step 5: Run composite tests and verify the red state**

Run:

```bash
cargo test -p kronika-analytics threshold::policy::tests
```

Expected: scalar tests pass; composite tests fail because the composite variants are not implemented.

- [ ] **Step 6: Implement composite policies**

Implement:

```rust
pub enum Policy {
    Scalar(ScalarPolicy),
    Fraction(FractionPolicy),
    RatioWithFloor(RatioWithFloorPolicy),
    AgeGated(AgePolicy),
    FreeCapacity(FreeCapacityPolicy),
}
```

Required behavior:

- `FractionPolicy` validates finite, non-negative operands and `denominator > 0`, computes `numerator / denominator`, checks that result is finite, and applies its scalar policy.
- `RatioWithFloorPolicy` validates ratio and count, returns `Ok` when the count does not match its floor boundary, and otherwise applies ratio boundaries.
- `AgePolicy` returns `NotApplicable` when `gate == false`; otherwise it validates operands, requires `epoch_seconds <= now_seconds`, computes age, and applies higher-is-worse boundaries.
- `FreeCapacityPolicy` validates finite non-negative bytes, requires `total_bytes > 0` and `available_bytes <= total_bytes`, computes available fraction, and requires both the fraction boundary and absolute ceiling boundary for warning or critical.
- A correct-shape non-finite operand returns `NonFinite`.
- A wrong shape returns `InputShapeMismatch`.
- Derived zero follows the nested scalar policy's `ZeroDisposition`.

Factor the scalar level/boundary selection into one private function that receives a validated value. Do not allocate an intermediate collection or string.

- [ ] **Step 7: Run policy tests and crate gates**

Run:

```bash
cargo test -p kronika-analytics threshold::policy::tests
cargo fmt --all --check
cargo clippy -p kronika-analytics --all-targets -- -D warnings
```

Expected: all pass. Review the code path manually: every branch uses fixed-size values, and every rustdoc states caller-visible contracts rather than narrating implementation.

- [ ] **Step 8: Commit the policy engine**

```bash
git add crates/kronika-analytics/src/threshold
git commit -m "feat: add typed threshold policy engine"
```

### Task 3: CPU, Memory, PSI, and cgroup Catalog

**Files:**
- Create: `crates/kronika-analytics/src/threshold/catalog/mod.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/cpu.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/memory.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/pressure.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/cgroup.rs`
- Modify: `crates/kronika-analytics/src/threshold/mod.rs`
- Test: `crates/kronika-analytics/src/threshold/catalog/mod.rs`

**Interfaces:**
- Consumes: all Task 2 policy types and `MetricInput`.
- Produces: `MetricId`, `Unit`, `Calibration`, `CatalogEntry`, `catalog() -> &'static [CatalogEntry]`, `catalog_entry(MetricId) -> &'static CatalogEntry`, and `classify(MetricId, MetricInput) -> Classified`.
- Interim result: 25 entries. Task 4 extends the same closed array to 42.

- [ ] **Step 1: Write red catalog identity tests**

Test the interim catalog:

```rust
#[test]
fn first_domain_batch_is_unique_ordered_and_provisional() {
    assert_eq!(catalog().len(), 25);
    assert_eq!(
        catalog().iter().map(|entry| entry.id).collect::<Vec<_>>(),
        MetricId::ALL.to_vec()
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
    let original = codes.clone();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), original.len());
}
```

Add public-path classification assertions for:

- `os.process.cpu_pct`: `50.0 -> Warning`, `90.0 -> Critical`;
- `os.cpu.idle_pct`: `30.0 -> Ok`, value immediately below `30.0 -> Warning`;
- `os.process.virtual_swap_kib`: `0.0 -> Inactive`;
- `os.psi.io_some_pct`: `10.0 -> Warning`, `40.0 -> Critical`;
- `os.cgroup.memory_oom_kills_delta`: `0.0 -> Inactive`, positive epsilon -> `Critical`.

- [ ] **Step 2: Run catalog tests and verify the red state**

Run:

```bash
cargo test -p kronika-analytics threshold::catalog::tests
```

Expected: compilation fails because catalog types and domain entries do not exist.

- [ ] **Step 3: Implement catalog metadata and lookup**

Define:

```rust
#[repr(u8)]
pub enum MetricId {
    OsProcessCpuPercent,
    OsLoadAvg1PerCore,
    OsCpuIdlePercent,
    OsCpuIoWaitPercent,
    OsCpuStealPercent,
    OsLoadProcsBlocked,
    PgActivityBackendLoadPerCore,
    OsMemoryUsedPercent,
    OsProcessVirtualGrowthKib,
    OsProcessResidentGrowthKib,
    OsProcessVirtualSwapKib,
    OsMemorySwapUsedKib,
    OsVmstatSwapInPerSecond,
    OsVmstatSwapOutPerSecond,
    OsProcessMajorFaultsDelta,
    OsProcessRssKib,
    OsPsiCpuSomePercent,
    OsPsiMemorySomePercent,
    OsPsiIoSomePercent,
    OsCgroupCpuUsedPercent,
    OsCgroupCpuThrottledMillisecondsDelta,
    OsCgroupCpuThrottleEventsDelta,
    OsCgroupMemoryAnonPercent,
    OsCgroupMemoryHeadroomPercent,
    OsCgroupMemoryOomKillsDelta,
}

pub enum Unit {
    Percent,
    Ratio,
    Count,
    Kibibytes,
    Milliseconds,
    Seconds,
    CountPerSecond,
    BytesPerSecond,
    Bytes,
}

pub enum Calibration {
    Provisional,
    Validated,
}

pub struct CatalogEntry {
    pub id: MetricId,
    pub policy: Policy,
    pub unit: Unit,
    pub calibration: Calibration,
}
```

Give `MetricId` an `ALL` array and exact `as_str()` match. Keep enum order identical to the canonical array. `catalog_entry` indexes the fixed array by `id as usize`; a unit test zips `MetricId::ALL` with the array to prove the invariant.

`threshold::classify(id, input)` delegates directly to `catalog_entry(id).policy.classify(input)`.

- [ ] **Step 4: Add the seven CPU/load policies**

Add these exact IDs, inputs, zero behavior, and boundaries:

| ID | Policy | Zero | Warning | Critical |
| --- | --- | --- | --- | --- |
| `os.process.cpu_pct` | scalar higher, percent | Ok | `>= 50` | `>= 90` |
| `os.load.avg1_per_core` | fraction higher, ratio | Ok | `> 1` | `> 2` |
| `os.cpu.idle_pct` | scalar lower, percent | Ok | `< 30` | `< 10` |
| `os.cpu.iowait_pct` | scalar higher, percent | Ok | `> 5` | `> 15` |
| `os.cpu.steal_pct` | scalar higher, percent | Ok | `> 3` | `> 10` |
| `os.load.procs_blocked` | scalar higher, count | Ok | `> 0` | `> 4` |
| `pg.activity.backend_load_per_core` | fraction higher, ratio | Ok | `>= 0.25` | `>= 0.5` |

- [ ] **Step 5: Add the nine memory/swap policies**

| ID | Zero | Warning | Critical |
| --- | --- | --- | --- |
| `os.memory.used_pct` | Ok | `>= 70` | `>= 90` |
| `os.process.virtual_growth_kib` | Inactive | `> 102400` | `> 1048576` |
| `os.process.resident_growth_kib` | Inactive | `> 102400` | `> 1048576` |
| `os.process.virtual_swap_kib` | Inactive | `> 0` | `> 102400` |
| `os.memory.swap_used_kib` | Inactive | `> 0` | `> 1048576` |
| `os.vmstat.swap_in_per_second` | Inactive | none | `> 0` |
| `os.vmstat.swap_out_per_second` | Inactive | none | `> 0` |
| `os.process.major_faults_delta` | Inactive | `> 100` | `> 10000` |
| `os.process.rss_kib` | Ok | `> 1048576` | `> 4194304` |

Use `Unit::Percent` for memory percentage, `Unit::Kibibytes` for KiB values, `Unit::CountPerSecond` for swap rates, and `Unit::Count` for major-fault delta.

- [ ] **Step 6: Add the three PSI and six cgroup policies**

PSI:

- `os.psi.cpu_some_pct`: `>= 5`, `>= 25`;
- `os.psi.memory_some_pct`: `>= 5`, `>= 25`;
- `os.psi.io_some_pct`: `>= 10`, `>= 40`.

cgroup:

- `os.cgroup.cpu_used_pct`: `>= 70`, `>= 90`, zero Ok;
- `os.cgroup.cpu_throttled_ms_delta`: `> 0`, `> 100`, zero Inactive;
- `os.cgroup.cpu_throttle_events_delta`: `> 0`, no critical, zero Inactive;
- `os.cgroup.memory_anon_pct`: `>= 70`, `>= 90`, zero Ok;
- `os.cgroup.memory_headroom_pct`: `< 20`, `< 10`, zero Ok;
- `os.cgroup.memory_oom_kills_delta`: no warning, `> 0`, zero Inactive.

- [ ] **Step 7: Run the interim catalog tests**

Run:

```bash
cargo test -p kronika-analytics threshold::catalog::tests
cargo test -p kronika-analytics
cargo fmt --all --check
cargo clippy -p kronika-analytics --all-targets -- -D warnings
```

Expected: all pass with 25 catalog entries. Confirm `Cargo.toml` and `Cargo.lock` are unchanged.

- [ ] **Step 8: Commit the first catalog batch**

```bash
git add crates/kronika-analytics/src/threshold
git commit -m "feat: catalog resource threshold policies"
```

### Task 4: Storage and PostgreSQL Policies, Exact Golden Catalog

**Files:**
- Create: `crates/kronika-analytics/src/threshold/catalog/storage.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/postgres_tables.rs`
- Modify: `crates/kronika-analytics/src/threshold/catalog/mod.rs`
- Create: `crates/kronika-analytics/tests/threshold_catalog.rs`

**Interfaces:**
- Consumes: the Task 3 catalog array, metadata, lookup, and public `classify`.
- Produces: the final 42-variant `MetricId::ALL` and 42-entry `catalog()`.
- Produces: a public consumer test whose expected `CatalogEntry` vector is the golden table.

- [ ] **Step 1: Write the final public golden test before adding entries**

Create an integration test that imports only public `kronika_analytics` APIs. Build `expected: Vec<CatalogEntry>` with small fixture helpers that call validated public policy constructors and `.expect("valid golden policy")`. List all 42 entries independently from production domain constants, then assert:

```rust
assert_eq!(catalog(), expected.as_slice());
assert_eq!(MetricId::ALL.len(), 42);
for (index, id) in MetricId::ALL.iter().copied().enumerate() {
    assert_eq!(catalog_entry(id), &catalog()[index]);
}
```

The expected table must repeat every ID, unit, calibration, input form, zero disposition, floor, absolute ceiling, and boundary from the approved design. This duplication is intentional: a threshold change must produce a visible test diff.

Add behavior tests:

```rust
#[test]
fn free_capacity_requires_fraction_and_absolute_conditions() {
    let gib = 1_073_741_824.0;
    assert_eq!(
        level(classify(
            MetricId::OsFilesystemFreeCapacity,
            MetricInput::FreeCapacity {
                available_bytes: 14.0 * gib,
                total_bytes: 100.0 * gib,
            },
        )),
        Level::Warning
    );
    assert_eq!(
        level(classify(
            MetricId::OsFilesystemFreeCapacity,
            MetricInput::FreeCapacity {
                available_bytes: 15.0 * gib,
                total_bytes: 100.0 * gib,
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
```

- [ ] **Step 2: Run the integration test and verify the red state**

Run:

```bash
cargo test -p kronika-analytics --test threshold_catalog
```

Expected: compilation fails because the 17 final `MetricId` variants and entries do not exist.

- [ ] **Step 3: Add the nine storage/network entries**

Extend `MetricId`, `MetricId::ALL`, `as_str`, and the canonical catalog in this exact order:

| ID | Policy | Zero | Warning | Critical |
| --- | --- | --- | --- | --- |
| `os.disk.util_pct` | scalar percent | Ok | `>= 60` | `>= 90` |
| `os.disk.max_await_ms` | scalar ms | Ok | `>= 2` | `>= 10` |
| `os.disk.read_await_ms` | scalar ms | Ok | `>= 2` | `>= 10` |
| `os.disk.write_await_ms` | scalar ms | Ok | `>= 2` | `>= 10` |
| `os.filesystem.free_capacity` | free capacity | Ok | fraction `< 0.20` and bytes `< 16106127360` | fraction `< 0.10` and bytes `< 16106127360` |
| `os.process.block_delay_seconds_delta` | scalar seconds | Inactive | `> 10` | `> 50` |
| `os.disk.blocks_read_per_second` | scalar count/s | Inactive | `> 0` | none |
| `os.network.errors_per_second` | scalar count/s | Inactive | `> 0` | `> 10` |
| `os.network.drops_per_second` | scalar count/s | Inactive | `> 0` | `> 10` |

Available bytes equal to `16106127360` do not cross the strict absolute ceiling.

- [ ] **Step 4: Add the eight PostgreSQL table/vacuum entries**

Extend the catalog in this exact order:

| ID | Policy | Zero/gate | Warning | Critical |
| --- | --- | --- | --- | --- |
| `pg.tables.dead_tuple_pct` | ratio with floor `count > 10000` | zero Ok | `>= 0.10` | `>= 0.20` |
| `pg.tables.dead_tuples` | scalar count | zero Ok | `>= 1000` | `>= 100000` |
| `pg.tables.sequential_scan_pct` | scalar percent | zero Ok | `>= 30` | `>= 80` |
| `pg.tables.modified_since_analyze` | scalar count | zero Ok | `>= 100000` | `>= 1000000` |
| `pg.tables.inserted_since_vacuum` | scalar count | zero Ok | `>= 100000` | `>= 1000000` |
| `pg.tables.autovacuum_age_seconds` | age | caller gate `dead > 0` | `> 21600` | `> 86400` |
| `pg.tables.autoanalyze_age_seconds` | age | caller gate `modified >= 10000` | `> 21600` | `> 86400` |
| `pg.tables.temp_bytes_per_second` | scalar bytes/s | zero Inactive | `> 0` | none |

Age policies use `ZeroDisposition::Classify`; a false gate returns `NotApplicable` before age evaluation.

- [ ] **Step 5: Complete invariant and invalid-input tests**

Add exact assertions for:

- final length 42;
- no duplicate enum IDs or wire codes;
- `MetricId::ALL` and canonical array index alignment;
- every entry provisional;
- every entry's `policy.input_kind()` matches its golden policy;
- representative correct input for every entry never returns `InputShapeMismatch`;
- wrong input shape for every entry returns `InputShapeMismatch`;
- `NaN`, positive infinity, and negative inputs return their exact reasons;
- free capacity with `available > total` returns `OutOfDomain`;
- both `0.0` and `-0.0` follow the same zero disposition.

Test allocations may use `Vec` and `BTreeSet`; production classification and catalog lookup may not.

- [ ] **Step 6: Run catalog, crate, formatting, and lint gates**

Run:

```bash
cargo test -p kronika-analytics --test threshold_catalog
cargo test -p kronika-analytics
cargo fmt --all --check
cargo clippy -p kronika-analytics --all-targets -- -D warnings
```

Expected: all pass and the integration test reports exactly 42 entries.

- [ ] **Step 7: Commit the complete catalog**

```bash
git add crates/kronika-analytics/src/threshold crates/kronika-analytics/tests/threshold_catalog.rs
git commit -m "feat: complete initial threshold catalog"
```

### Task 5: Contract Documentation and Workspace Verification

**Files:**
- Modify: `crates/kronika-analytics/README.md`
- Modify: `crates/kronika-analytics/README.ru.md`
- Modify: `crates/kronika-analytics/src/lib.rs`

**Interfaces:**
- Consumes: the final public threshold API and 42-entry catalog.
- Produces: synchronized English/Russian contract documentation and final verification evidence.

- [ ] **Step 1: Add the English README contract**

Add a `Class 1 threshold catalog` section stating:

- `threshold` classifies fixed-size inputs against 42 provisional built-in policies;
- `classify(MetricId, MetricInput)` returns a level, exact crossed boundary, and fixed-size evidence;
- missing, not-applicable, corrupt numeric data, invalid denominators, and adapter shape errors are distinct;
- the kernel is deterministic, I/O-free, clock-free, allocation-free, and O(1) per classification;
- the catalog is not yet connected to HTTP or the embedded UI;
- Class 2 modified z-score remains the separate `anomaly` module.

- [ ] **Step 2: Mirror the contract in Russian**

Add the same claims and limitations to `README.ru.md`. Preserve exact Rust identifiers and numeric counts. Do not introduce behavior absent from the code.

- [ ] **Step 3: Run focused documentation and public API checks**

Run:

```bash
cargo doc -p kronika-analytics --no-deps
cargo test -p kronika-analytics
RUSTFLAGS="-D warnings" cargo check -p kronika-analytics --all-targets
```

Expected: no rustdoc or warning failures.

- [ ] **Step 4: Run the complete project gates**

Run:

```bash
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```

Expected: every command exits zero.

- [ ] **Step 5: Perform the standing review passes**

Memory-bounds review:

- confirm `MetricInput`, `Evidence`, `Policy`, and `Classified` contain no `Vec`, `String`, map, boxed value, or reference-counted pointer;
- confirm `catalog()` returns one static slice and lookup does not clone it;
- confirm classification contains no input-sized loop or heap allocation;
- record that peak additional memory is bounded by fixed-size enum locals.

Comment-quality review:

- remove comments that paraphrase comparisons or assignments;
- retain rustdoc that specifies invalid-input behavior, boundary strictness, units, and invariants;
- ensure no code comment or rustdoc refers to a `docs/` path.

Scope review:

- confirm no web, OpenAPI, PGM/OVF, anomaly, incident, Cargo dependency, or lockfile changes;
- confirm all 42 entries remain provisional.

Run:

```bash
git diff --check
git status --short
git diff --stat
```

Expected: only planned source, tests, README, spec, and plan files differ from the branch base.

- [ ] **Step 6: Commit documentation and final adjustments**

```bash
git add crates/kronika-analytics/README.md crates/kronika-analytics/README.ru.md crates/kronika-analytics/src/lib.rs
git commit -m "docs: document threshold catalog contract"
```

- [ ] **Step 7: Verify the final branch state**

Run:

```bash
git status --short --branch
git log --oneline --decorate -8
```

Expected: clean worktree on `feat/absolute-threshold-catalog` with the design, plan, model, policy, catalog, tests, and README commits present.
