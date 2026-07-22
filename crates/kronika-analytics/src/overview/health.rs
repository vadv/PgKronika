//! Health evaluation: continuous pressure, trusted floors, and honest
//! unknowns.
//!
//! A health point separates three claims that must never blur. The continuous
//! score describes resource and operational pressure as a product of domain
//! complements. Floor evidence records trusted catastrophic observations —
//! a crash, proven integrity damage — that pin the state to `Critical`
//! regardless of pressure. And unknown stays unknown: when a required domain
//! has no proven coverage, both numeric scores are `None`; an empty bucket is
//! never a healthy `1.0`, and a missing factor never contributes a zero
//! penalty.
//!
//! Within a domain, correlated factors take the maximum penalty instead of
//! multiplying, so one physical cause reported through two meters is not
//! counted twice. Across domains, complements multiply into an ordinal
//! operational index — not a probability.

use std::cmp::Ordering;

use super::REGISTRY_CONTRACT_VERSION;
use super::coverage::{Applicability, CoverageSpan, CoverageState};
use super::observation::{FactId, LossReason};
use super::sha256;

/// Domain-separation tag of the factor-set preimage.
const FACTOR_SET_DOMAIN_TAG: &[u8] = b"pgk-overview-factor-set-v1";

/// Stable identity of one health factor in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactorId(pub u32);

/// A health domain: one axis of operational pressure.
///
/// The closed v1 set; the discriminant is the stable index used in the
/// factor-set preimage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DomainId {
    /// Joint severity/category/SQLSTATE counts and session failure deltas.
    DatabaseErrorPressure = 0,
    /// Connections against limits and retained 53300-like observations.
    ConnectionCapacity = 1,
    /// Blocked sessions, lock waits, deadlock deltas.
    Contention = 2,
    /// Host/cgroup CPU, PSI CPU, runnable pressure.
    CpuPressure = 3,
    /// PSI memory, cgroup usage against limits, `OOM` facts.
    MemoryPressure = 4,
    /// Disk I/O, proven mount capacity, temp and disk-full observations.
    StoragePressure = 5,
    /// Checkpoint pressure and XID/MXID headroom.
    Maintenance = 6,
    /// Lag, state, and slot loss where replication is declared applicable.
    Replication = 7,
}

impl DomainId {
    /// Every domain in stable-index order.
    pub const ALL: [Self; 8] = [
        Self::DatabaseErrorPressure,
        Self::ConnectionCapacity,
        Self::Contention,
        Self::CpuPressure,
        Self::MemoryPressure,
        Self::StoragePressure,
        Self::Maintenance,
        Self::Replication,
    ];

    /// The stable index, `0..8`.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The stable machine code of this domain on the wire.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::DatabaseErrorPressure => "database_error_pressure",
            Self::ConnectionCapacity => "connection_capacity",
            Self::Contention => "contention",
            Self::CpuPressure => "cpu_pressure",
            Self::MemoryPressure => "memory_pressure",
            Self::StoragePressure => "storage_pressure",
            Self::Maintenance => "maintenance",
            Self::Replication => "replication",
        }
    }
}

/// The combined state shown to a user, never erasing unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Required coverage is missing and no trusted floor exists.
    Unknown,
    /// No significant proven pressure.
    Normal,
    /// Proven pressure below the critical threshold.
    Degraded,
    /// A trusted floor or critical proven pressure.
    Critical,
}

/// A class of trusted catastrophic evidence.
///
/// Only evidence the source contract can prove belongs here: a lifecycle
/// crash or structured `PANIC` is availability evidence, a checksum failure
/// is integrity evidence. A lone signal 9, a heuristic category, or an
/// immediate shutdown without context is not representable as a floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FloorClass {
    /// The server or a child provably stopped serving.
    Availability = 0,
    /// Proven data or index integrity damage.
    Integrity = 1,
    /// A proven kernel or cgroup `OOM` kill.
    OomKill = 2,
    /// Proven exhaustion of a filesystem the server writes to.
    DiskFull = 3,
}

/// One piece of trusted floor evidence with the fact that proves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FloorEvidence {
    /// The evidence class.
    pub class: FloorClass,
    /// The fact that carries the proof.
    pub supporting_fact_id: FactId,
}

/// One factor's curved penalty inside its domain.
///
/// The penalty is already normalized by the factor's monotonic curve; this
/// type only guarantees it stays finite and inside `[0, 1]`, which keeps
/// every derived score bounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactorPenalty {
    /// The factor this penalty came from.
    pub factor_id: FactorId,
    /// The domain the factor belongs to.
    pub domain: DomainId,
    /// The reduced fact the penalty was computed from.
    pub supporting_fact_id: FactId,
    penalty: f64,
}

impl FactorPenalty {
    /// Builds a penalty, rejecting a non-finite or out-of-range value.
    #[must_use]
    pub fn new(
        factor_id: FactorId,
        domain: DomainId,
        penalty: f64,
        supporting_fact_id: FactId,
    ) -> Option<Self> {
        (penalty.is_finite() && (0.0..=1.0).contains(&penalty)).then_some(Self {
            factor_id,
            domain,
            supporting_fact_id,
            penalty,
        })
    }

    /// The normalized penalty, finite in `[0, 1]`.
    #[must_use]
    pub const fn penalty(&self) -> f64 {
        self.penalty
    }
}

/// One domain's combined penalty and the factors that drive it.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainPenalty {
    /// The domain.
    pub domain: DomainId,
    /// The maximum factor penalty in the domain, in `[0, 1]`.
    pub penalty: f64,
    /// The factors whose penalty equals the domain penalty, sorted.
    pub driving_factor_ids: Vec<FactorId>,
}

/// The retained population behind a top-N factor input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePopulation {
    /// How many members were retained.
    pub collected: u64,
    /// How many members the source reported in total.
    pub total: u64,
}

/// How exact a factor's retained input is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exactness {
    /// Every retained record is counted exactly once.
    RetainedExact,
    /// Bounds dropped records; retained values are a lower bound.
    LowerBound,
    /// Exactness cannot be determined.
    Unknown,
}

/// The coverage of one factor over one evaluation interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactorCoverage {
    /// The factor.
    pub factor_id: FactorId,
    /// Whether the factor applies to this source at all.
    pub applicability: Applicability,
    /// The coverage state, keeping gap and not-collected apart from zero.
    pub state: CoverageState,
    /// The interval the entry describes.
    pub interval: CoverageSpan,
    /// The declared collection period, when the source has one.
    pub expected_period_us: Option<u64>,
    /// How many samples are actually present.
    pub present_samples: u64,
    /// How many microseconds of the interval are covered.
    pub covered_duration_us: u64,
    /// Population completeness behind a top-N input, when bounded.
    pub source_population: Option<SourcePopulation>,
    /// Proven loss reasons, sorted and unique.
    pub loss_reasons: Vec<LossReason>,
    /// The proven minimum number of lost records, when counted.
    pub lost_count_lower_bound: Option<u64>,
    /// How exact the retained input is.
    pub exactness: Exactness,
}

/// Identity of the exact factor set that produced a health point.
///
/// Scores are comparable only under the same policy version and factor-set
/// identity; a disappeared optional factor changes the ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FactorSetId(pub [u8; 16]);

impl FactorSetId {
    /// Derives the identity from the policy and the participating factors.
    ///
    /// The preimage covers the policy version, profile, the crate's registry
    /// contract version, and the ordered `(factor, domain)` pairs that
    /// actually participated; every field is fixed-width little-endian.
    #[must_use]
    pub fn derive(
        health_policy_version: u32,
        profile_id: u32,
        participating: &[(FactorId, DomainId)],
    ) -> Self {
        let mut parts: Vec<[u8; 8]> = Vec::with_capacity(participating.len());
        for &(factor, domain) in participating {
            let mut pair = [0_u8; 8];
            pair[..4].copy_from_slice(&factor.0.to_le_bytes());
            pair[4..].copy_from_slice(&(domain as u32).to_le_bytes());
            parts.push(pair);
        }
        let policy_version = health_policy_version.to_le_bytes();
        let profile = profile_id.to_le_bytes();
        let registry = REGISTRY_CONTRACT_VERSION.to_le_bytes();
        let mut preimage: Vec<&[u8]> =
            vec![FACTOR_SET_DOMAIN_TAG, &policy_version, &profile, &registry];
        preimage.extend(parts.iter().map(<[u8; 8]>::as_slice));
        let digest = sha256::digest_parts(&preimage);
        let mut id = [0_u8; 16];
        id.copy_from_slice(&digest[..16]);
        Self(id)
    }
}

/// Which factors a profile requires before a score may exist.
#[derive(Debug, Clone, PartialEq)]
pub struct RequiredFactorProfile {
    /// Stable identity of the profile.
    pub profile_id: u32,
    /// Domains that must be known for any numeric score.
    pub required_domains: Vec<DomainId>,
    /// The factors each required domain needs, when applicable.
    pub required_factors_by_domain: Vec<(DomainId, Vec<FactorId>)>,
    /// Factors that refine the score but never block it.
    pub optional_factors: Vec<FactorId>,
    /// Minimum covered ratio per factor for partial coverage to count.
    ///
    /// A required factor with no declared ratio counts only when its
    /// coverage is `Complete`; no threshold is invented for it.
    pub minimum_covered_ratio_by_factor: Vec<(FactorId, f64)>,
}

/// The versioned health policy: profile plus state thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthPolicy {
    /// The policy version stamped into every produced point.
    pub version: u32,
    /// The required-coverage profile.
    pub profile: RequiredFactorProfile,
    /// Scores below this are `Degraded`; expected above `critical_below`.
    pub degraded_below: f64,
    /// Scores below this are `Critical`.
    pub critical_below: f64,
}

/// One evaluated health point over one interval.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthPoint {
    /// The evaluated interval.
    pub interval: CoverageSpan,
    /// Continuous pressure score, `None` when required coverage is missing.
    pub continuous_score: Option<f64>,
    /// The combined score: zero under a trusted floor, else continuous.
    pub overall_score: Option<f64>,
    /// The combined state, never erasing unknown.
    pub overall_state: HealthState,
    /// The policy version that produced this point.
    pub health_policy_version: u32,
    /// Identity of the factor set that actually participated.
    pub factor_set_id: FactorSetId,
    /// Normalized factor penalties, deterministically ordered.
    pub factor_penalties: Vec<FactorPenalty>,
    /// Domain penalties with their driving factors, in domain order.
    pub domain_penalties: Vec<DomainPenalty>,
    /// Trusted floor evidence, sorted and unique.
    pub floor_evidence: Vec<FloorEvidence>,
    /// Per-factor coverage of this interval.
    pub coverage: Vec<FactorCoverage>,
}

impl HealthPolicy {
    /// Maps a known overall score to a state.
    #[must_use]
    pub fn state_from_score(&self, score: f64) -> HealthState {
        if score < self.critical_below {
            HealthState::Critical
        } else if score < self.degraded_below {
            HealthState::Degraded
        } else {
            HealthState::Normal
        }
    }

    /// Evaluates one co-temporal cell into a health point.
    ///
    /// The decision table: a missing required domain makes both numeric
    /// scores `None` — with a trusted floor the state is still `Critical`,
    /// otherwise `Unknown`. With full required coverage the continuous score
    /// is the product of domain complements; a trusted floor forces the
    /// overall score to `0.0` and the state to `Critical`.
    ///
    /// Every `FactorPenalty` presupposes a factor reduced from real covered
    /// samples; an uncovered factor must not appear here at all.
    #[must_use]
    pub fn evaluate_cell(
        &self,
        interval: CoverageSpan,
        factors: &[FactorPenalty],
        coverage: Vec<FactorCoverage>,
        floor_evidence: Vec<FloorEvidence>,
    ) -> HealthPoint {
        let mut factor_penalties = factors.to_vec();
        factor_penalties.sort_unstable_by(order_factor_penalties);

        let domain_penalties = domain_penalties_of(&factor_penalties);
        let floor_evidence = normalized_floors(floor_evidence);
        let has_floor = !floor_evidence.is_empty();

        let participating: Vec<(FactorId, DomainId)> = factor_penalties
            .iter()
            .map(|p| (p.factor_id, p.domain))
            .collect();
        let factor_set_id =
            FactorSetId::derive(self.version, self.profile.profile_id, &participating);

        let (continuous_score, overall_score, overall_state);
        if self.required_domains_known(interval, &coverage) {
            let product: f64 = domain_penalties.iter().map(|d| 1.0 - d.penalty).product();
            continuous_score = Some(product);
            overall_score = Some(if has_floor { 0.0 } else { product });
            overall_state = if has_floor {
                HealthState::Critical
            } else {
                self.state_from_score(product)
            };
        } else {
            continuous_score = None;
            overall_score = None;
            overall_state = if has_floor {
                HealthState::Critical
            } else {
                HealthState::Unknown
            };
        }

        HealthPoint {
            interval,
            continuous_score,
            overall_score,
            overall_state,
            health_policy_version: self.version,
            factor_set_id,
            factor_penalties,
            domain_penalties,
            floor_evidence,
            coverage,
        }
    }

    /// Whether every required domain has proven coverage in the cell.
    fn required_domains_known(&self, interval: CoverageSpan, coverage: &[FactorCoverage]) -> bool {
        self.profile.required_domains.iter().all(|&domain| {
            let required = self
                .profile
                .required_factors_by_domain
                .iter()
                .find(|(d, _)| *d == domain)
                .map(|(_, factors)| factors.as_slice())
                .unwrap_or_default();
            required
                .iter()
                .all(|&factor| self.factor_is_covered(factor, interval, coverage))
        })
    }

    /// Whether one required factor is applicably and sufficiently covered.
    fn factor_is_covered(
        &self,
        factor: FactorId,
        interval: CoverageSpan,
        coverage: &[FactorCoverage],
    ) -> bool {
        let Some(entry) = coverage.iter().find(|c| c.factor_id == factor) else {
            return false;
        };
        match entry.applicability {
            // Not applicable: the factor does not block the domain.
            Applicability::NotApplicable => return true,
            Applicability::Unsupported => return false,
            Applicability::Applicable => {}
        }
        match entry.state {
            CoverageState::Complete => true,
            CoverageState::Partial => self.partial_coverage_suffices(factor, interval, entry),
            CoverageState::Gap | CoverageState::Unknown | CoverageState::NotCollected => false,
        }
    }

    /// Whether partial coverage clears the factor's declared minimum ratio.
    #[allow(
        clippy::cast_precision_loss,
        reason = "durations stay far below 2^53 microseconds, so the f64 \
                  ratio is exact"
    )]
    fn partial_coverage_suffices(
        &self,
        factor: FactorId,
        interval: CoverageSpan,
        entry: &FactorCoverage,
    ) -> bool {
        let Some(&(_, minimum)) = self
            .profile
            .minimum_covered_ratio_by_factor
            .iter()
            .find(|(f, _)| *f == factor)
        else {
            return false;
        };
        let ratio = entry.covered_duration_us as f64 / interval.duration_us() as f64;
        ratio >= minimum
    }
}

/// Worst-point downsample of fine points into one bucket point.
///
/// The bucket takes the scores and penalties of the single computed point
/// with the minimum overall score — never component-wise maxima from
/// different moments. Floor evidence from every point carries into the
/// bucket. When no point has a numeric score, the bucket stays `Unknown`
/// (or `Critical` under a floor) with no invented penalties.
///
/// All points must come from the same policy version; returns `None` for an
/// empty input.
#[must_use]
pub fn downsample_worst(points: &[HealthPoint], bucket: CoverageSpan) -> Option<HealthPoint> {
    let first = points.first()?;

    let mut floors: Vec<FloorEvidence> = points
        .iter()
        .flat_map(|p| p.floor_evidence.iter().copied())
        .collect();
    floors = normalized_floors(floors);
    let has_floor = !floors.is_empty();

    let worst = points
        .iter()
        .filter(|p| p.overall_score.is_some())
        .min_by(|a, b| {
            let (Some(a_score), Some(b_score)) = (a.overall_score, b.overall_score) else {
                return Ordering::Equal;
            };
            a_score.total_cmp(&b_score)
        });

    let mut point = worst.map_or_else(
        || HealthPoint {
            interval: bucket,
            continuous_score: None,
            overall_score: None,
            overall_state: HealthState::Unknown,
            health_policy_version: first.health_policy_version,
            factor_set_id: first.factor_set_id,
            factor_penalties: Vec::new(),
            domain_penalties: Vec::new(),
            floor_evidence: Vec::new(),
            coverage: Vec::new(),
        },
        |p| HealthPoint {
            interval: bucket,
            ..p.clone()
        },
    );

    if has_floor {
        point.overall_state = HealthState::Critical;
        point.overall_score = point.overall_score.map(|_| 0.0);
    }
    point.floor_evidence = floors;
    Some(point)
}

/// Deterministic order of factor penalties inside a point.
fn order_factor_penalties(a: &FactorPenalty, b: &FactorPenalty) -> Ordering {
    (a.domain, a.factor_id, a.supporting_fact_id)
        .cmp(&(b.domain, b.factor_id, b.supporting_fact_id))
        .then_with(|| a.penalty.total_cmp(&b.penalty))
}

/// Sorted, deduplicated floor evidence.
fn normalized_floors(mut floors: Vec<FloorEvidence>) -> Vec<FloorEvidence> {
    floors.sort_unstable();
    floors.dedup();
    floors
}

/// Per-domain maximum penalties with their driving factors.
///
/// The maximum, not a product: correlated meters of one physical cause must
/// not multiply into stronger evidence. A fact cited by several factors of
/// the domain counts once by construction of the maximum.
fn domain_penalties_of(factor_penalties: &[FactorPenalty]) -> Vec<DomainPenalty> {
    let mut out: Vec<DomainPenalty> = Vec::new();
    for domain in DomainId::ALL {
        let in_domain = factor_penalties.iter().filter(|p| p.domain == domain);
        let Some(max) = in_domain.clone().map(|p| p.penalty).max_by(f64::total_cmp) else {
            continue;
        };
        let mut driving_factor_ids: Vec<FactorId> = in_domain
            .filter(|p| p.penalty.total_cmp(&max) == Ordering::Equal)
            .map(|p| p.factor_id)
            .collect();
        driving_factor_ids.sort_unstable();
        driving_factor_ids.dedup();
        out.push(DomainPenalty {
            domain,
            penalty: max,
            driving_factor_ids,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "scores asserted are exact products of dyadic penalties"
    )]

    use super::*;

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid span in fixture")
    }

    fn policy(required: &[DomainId]) -> HealthPolicy {
        HealthPolicy {
            version: 1,
            profile: RequiredFactorProfile {
                profile_id: 1,
                required_domains: required.to_vec(),
                required_factors_by_domain: required
                    .iter()
                    .map(|&d| (d, vec![factor_of(d)]))
                    .collect(),
                optional_factors: Vec::new(),
                minimum_covered_ratio_by_factor: vec![(FactorId(100), 0.5)],
            },
            degraded_below: 0.9,
            critical_below: 0.5,
        }
    }

    /// One synthetic required factor per domain: factor 10 * index.
    fn factor_of(domain: DomainId) -> FactorId {
        FactorId(10 * (domain as u32 + 1))
    }

    fn complete_coverage(domain: DomainId, interval: CoverageSpan) -> FactorCoverage {
        FactorCoverage {
            factor_id: factor_of(domain),
            applicability: Applicability::Applicable,
            state: CoverageState::Complete,
            interval,
            expected_period_us: Some(1_000_000),
            present_samples: 10,
            covered_duration_us: interval.duration_us(),
            source_population: None,
            loss_reasons: Vec::new(),
            lost_count_lower_bound: None,
            exactness: Exactness::RetainedExact,
        }
    }

    fn penalty(factor: u32, domain: DomainId, value: f64, fact: u8) -> FactorPenalty {
        FactorPenalty::new(FactorId(factor), domain, value, FactId([fact; 32]))
            .expect("valid penalty in fixture")
    }

    fn floor(class: FloorClass, fact: u8) -> FloorEvidence {
        FloorEvidence {
            class,
            supporting_fact_id: FactId([fact; 32]),
        }
    }

    #[test]
    fn penalty_rejects_non_finite_and_out_of_range_values() {
        for bad in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(
                FactorPenalty::new(FactorId(1), DomainId::Contention, bad, FactId([0; 32]))
                    .is_none(),
                "accepted {bad}"
            );
        }
        assert!(
            FactorPenalty::new(FactorId(1), DomainId::Contention, 1.0, FactId([0; 32])).is_some()
        );
    }

    #[test]
    fn score_stays_finite_and_bounded_over_a_penalty_sweep() {
        let interval = span(0, 60_000_000);
        let p = policy(&[DomainId::Contention]);
        let coverage = vec![complete_coverage(DomainId::Contention, interval)];
        let grid = [0.0, 0.25, 0.5, 0.75, 1.0];
        for a in grid {
            for b in grid {
                for c in grid {
                    let factors = [
                        penalty(1, DomainId::Contention, a, 1),
                        penalty(2, DomainId::MemoryPressure, b, 2),
                        penalty(3, DomainId::CpuPressure, c, 3),
                    ];
                    let point = p.evaluate_cell(interval, &factors, coverage.clone(), Vec::new());
                    let score = point.overall_score.expect("required domain is covered");
                    assert!(score.is_finite());
                    assert!((0.0..=1.0).contains(&score), "score {score} out of bounds");
                    assert_eq!(point.continuous_score, point.overall_score);
                }
            }
        }
    }

    #[test]
    fn across_domains_complements_multiply() {
        let interval = span(0, 1_000_000);
        let p = policy(&[]);
        let factors = [
            penalty(1, DomainId::Contention, 0.5, 1),
            penalty(2, DomainId::MemoryPressure, 0.25, 2),
        ];
        let point = p.evaluate_cell(interval, &factors, Vec::new(), Vec::new());
        assert_eq!(point.continuous_score, Some(0.5 * 0.75));
    }

    #[test]
    fn within_a_domain_the_maximum_wins_not_the_product() {
        let interval = span(0, 1_000_000);
        let p = policy(&[]);
        // Two correlated meters of one chain: 0.5 and 0.4 must give 0.5,
        // not 1 - 0.5*0.6 = 0.7.
        let factors = [
            penalty(1, DomainId::MemoryPressure, 0.5, 1),
            penalty(2, DomainId::MemoryPressure, 0.4, 2),
        ];
        let point = p.evaluate_cell(interval, &factors, Vec::new(), Vec::new());
        assert_eq!(point.continuous_score, Some(0.5));
        assert_eq!(point.domain_penalties.len(), 1);
        assert_eq!(
            point.domain_penalties[0].driving_factor_ids,
            vec![FactorId(1)]
        );
    }

    #[test]
    fn one_supporting_fact_is_never_double_counted() {
        let interval = span(0, 1_000_000);
        let p = policy(&[]);
        let base = [penalty(1, DomainId::Contention, 0.5, 7)];
        let with_duplicate = [
            penalty(1, DomainId::Contention, 0.5, 7),
            // The same fact reported through a second factor of the domain.
            penalty(2, DomainId::Contention, 0.5, 7),
        ];
        let a = p.evaluate_cell(interval, &base, Vec::new(), Vec::new());
        let b = p.evaluate_cell(interval, &with_duplicate, Vec::new(), Vec::new());
        assert_eq!(a.continuous_score, b.continuous_score);
    }

    #[test]
    fn raising_one_penalty_in_a_fixed_set_never_raises_the_score() {
        let interval = span(0, 1_000_000);
        let p = policy(&[]);
        let grid = [0.0, 0.1, 0.3, 0.6, 0.9, 1.0];
        for low in grid {
            for high in grid {
                if low > high {
                    continue;
                }
                let weaker = [
                    penalty(1, DomainId::Contention, low, 1),
                    penalty(2, DomainId::CpuPressure, 0.3, 2),
                ];
                let stronger = [
                    penalty(1, DomainId::Contention, high, 1),
                    penalty(2, DomainId::CpuPressure, 0.3, 2),
                ];
                let weak = p.evaluate_cell(interval, &weaker, Vec::new(), Vec::new());
                let strong = p.evaluate_cell(interval, &stronger, Vec::new(), Vec::new());
                assert!(
                    strong.continuous_score <= weak.continuous_score,
                    "penalty {low} -> {high} raised the score"
                );
            }
        }
    }

    #[test]
    fn factor_permutation_changes_nothing() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::Contention]);
        let coverage = vec![complete_coverage(DomainId::Contention, interval)];
        let factors = [
            penalty(3, DomainId::CpuPressure, 0.2, 3),
            penalty(1, DomainId::Contention, 0.5, 1),
            penalty(2, DomainId::MemoryPressure, 0.4, 2),
        ];
        let mut reversed = factors;
        reversed.reverse();
        let a = p.evaluate_cell(interval, &factors, coverage.clone(), Vec::new());
        let b = p.evaluate_cell(interval, &reversed, coverage, Vec::new());
        assert_eq!(a, b);
    }

    #[test]
    fn missing_required_domain_gives_none_scores_never_one() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::MemoryPressure]);
        // Pressure elsewhere is known, but the required domain is not.
        let factors = [penalty(1, DomainId::Contention, 0.0, 1)];
        let point = p.evaluate_cell(interval, &factors, Vec::new(), Vec::new());
        assert_eq!(point.continuous_score, None);
        assert_eq!(point.overall_score, None);
        assert_eq!(point.overall_state, HealthState::Unknown);
    }

    #[test]
    fn gap_and_not_collected_and_unsupported_block_a_required_domain() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::MemoryPressure]);
        for (state, applicability) in [
            (CoverageState::Gap, Applicability::Applicable),
            (CoverageState::Unknown, Applicability::Applicable),
            (CoverageState::NotCollected, Applicability::Applicable),
            (CoverageState::Complete, Applicability::Unsupported),
        ] {
            let mut entry = complete_coverage(DomainId::MemoryPressure, interval);
            entry.state = state;
            entry.applicability = applicability;
            let point = p.evaluate_cell(interval, &[], vec![entry], Vec::new());
            assert_eq!(point.overall_score, None, "{state:?}/{applicability:?}");
            assert_eq!(point.overall_state, HealthState::Unknown);
        }
    }

    #[test]
    fn a_not_applicable_required_factor_does_not_block_the_domain() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::Replication]);
        let mut entry = complete_coverage(DomainId::Replication, interval);
        entry.applicability = Applicability::NotApplicable;
        entry.state = CoverageState::NotCollected;
        let point = p.evaluate_cell(interval, &[], vec![entry], Vec::new());
        assert_eq!(point.overall_score, Some(1.0));
        assert_eq!(point.overall_state, HealthState::Normal);
    }

    #[test]
    fn partial_coverage_counts_only_above_the_declared_ratio() {
        let interval = span(0, 1_000_000);
        let mut p = policy(&[DomainId::Contention]);
        p.profile.required_factors_by_domain = vec![(DomainId::Contention, vec![FactorId(100)])];
        let mut entry = complete_coverage(DomainId::Contention, interval);
        entry.factor_id = FactorId(100);
        entry.state = CoverageState::Partial;

        entry.covered_duration_us = 600_000; // 0.6 >= declared 0.5
        let known = p.evaluate_cell(interval, &[], vec![entry.clone()], Vec::new());
        assert_eq!(known.overall_score, Some(1.0));

        entry.covered_duration_us = 400_000; // 0.4 < declared 0.5
        let unknown = p.evaluate_cell(interval, &[], vec![entry.clone()], Vec::new());
        assert_eq!(unknown.overall_score, None);

        // No declared ratio: partial coverage is never sufficient.
        entry.factor_id = factor_of(DomainId::Contention);
        entry.covered_duration_us = 999_999;
        p.profile.required_factors_by_domain =
            vec![(DomainId::Contention, vec![factor_of(DomainId::Contention)])];
        let strict = p.evaluate_cell(interval, &[], vec![entry], Vec::new());
        assert_eq!(strict.overall_score, None);
    }

    #[test]
    fn a_trusted_floor_with_full_coverage_zeroes_the_overall_score() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::Contention]);
        let coverage = vec![complete_coverage(DomainId::Contention, interval)];
        let factors = [penalty(1, DomainId::Contention, 0.25, 1)];
        let point = p.evaluate_cell(
            interval,
            &factors,
            coverage,
            vec![floor(FloorClass::Availability, 9)],
        );
        // The continuous pressure stays what it was; the floor zeroes only
        // the overall score.
        assert_eq!(point.continuous_score, Some(0.75));
        assert_eq!(point.overall_score, Some(0.0));
        assert_eq!(point.overall_state, HealthState::Critical);
    }

    #[test]
    fn a_trusted_floor_without_required_coverage_is_critical_with_none_scores() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::MemoryPressure]);
        let point = p.evaluate_cell(
            interval,
            &[],
            Vec::new(),
            vec![floor(FloorClass::OomKill, 3)],
        );
        assert_eq!(point.continuous_score, None);
        assert_eq!(point.overall_score, None);
        assert_eq!(point.overall_state, HealthState::Critical);
    }

    #[test]
    fn domain_codes_are_the_stable_wire_codes() {
        let cases = [
            (DomainId::DatabaseErrorPressure, "database_error_pressure"),
            (DomainId::ConnectionCapacity, "connection_capacity"),
            (DomainId::Contention, "contention"),
            (DomainId::CpuPressure, "cpu_pressure"),
            (DomainId::MemoryPressure, "memory_pressure"),
            (DomainId::StoragePressure, "storage_pressure"),
            (DomainId::Maintenance, "maintenance"),
            (DomainId::Replication, "replication"),
        ];
        for (domain, code) in cases {
            assert_eq!(domain.code(), code);
        }
    }

    #[test]
    fn a_floor_among_unknown_points_still_pins_the_bucket_critical() {
        let p = policy(&[DomainId::MemoryPressure]);
        let bucket = span(0, 2_000_000);
        // Neither point has required coverage; one carries a trusted floor.
        let unknown = p.evaluate_cell(span(0, 1_000_000), &[], Vec::new(), Vec::new());
        let crashed = p.evaluate_cell(
            span(1_000_000, 2_000_000),
            &[],
            Vec::new(),
            vec![floor(FloorClass::Availability, 9)],
        );
        let bucketed = downsample_worst(&[unknown, crashed], bucket).expect("non-empty input");
        assert_eq!(bucketed.overall_score, None);
        assert_eq!(bucketed.overall_state, HealthState::Critical);
        assert_eq!(
            bucketed.floor_evidence,
            vec![floor(FloorClass::Availability, 9)]
        );
    }

    #[test]
    fn state_thresholds_map_scores_in_order() {
        let p = policy(&[]);
        assert_eq!(p.state_from_score(0.95), HealthState::Normal);
        assert_eq!(p.state_from_score(0.7), HealthState::Degraded);
        assert_eq!(p.state_from_score(0.2), HealthState::Critical);
    }

    #[test]
    fn factor_set_id_changes_when_a_factor_disappears() {
        let both = [
            (FactorId(1), DomainId::Contention),
            (FactorId(2), DomainId::CpuPressure),
        ];
        let full = FactorSetId::derive(1, 1, &both);
        assert_eq!(full, FactorSetId::derive(1, 1, &both));
        assert_ne!(full, FactorSetId::derive(1, 1, &both[..1]));
        assert_ne!(full, FactorSetId::derive(2, 1, &both));
    }

    #[test]
    fn downsample_picks_the_worst_computed_point_wholesale() {
        let p = policy(&[]);
        let bucket = span(0, 2_000_000);
        let mild = p.evaluate_cell(
            span(0, 1_000_000),
            &[penalty(1, DomainId::Contention, 0.1, 1)],
            Vec::new(),
            Vec::new(),
        );
        let severe = p.evaluate_cell(
            span(1_000_000, 2_000_000),
            &[penalty(2, DomainId::CpuPressure, 0.8, 2)],
            Vec::new(),
            Vec::new(),
        );
        let bucketed = downsample_worst(&[mild, severe.clone()], bucket).expect("non-empty input");
        assert_eq!(bucketed.interval, bucket);
        assert_eq!(bucketed.overall_score, severe.overall_score);
        // The penalties come from the same single point — no cross-moment
        // mixture with the mild point's contention factor.
        assert_eq!(bucketed.factor_penalties, severe.factor_penalties);
        assert_eq!(bucketed.domain_penalties, severe.domain_penalties);
    }

    #[test]
    fn floor_evidence_survives_downsample_from_any_point() {
        let p = policy(&[]);
        let bucket = span(0, 2_000_000);
        // The floor sits in the *better*-scored point: it must still carry.
        let crashed = p.evaluate_cell(
            span(0, 1_000_000),
            &[],
            Vec::new(),
            vec![floor(FloorClass::Availability, 9)],
        );
        let pressured = p.evaluate_cell(
            span(1_000_000, 2_000_000),
            &[penalty(2, DomainId::CpuPressure, 0.8, 2)],
            Vec::new(),
            Vec::new(),
        );
        let bucketed = downsample_worst(&[pressured, crashed], bucket).expect("non-empty input");
        assert_eq!(
            bucketed.floor_evidence,
            vec![floor(FloorClass::Availability, 9)]
        );
        assert_eq!(bucketed.overall_state, HealthState::Critical);
        assert_eq!(bucketed.overall_score, Some(0.0));
    }

    #[test]
    fn downsample_of_unknown_points_stays_unknown_not_healthy() {
        let p = policy(&[DomainId::MemoryPressure]);
        let bucket = span(0, 2_000_000);
        let unknown = p.evaluate_cell(span(0, 1_000_000), &[], Vec::new(), Vec::new());
        let bucketed = downsample_worst(&[unknown], bucket).expect("non-empty input");
        assert_eq!(bucketed.overall_score, None);
        assert_eq!(bucketed.overall_state, HealthState::Unknown);
        assert!(downsample_worst(&[], bucket).is_none());
    }
}
