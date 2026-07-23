//! Versioned health evaluation with strict coverage eligibility.
//!
//! A required applicable factor contributes an explicit finite penalty,
//! including zero, only when its full interval coverage and provenance are
//! acceptable. Partial, lossy, mismatched, or assumed-current coverage keeps
//! the numeric score unknown. Floor evidence is carried independently.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::REGISTRY_CONTRACT_VERSION;
use super::coverage::{
    Applicability, BoundaryQuality, CoverageSpan, CoverageState, PeriodQuality,
    PhysicalCountSemantics, RetainedExactness, SourceCompleteness,
};
use super::observation::{FactId, LossReason};
use super::sha256;

const FACTOR_SET_DOMAIN_TAG: &[u8] = b"pgk-overview-factor-set-v1";
const PROFILE_TOPOLOGY_DOMAIN_TAG: &[u8] = b"pgk-overview-profile-topology-v1";

/// Stable identity of one health factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactorId(pub u32);

/// Identity of one collection-cadence epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CadenceEpochId(pub [u8; 16]);

/// Work and output limits for health operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the max_ prefix distinguishes hard caps in the public limits API"
)]
pub struct HealthLimits {
    /// Maximum factors in one profile.
    pub max_profile_factors: usize,
    /// Maximum penalties in one cell.
    pub max_cell_factors: usize,
    /// Maximum coverage records in one cell.
    pub max_coverage_entries: usize,
    /// Maximum floor records in one cell or downsampled bucket.
    pub max_floor_evidence: usize,
    /// Maximum fine points scanned by downsampling.
    pub max_downsample_points: usize,
}

/// Health input that exceeded a configured limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthResource {
    /// Profile factor topology.
    ProfileFactors,
    /// Cell penalties.
    CellFactors,
    /// Cell coverage records.
    CoverageEntries,
    /// Loss-reason entries in one coverage record.
    LossReasons,
    /// Floor records.
    FloorEvidence,
    /// Fine points passed to downsampling.
    DownsamplePoints,
}

/// One operational pressure domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DomainId {
    /// Database errors and session failures.
    DatabaseErrorPressure = 0,
    /// Connection capacity.
    ConnectionCapacity = 1,
    /// Locking and blocked work.
    Contention = 2,
    /// CPU pressure.
    CpuPressure = 3,
    /// Memory pressure.
    MemoryPressure = 4,
    /// Storage pressure.
    StoragePressure = 5,
    /// Checkpoint and transaction-ID maintenance.
    Maintenance = 6,
    /// Replication health when applicable.
    Replication = 7,
}

impl DomainId {
    /// Domains in stable index order.
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

    /// Stable index in `0..8`.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Stable locale-neutral machine code.
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

/// Health state shown to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Required evidence is incomplete.
    Unknown,
    /// Score is at or above the degraded threshold.
    Normal,
    /// Score is below the degraded threshold.
    Degraded,
    /// Score is below the critical threshold or a floor is present.
    Critical,
}

/// Class assigned by a versioned policy to proven catastrophic evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FloorClass {
    /// Proven service unavailability.
    Availability = 0,
    /// Proven integrity damage.
    Integrity = 1,
    /// Proven kernel or cgroup OOM kill.
    OomKill = 2,
    /// Proven exhaustion of a writable filesystem.
    DiskFull = 3,
}

/// Policy-validated floor evidence.
///
/// The type records a decision already made by the versioned policy. A raw
/// signal, severity, or arbitrary [`FactId`] does not establish a floor by
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FloorEvidence {
    /// Assigned floor class.
    pub class: FloorClass,
    /// Fact containing the proof.
    pub supporting_fact_id: FactId,
}

/// One normalized factor penalty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactorPenalty {
    factor_id: FactorId,
    domain: DomainId,
    supporting_fact_id: FactId,
    penalty: f64,
}

impl FactorPenalty {
    /// Builds a finite penalty in `[0, 1]`.
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
            penalty: if penalty == 0.0 { 0.0 } else { penalty },
        })
    }

    /// Factor identity.
    #[must_use]
    pub const fn factor_id(self) -> FactorId {
        self.factor_id
    }

    /// Factor domain.
    #[must_use]
    pub const fn domain(self) -> DomainId {
        self.domain
    }

    /// Supporting fact.
    #[must_use]
    pub const fn supporting_fact_id(self) -> FactId {
        self.supporting_fact_id
    }

    /// Normalized penalty.
    #[must_use]
    pub const fn penalty(self) -> f64 {
        self.penalty
    }
}

/// Maximum penalty selected for one domain.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainPenalty {
    domain: DomainId,
    penalty: f64,
    driving_factor_ids: Vec<FactorId>,
}

impl DomainPenalty {
    /// Domain identity.
    #[must_use]
    pub const fn domain(&self) -> DomainId {
        self.domain
    }

    /// Maximum factor penalty.
    #[must_use]
    pub const fn penalty(&self) -> f64 {
        self.penalty
    }

    /// Sorted factor IDs tied for the maximum.
    #[must_use]
    pub fn driving_factor_ids(&self) -> &[FactorId] {
        &self.driving_factor_ids
    }
}

/// Quality of a source population total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationTotalQuality {
    /// Exact source total.
    Exact,
    /// Lower bound on the source total.
    LowerBound,
    /// Provenance of the total is unknown.
    Unknown,
}

/// Population behind a bounded factor input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePopulation {
    /// Members retained.
    pub collected: u64,
    /// Source total, when reported.
    pub total: Option<u64>,
    /// Quality of `total`.
    pub total_quality: PopulationTotalQuality,
}

/// Evidence coverage for one factor and interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactorCoverage {
    /// Factor identity.
    pub factor_id: FactorId,
    /// Applicability to the source.
    pub applicability: Applicability,
    /// Coverage state.
    pub state: CoverageState,
    /// Exact interval described by this record.
    pub interval: CoverageSpan,
    /// Declared collection period.
    pub expected_period_us: Option<u64>,
    /// Provenance of the period.
    pub period_quality: PeriodQuality,
    /// Stable cadence epoch, when proven.
    pub cadence_epoch_id: Option<CadenceEpochId>,
    /// Whether a cadence/configuration boundary crosses the interval.
    pub crosses_cadence_boundary: bool,
    /// Present sample count.
    pub present_samples: u64,
    /// Exact covered duration inside `interval`.
    pub covered_duration_us: u64,
    /// Population information for bounded inputs.
    pub source_population: Option<SourcePopulation>,
    /// Sorted, unique proven loss reasons.
    pub loss_reasons: Vec<LossReason>,
    /// Proven lower bound on lost records.
    pub lost_count_lower_bound: Option<u64>,
    /// Exactness of retained values.
    pub retained_exactness: RetainedExactness,
    /// Completeness of the source population.
    pub source_completeness: SourceCompleteness,
    /// Semantics of physical counts used by the factor.
    pub physical_count_semantics: PhysicalCountSemantics,
    /// Pair-boundary attribution.
    pub boundary_quality: BoundaryQuality,
}

impl FactorCoverage {
    fn is_consistent(&self) -> bool {
        let population_is_consistent = self.source_population.is_none_or(|population| {
            population
                .total
                .is_none_or(|total| population.collected <= total)
                && (population.total_quality != PopulationTotalQuality::Exact
                    || population.total.is_some())
        });
        let loss_is_consistent = self
            .lost_count_lower_bound
            .is_none_or(|count| count == 0 || !self.loss_reasons.is_empty());
        let state_is_consistent = match self.applicability {
            Applicability::NotApplicable => self.is_valid_not_applicable(self.interval),
            Applicability::Unsupported => {
                matches!(
                    self.state,
                    CoverageState::Unknown | CoverageState::NotCollected
                ) && self.present_samples == 0
                    && self.covered_duration_us == 0
            }
            Applicability::Applicable => match self.state {
                CoverageState::Complete => {
                    self.present_samples > 0
                        && self.covered_duration_us == self.interval.duration_us()
                }
                CoverageState::Partial => {
                    self.present_samples > 0
                        && self.covered_duration_us > 0
                        && self.covered_duration_us < self.interval.duration_us()
                }
                CoverageState::Gap => self.covered_duration_us < self.interval.duration_us(),
                CoverageState::Unknown | CoverageState::NotCollected => {
                    self.covered_duration_us == 0
                }
            },
        };
        self.expected_period_us.is_none_or(|period| period > 0)
            && self.covered_duration_us <= self.interval.duration_us()
            && population_is_consistent
            && loss_is_consistent
            && state_is_consistent
    }

    fn is_strictly_eligible(&self, interval: CoverageSpan) -> bool {
        let population_is_full = self.source_population.is_none_or(|population| {
            population.total == Some(population.collected)
                && population.total_quality == PopulationTotalQuality::Exact
        });
        self.interval == interval
            && self.applicability == Applicability::Applicable
            && self.state == CoverageState::Complete
            && self.covered_duration_us == interval.duration_us()
            && self.expected_period_us.is_some_and(|period| period > 0)
            && matches!(
                self.period_quality,
                PeriodQuality::PersistedConfigEpoch | PeriodQuality::ObservedStable
            )
            && self.cadence_epoch_id.is_some()
            && !self.crosses_cadence_boundary
            && self.loss_reasons.is_empty()
            && self.lost_count_lower_bound.is_none_or(|count| count == 0)
            && self.retained_exactness == RetainedExactness::Exact
            && self.source_completeness == SourceCompleteness::Full
            && matches!(
                self.physical_count_semantics,
                PhysicalCountSemantics::Exact | PhysicalCountSemantics::NotApplicable
            )
            && self.boundary_quality != BoundaryQuality::Unknown
            && population_is_full
    }

    fn is_valid_not_applicable(&self, interval: CoverageSpan) -> bool {
        self.interval == interval
            && self.applicability == Applicability::NotApplicable
            && self.state == CoverageState::NotCollected
            && self.expected_period_us.is_none()
            && self.period_quality == PeriodQuality::Unknown
            && self.cadence_epoch_id.is_none()
            && !self.crosses_cadence_boundary
            && self.present_samples == 0
            && self.covered_duration_us == 0
            && self.source_population.is_none()
            && self.loss_reasons.is_empty()
            && self.lost_count_lower_bound.is_none()
            && self.retained_exactness == RetainedExactness::Unknown
            && self.source_completeness == SourceCompleteness::Unknown
            && self.physical_count_semantics == PhysicalCountSemantics::NotApplicable
            && self.boundary_quality == BoundaryQuality::Unknown
    }
}

/// Identity of the exact policy and factor topology used by a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactorSetId(pub [u8; 16]);

/// Digest of required and optional factor assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileTopologyId(pub [u8; 16]);

impl FactorSetId {
    /// Derives an ID from policy axes and canonical factor sets.
    #[must_use]
    pub fn derive(
        health_policy_version: u32,
        reduction_semantics_version: u32,
        profile_id: u32,
        profile_topology_id: ProfileTopologyId,
        applicable: &[(FactorId, DomainId)],
        participating: &[(FactorId, DomainId)],
    ) -> Self {
        let applicable = encode_factor_pairs(applicable);
        let participating = encode_factor_pairs(participating);
        let digest = sha256::digest_parts(&[
            FACTOR_SET_DOMAIN_TAG,
            &health_policy_version.to_le_bytes(),
            &reduction_semantics_version.to_le_bytes(),
            &profile_id.to_le_bytes(),
            &profile_topology_id.0,
            &REGISTRY_CONTRACT_VERSION.to_le_bytes(),
            &applicable,
            &participating,
        ]);
        let mut id = [0_u8; 16];
        id.copy_from_slice(&digest[..16]);
        Self(id)
    }
}

fn encode_factor_pairs(pairs: &[(FactorId, DomainId)]) -> Vec<u8> {
    let mut pairs = pairs.to_vec();
    pairs.sort_unstable();
    pairs.dedup();
    let count = u64::try_from(pairs.len()).unwrap_or(u64::MAX);
    let mut encoded = Vec::with_capacity(8 + pairs.len() * 8);
    encoded.extend_from_slice(&count.to_le_bytes());
    for (factor, domain) in pairs {
        encoded.extend_from_slice(&factor.0.to_le_bytes());
        encoded.extend_from_slice(&(domain as u32).to_le_bytes());
    }
    encoded
}

/// Invalid required-factor profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidFactorProfile {
    /// Profile exceeds its configured factor-work bound.
    LimitExceeded,
    /// No required domain was declared.
    NoRequiredDomains,
    /// A required domain has no factors.
    EmptyRequiredDomain,
    /// A domain or factor occurs more than once.
    DuplicateEntry,
    /// A factor is assigned to more than one domain or role.
    ConflictingFactorAssignment,
}

/// Validated factor topology for one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredFactorProfile {
    profile_id: u32,
    topology_id: ProfileTopologyId,
    required_factors_by_domain: Vec<(DomainId, Vec<FactorId>)>,
    optional_factors: Vec<(FactorId, DomainId)>,
}

impl RequiredFactorProfile {
    /// Builds a profile with unique, nonempty required domains.
    ///
    /// # Errors
    /// Returns [`InvalidFactorProfile`] for an empty or ambiguous topology.
    pub fn new(
        profile_id: u32,
        mut required_factors_by_domain: Vec<(DomainId, Vec<FactorId>)>,
        mut optional_factors: Vec<(FactorId, DomainId)>,
        limits: HealthLimits,
    ) -> Result<Self, InvalidFactorProfile> {
        if required_factors_by_domain.is_empty() {
            return Err(InvalidFactorProfile::NoRequiredDomains);
        }
        if required_factors_by_domain.len() > DomainId::ALL.len()
            || optional_factors.len() > limits.max_profile_factors
        {
            return Err(InvalidFactorProfile::LimitExceeded);
        }
        let profile_factor_count = required_factors_by_domain.iter().try_fold(
            optional_factors.len(),
            |count, (_, factors)| {
                if factors.is_empty() {
                    return Err(InvalidFactorProfile::EmptyRequiredDomain);
                }
                count
                    .checked_add(factors.len())
                    .ok_or(InvalidFactorProfile::LimitExceeded)
            },
        )?;
        if profile_factor_count > limits.max_profile_factors {
            return Err(InvalidFactorProfile::LimitExceeded);
        }
        required_factors_by_domain.sort_unstable_by_key(|(domain, _)| *domain);
        optional_factors.sort_unstable();
        if required_factors_by_domain
            .windows(2)
            .any(|pair| pair[0].0 == pair[1].0)
            || optional_factors.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(InvalidFactorProfile::DuplicateEntry);
        }
        let mut assigned = BTreeMap::new();
        for (domain, factors) in &mut required_factors_by_domain {
            factors.sort_unstable();
            if factors.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(InvalidFactorProfile::DuplicateEntry);
            }
            for factor in factors {
                if assigned.insert(*factor, *domain).is_some() {
                    return Err(InvalidFactorProfile::ConflictingFactorAssignment);
                }
            }
        }
        for &(factor, domain) in &optional_factors {
            if assigned.insert(factor, domain).is_some() {
                return Err(InvalidFactorProfile::ConflictingFactorAssignment);
            }
        }
        let required_pairs: Vec<(FactorId, DomainId)> = required_factors_by_domain
            .iter()
            .flat_map(|(domain, factors)| factors.iter().copied().map(|factor| (factor, *domain)))
            .collect();
        let required_encoded = encode_factor_pairs(&required_pairs);
        let optional_encoded = encode_factor_pairs(&optional_factors);
        let topology_digest = sha256::digest_parts(&[
            PROFILE_TOPOLOGY_DOMAIN_TAG,
            &profile_id.to_le_bytes(),
            &required_encoded,
            &optional_encoded,
        ]);
        let mut topology_id = [0_u8; 16];
        topology_id.copy_from_slice(&topology_digest[..16]);
        Ok(Self {
            profile_id,
            topology_id: ProfileTopologyId(topology_id),
            required_factors_by_domain,
            optional_factors,
        })
    }

    /// Stable profile ID.
    #[must_use]
    pub const fn profile_id(&self) -> u32 {
        self.profile_id
    }

    /// Digest of required and optional assignments.
    #[must_use]
    pub const fn topology_id(&self) -> ProfileTopologyId {
        self.topology_id
    }

    fn configured_domain(&self, factor: FactorId) -> Option<DomainId> {
        self.required_factors_by_domain
            .iter()
            .find_map(|(domain, factors)| factors.contains(&factor).then_some(*domain))
            .or_else(|| {
                self.optional_factors
                    .iter()
                    .find_map(|&(candidate, domain)| (candidate == factor).then_some(domain))
            })
    }

    fn factor_count(&self) -> usize {
        self.required_factors_by_domain
            .iter()
            .map(|(_, factors)| factors.len())
            .sum::<usize>()
            + self.optional_factors.len()
    }
}

/// Invalid health policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidHealthPolicy {
    /// A policy version is zero.
    ZeroVersion,
    /// Thresholds are non-finite, out of range, or reversed.
    InvalidThresholds,
    /// Operation limits cannot hold one complete profile cell.
    IncompatibleLimits,
}

/// Versioned policy and score thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthPolicy {
    version: u32,
    reduction_semantics_version: u32,
    profile: RequiredFactorProfile,
    degraded_below: f64,
    critical_below: f64,
    limits: HealthLimits,
}

impl HealthPolicy {
    /// Builds a policy with ordered finite thresholds.
    ///
    /// # Errors
    /// Returns [`InvalidHealthPolicy`] for invalid versions, thresholds, or
    /// operation limits that cannot hold one complete profile cell.
    pub fn new(
        version: u32,
        reduction_semantics_version: u32,
        profile: RequiredFactorProfile,
        degraded_below: f64,
        critical_below: f64,
        limits: HealthLimits,
    ) -> Result<Self, InvalidHealthPolicy> {
        if version == 0 || reduction_semantics_version == 0 {
            return Err(InvalidHealthPolicy::ZeroVersion);
        }
        if !degraded_below.is_finite()
            || !critical_below.is_finite()
            || !(0.0..=1.0).contains(&critical_below)
            || !(0.0..=1.0).contains(&degraded_below)
            || critical_below > degraded_below
        {
            return Err(InvalidHealthPolicy::InvalidThresholds);
        }
        let profile_factor_count = profile.factor_count();
        if profile_factor_count > limits.max_profile_factors
            || profile_factor_count > limits.max_cell_factors
            || profile_factor_count > limits.max_coverage_entries
        {
            return Err(InvalidHealthPolicy::IncompatibleLimits);
        }
        Ok(Self {
            version,
            reduction_semantics_version,
            profile,
            degraded_below,
            critical_below,
            limits,
        })
    }

    /// Policy version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Maps a known score to a state.
    #[must_use]
    fn state_from_score(&self, score: f64) -> HealthState {
        if score < self.critical_below {
            HealthState::Critical
        } else if score < self.degraded_below {
            HealthState::Degraded
        } else {
            HealthState::Normal
        }
    }

    /// Evaluates one co-temporal cell.
    ///
    /// Missing or ineligible required evidence yields an `Unknown` numeric
    /// score. Contradictory records return a typed error.
    ///
    /// # Errors
    /// Returns [`HealthEvaluationError`] for duplicate, foreign, or
    /// contradictory evidence.
    #[allow(
        clippy::too_many_lines,
        reason = "validation and construction form one atomic decision table"
    )]
    pub fn evaluate_cell(
        &self,
        interval: CoverageSpan,
        factors: &[FactorPenalty],
        mut coverage: Vec<FactorCoverage>,
        floor_evidence: Vec<FloorEvidence>,
    ) -> Result<HealthPoint, HealthEvaluationError> {
        if factors.len() > self.limits.max_cell_factors {
            return Err(HealthEvaluationError::LimitExceeded(
                HealthResource::CellFactors,
            ));
        }
        if coverage.len() > self.limits.max_coverage_entries {
            return Err(HealthEvaluationError::LimitExceeded(
                HealthResource::CoverageEntries,
            ));
        }
        if floor_evidence.len() > self.limits.max_floor_evidence {
            return Err(HealthEvaluationError::LimitExceeded(
                HealthResource::FloorEvidence,
            ));
        }
        coverage.sort_unstable_by_key(|entry| entry.factor_id);
        if coverage
            .windows(2)
            .any(|pair| pair[0].factor_id == pair[1].factor_id)
        {
            return Err(HealthEvaluationError::DuplicateCoverage);
        }
        for entry in &mut coverage {
            if entry.loss_reasons.len() > LossReason::ALL.len() {
                return Err(HealthEvaluationError::LimitExceeded(
                    HealthResource::LossReasons,
                ));
            }
            entry.loss_reasons.sort_unstable();
            entry.loss_reasons.dedup();
            if entry.interval != interval {
                return Err(HealthEvaluationError::CoverageIntervalMismatch);
            }
            if !entry.is_consistent() {
                return Err(HealthEvaluationError::InvalidCoverage);
            }
            if self.profile.configured_domain(entry.factor_id).is_none() {
                return Err(HealthEvaluationError::UnconfiguredFactor);
            }
        }

        let mut factor_penalties = factors.to_vec();
        factor_penalties.sort_unstable_by(order_factor_penalties);
        let mut seen_factors = BTreeSet::new();
        for penalty in &factor_penalties {
            if !seen_factors.insert(penalty.factor_id) {
                return Err(HealthEvaluationError::DuplicatePenalty);
            }
            if self.profile.configured_domain(penalty.factor_id) != Some(penalty.domain) {
                return Err(HealthEvaluationError::FactorDomainMismatch);
            }
            let Some(entry) = coverage
                .iter()
                .find(|entry| entry.factor_id == penalty.factor_id)
            else {
                return Err(HealthEvaluationError::PenaltyWithoutCoverage);
            };
            if !entry.is_strictly_eligible(interval) {
                return Err(HealthEvaluationError::PenaltyWithIneligibleCoverage);
            }
        }

        let floor_evidence = normalize_floors(floor_evidence)?;
        let has_floor = !floor_evidence.is_empty();
        let required_known =
            self.profile
                .required_factors_by_domain
                .iter()
                .all(|(_, required_factors)| {
                    required_factors.iter().all(|factor| {
                        let Some(entry) = coverage.iter().find(|entry| entry.factor_id == *factor)
                        else {
                            return false;
                        };
                        if entry.is_valid_not_applicable(interval) {
                            return true;
                        }
                        entry.is_strictly_eligible(interval)
                            && factor_penalties
                                .iter()
                                .any(|penalty| penalty.factor_id == *factor)
                    })
                });

        let domain_penalties = domain_penalties_of(&factor_penalties);
        let continuous_score = required_known.then(|| {
            domain_penalties
                .iter()
                .map(|domain| 1.0 - domain.penalty)
                .product::<f64>()
        });
        let overall_score = continuous_score.map(|score| if has_floor { 0.0 } else { score });
        let overall_state = if has_floor {
            HealthState::Critical
        } else if let Some(score) = overall_score {
            self.state_from_score(score)
        } else {
            HealthState::Unknown
        };

        let applicable: Vec<(FactorId, DomainId)> = coverage
            .iter()
            .filter(|entry| entry.applicability == Applicability::Applicable)
            .filter_map(|entry| {
                self.profile
                    .configured_domain(entry.factor_id)
                    .map(|domain| (entry.factor_id, domain))
            })
            .collect();
        let participating: Vec<(FactorId, DomainId)> = factor_penalties
            .iter()
            .map(|penalty| (penalty.factor_id, penalty.domain))
            .collect();
        let factor_set_id = FactorSetId::derive(
            self.version,
            self.reduction_semantics_version,
            self.profile.profile_id,
            self.profile.topology_id,
            &applicable,
            &participating,
        );

        Ok(HealthPoint {
            interval,
            continuous_score,
            overall_score,
            overall_state,
            health_policy_version: self.version,
            reduction_semantics_version: self.reduction_semantics_version,
            profile_topology_id: self.profile.topology_id,
            factor_set_id,
            factor_penalties,
            domain_penalties,
            floor_evidence,
            coverage,
        })
    }
}

/// Contradiction in cell evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthEvaluationError {
    /// A configured work/output bound was exceeded.
    LimitExceeded(HealthResource),
    /// More than one coverage record exists for a factor.
    DuplicateCoverage,
    /// More than one penalty exists for a factor.
    DuplicatePenalty,
    /// Coverage describes a different interval.
    CoverageIntervalMismatch,
    /// Coverage fields contradict applicability or state.
    InvalidCoverage,
    /// A coverage record or penalty references an unconfigured factor.
    UnconfiguredFactor,
    /// Penalty domain differs from the profile.
    FactorDomainMismatch,
    /// A penalty has no coverage record.
    PenaltyWithoutCoverage,
    /// A penalty claims evidence that strict coverage rejects.
    PenaltyWithIneligibleCoverage,
    /// One fact ID was assigned conflicting floor classes.
    ConflictingFloorEvidence,
}

/// Evaluated health point.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthPoint {
    interval: CoverageSpan,
    continuous_score: Option<f64>,
    overall_score: Option<f64>,
    overall_state: HealthState,
    health_policy_version: u32,
    reduction_semantics_version: u32,
    profile_topology_id: ProfileTopologyId,
    factor_set_id: FactorSetId,
    factor_penalties: Vec<FactorPenalty>,
    domain_penalties: Vec<DomainPenalty>,
    floor_evidence: Vec<FloorEvidence>,
    coverage: Vec<FactorCoverage>,
}

impl HealthPoint {
    /// Evaluated interval.
    #[must_use]
    pub const fn interval(&self) -> CoverageSpan {
        self.interval
    }

    /// Continuous score, absent when required evidence is unknown.
    #[must_use]
    pub const fn continuous_score(&self) -> Option<f64> {
        self.continuous_score
    }

    /// Overall score after applying floors.
    #[must_use]
    pub const fn overall_score(&self) -> Option<f64> {
        self.overall_score
    }

    /// Overall state.
    #[must_use]
    pub const fn overall_state(&self) -> HealthState {
        self.overall_state
    }

    /// Health-policy version.
    #[must_use]
    pub const fn health_policy_version(&self) -> u32 {
        self.health_policy_version
    }

    /// Reduction-semantics version.
    #[must_use]
    pub const fn reduction_semantics_version(&self) -> u32 {
        self.reduction_semantics_version
    }

    /// Required and optional factor topology.
    #[must_use]
    pub const fn profile_topology_id(&self) -> ProfileTopologyId {
        self.profile_topology_id
    }

    /// Factor-set identity.
    #[must_use]
    pub const fn factor_set_id(&self) -> FactorSetId {
        self.factor_set_id
    }

    /// Canonical factor penalties.
    #[must_use]
    pub fn factor_penalties(&self) -> &[FactorPenalty] {
        &self.factor_penalties
    }

    /// Domain penalties.
    #[must_use]
    pub fn domain_penalties(&self) -> &[DomainPenalty] {
        &self.domain_penalties
    }

    /// Canonical floor evidence.
    #[must_use]
    pub fn floor_evidence(&self) -> &[FloorEvidence] {
        &self.floor_evidence
    }

    /// Factor coverage records.
    #[must_use]
    pub fn coverage(&self) -> &[FactorCoverage] {
        &self.coverage
    }
}

/// Downsample failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownsampleError {
    /// A configured work/output bound was exceeded.
    LimitExceeded(HealthResource),
    /// Points use different health, reduction, or profile topology.
    MixedPolicyAxes,
    /// A point lies outside the requested bucket.
    PointOutsideBucket,
    /// One fact ID was assigned conflicting floor classes.
    ConflictingFloorEvidence,
}

/// Bucket result that preserves the exact representative cell.
#[derive(Debug, Clone, PartialEq)]
pub struct DownsampledHealthPoint {
    bucket: CoverageSpan,
    representative: HealthPoint,
    floor_evidence: Vec<FloorEvidence>,
}

impl DownsampledHealthPoint {
    /// Requested downsample bucket.
    #[must_use]
    pub const fn bucket(&self) -> CoverageSpan {
        self.bucket
    }

    /// Selected fine cell and its original coverage interval.
    #[must_use]
    pub const fn representative(&self) -> &HealthPoint {
        &self.representative
    }

    /// Canonical floor evidence from the complete bucket.
    #[must_use]
    pub fn floor_evidence(&self) -> &[FloorEvidence] {
        &self.floor_evidence
    }
}

/// Selects one representative cell for a bucket.
///
/// Floor cells win first, ordered by interval start and supporting fact ID.
/// Without a floor, the minimum numeric score wins, then interval start and
/// factor-set ID. The selected cell keeps its original interval and coverage.
///
/// # Errors
/// Returns [`DownsampleError`] for mixed policy axes, out-of-bucket points, or
/// conflicting floor evidence.
pub fn downsample_worst(
    points: &[HealthPoint],
    bucket: CoverageSpan,
    limits: HealthLimits,
) -> Result<Option<DownsampledHealthPoint>, DownsampleError> {
    if points.len() > limits.max_downsample_points {
        return Err(DownsampleError::LimitExceeded(
            HealthResource::DownsamplePoints,
        ));
    }
    let Some(first) = points.first() else {
        return Ok(None);
    };
    if points.iter().any(|point| {
        point.health_policy_version != first.health_policy_version
            || point.reduction_semantics_version != first.reduction_semantics_version
            || point.profile_topology_id != first.profile_topology_id
    }) {
        return Err(DownsampleError::MixedPolicyAxes);
    }
    if points.iter().any(|point| {
        point.interval.start_us() < bucket.start_us() || point.interval.end_us() > bucket.end_us()
    }) {
        return Err(DownsampleError::PointOutsideBucket);
    }
    let floor_count = points
        .iter()
        .try_fold(0_usize, |count, point| {
            count.checked_add(point.floor_evidence.len())
        })
        .ok_or(DownsampleError::LimitExceeded(
            HealthResource::FloorEvidence,
        ))?;
    if floor_count > limits.max_floor_evidence {
        return Err(DownsampleError::LimitExceeded(
            HealthResource::FloorEvidence,
        ));
    }
    let all_floors = normalize_floors(
        points
            .iter()
            .flat_map(|point| point.floor_evidence.iter().copied())
            .collect(),
    )
    .map_err(|_conflict| DownsampleError::ConflictingFloorEvidence)?;

    let representative = points
        .iter()
        .filter(|point| !point.floor_evidence.is_empty())
        .min_by(|left, right| floor_point_cmp(left, right))
        .or_else(|| {
            points
                .iter()
                .filter(|point| point.overall_score.is_some())
                .min_by(|left, right| {
                    left.overall_score
                        .unwrap_or(1.0)
                        .total_cmp(&right.overall_score.unwrap_or(1.0))
                        .then_with(|| point_tie_cmp(left, right))
                })
        })
        .unwrap_or_else(|| {
            points
                .iter()
                .min_by(|left, right| point_tie_cmp(left, right))
                .unwrap_or(first)
        });
    Ok(Some(DownsampledHealthPoint {
        bucket,
        representative: representative.clone(),
        floor_evidence: all_floors,
    }))
}

fn point_tie_cmp(left: &HealthPoint, right: &HealthPoint) -> Ordering {
    (left.interval.start_us(), left.factor_set_id)
        .cmp(&(right.interval.start_us(), right.factor_set_id))
}

fn floor_point_cmp(left: &HealthPoint, right: &HealthPoint) -> Ordering {
    let left_fact = left
        .floor_evidence
        .iter()
        .map(|floor| floor.supporting_fact_id)
        .min();
    let right_fact = right
        .floor_evidence
        .iter()
        .map(|floor| floor.supporting_fact_id)
        .min();
    (left.interval.start_us(), left_fact, left.factor_set_id).cmp(&(
        right.interval.start_us(),
        right_fact,
        right.factor_set_id,
    ))
}

fn order_factor_penalties(left: &FactorPenalty, right: &FactorPenalty) -> Ordering {
    (left.domain, left.factor_id, left.supporting_fact_id)
        .cmp(&(right.domain, right.factor_id, right.supporting_fact_id))
        .then_with(|| left.penalty.total_cmp(&right.penalty))
}

fn normalize_floors(
    mut floors: Vec<FloorEvidence>,
) -> Result<Vec<FloorEvidence>, HealthEvaluationError> {
    floors.sort_unstable_by_key(|floor| (floor.supporting_fact_id, floor.class));
    for pair in floors.windows(2) {
        if pair[0].supporting_fact_id == pair[1].supporting_fact_id
            && pair[0].class != pair[1].class
        {
            return Err(HealthEvaluationError::ConflictingFloorEvidence);
        }
    }
    floors.dedup();
    Ok(floors)
}

fn domain_penalties_of(factors: &[FactorPenalty]) -> Vec<DomainPenalty> {
    let mut out = Vec::new();
    for domain in DomainId::ALL {
        let in_domain = factors.iter().filter(|factor| factor.domain == domain);
        let Some(maximum) = in_domain
            .clone()
            .map(|factor| factor.penalty)
            .max_by(f64::total_cmp)
        else {
            continue;
        };
        let mut driving_factor_ids: Vec<FactorId> = in_domain
            .filter(|factor| factor.penalty.total_cmp(&maximum) == Ordering::Equal)
            .map(|factor| factor.factor_id)
            .collect();
        driving_factor_ids.sort_unstable();
        driving_factor_ids.dedup();
        out.push(DomainPenalty {
            domain,
            penalty: maximum,
            driving_factor_ids,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "fixtures use exactly representable values"
    )]

    use super::*;

    const LIMITS: HealthLimits = HealthLimits {
        max_profile_factors: 32,
        max_cell_factors: 32,
        max_coverage_entries: 32,
        max_floor_evidence: 32,
        max_downsample_points: 32,
    };

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid fixture span")
    }

    fn factor_of(domain: DomainId) -> FactorId {
        FactorId(10 * (domain as u32 + 1))
    }

    fn profile(required: &[DomainId]) -> RequiredFactorProfile {
        RequiredFactorProfile::new(
            1,
            required
                .iter()
                .map(|&domain| (domain, vec![factor_of(domain)]))
                .collect(),
            vec![(FactorId(999), DomainId::CpuPressure)],
            LIMITS,
        )
        .expect("valid profile")
    }

    fn policy(required: &[DomainId]) -> HealthPolicy {
        HealthPolicy::new(1, 1, profile(required), 0.9, 0.5, LIMITS).expect("valid policy")
    }

    fn complete_coverage(domain: DomainId, interval: CoverageSpan) -> FactorCoverage {
        FactorCoverage {
            factor_id: factor_of(domain),
            applicability: Applicability::Applicable,
            state: CoverageState::Complete,
            interval,
            expected_period_us: Some(1_000_000),
            period_quality: PeriodQuality::PersistedConfigEpoch,
            cadence_epoch_id: Some(CadenceEpochId([7; 16])),
            crosses_cadence_boundary: false,
            present_samples: 10,
            covered_duration_us: interval.duration_us(),
            source_population: None,
            loss_reasons: Vec::new(),
            lost_count_lower_bound: None,
            retained_exactness: RetainedExactness::Exact,
            source_completeness: SourceCompleteness::Full,
            physical_count_semantics: PhysicalCountSemantics::NotApplicable,
            boundary_quality: BoundaryQuality::Contained,
        }
    }

    fn penalty(domain: DomainId, value: f64, fact: u8) -> FactorPenalty {
        FactorPenalty::new(factor_of(domain), domain, value, FactId([fact; 32]))
            .expect("valid penalty")
    }

    fn floor(class: FloorClass, fact: u8) -> FloorEvidence {
        FloorEvidence {
            class,
            supporting_fact_id: FactId([fact; 32]),
        }
    }

    #[test]
    fn profile_and_thresholds_reject_vacuous_or_invalid_policy() {
        assert_eq!(
            RequiredFactorProfile::new(1, Vec::new(), Vec::new(), LIMITS),
            Err(InvalidFactorProfile::NoRequiredDomains)
        );
        assert_eq!(
            HealthPolicy::new(
                1,
                1,
                profile(&[DomainId::Contention]),
                f64::NAN,
                0.5,
                LIMITS,
            ),
            Err(InvalidHealthPolicy::InvalidThresholds)
        );
        assert_eq!(
            HealthPolicy::new(1, 1, profile(&[DomainId::Contention]), 0.4, 0.5, LIMITS,),
            Err(InvalidHealthPolicy::InvalidThresholds)
        );

        let too_tight = HealthLimits {
            max_profile_factors: 1,
            max_cell_factors: 1,
            max_coverage_entries: 1,
            ..LIMITS
        };
        assert_eq!(
            HealthPolicy::new(
                1,
                1,
                profile(&[DomainId::Contention, DomainId::MemoryPressure]),
                0.9,
                0.5,
                too_tight,
            ),
            Err(InvalidHealthPolicy::IncompatibleLimits)
        );
    }

    #[test]
    fn missing_required_penalty_is_unknown_even_with_complete_coverage() {
        let interval = span(0, 1_000_000);
        let point = policy(&[DomainId::Contention])
            .evaluate_cell(
                interval,
                &[],
                vec![complete_coverage(DomainId::Contention, interval)],
                Vec::new(),
            )
            .expect("consistent input");
        assert_eq!(point.overall_score(), None);
        assert_eq!(point.overall_state(), HealthState::Unknown);
    }

    #[test]
    fn every_required_factor_needs_coverage_and_a_penalty() {
        let interval = span(0, 1_000_000);
        let first = factor_of(DomainId::Contention);
        let second = FactorId(first.0 + 1);
        let required = RequiredFactorProfile::new(
            2,
            vec![(DomainId::Contention, vec![first, second])],
            Vec::new(),
            LIMITS,
        )
        .expect("valid profile");
        let policy = HealthPolicy::new(1, 1, required, 0.9, 0.5, LIMITS).expect("valid policy");
        let mut second_coverage = complete_coverage(DomainId::Contention, interval);
        second_coverage.factor_id = second;
        let point = policy
            .evaluate_cell(
                interval,
                &[penalty(DomainId::Contention, 0.0, 1)],
                vec![
                    complete_coverage(DomainId::Contention, interval),
                    second_coverage,
                ],
                Vec::new(),
            )
            .expect("consistent input");
        assert_eq!(point.overall_score(), None);
        assert_eq!(point.overall_state(), HealthState::Unknown);
    }

    #[test]
    fn partial_lossy_assumed_or_foreign_coverage_never_turns_green() {
        let interval = span(0, 1_000_000);
        let p = policy(&[DomainId::Contention]);
        let required_penalty = penalty(DomainId::Contention, 0.0, 1);
        let mut cases = Vec::new();
        let mut partial = complete_coverage(DomainId::Contention, interval);
        partial.state = CoverageState::Partial;
        partial.covered_duration_us -= 1;
        cases.push(partial);
        let mut lossy = complete_coverage(DomainId::Contention, interval);
        lossy.loss_reasons.push(LossReason::ParserBound);
        cases.push(lossy);
        let mut assumed = complete_coverage(DomainId::Contention, interval);
        assumed.period_quality = PeriodQuality::AssumedCurrentConfig;
        cases.push(assumed);
        let mut boundary = complete_coverage(DomainId::Contention, interval);
        boundary.crosses_cadence_boundary = true;
        cases.push(boundary);

        for coverage in cases {
            assert_eq!(
                p.evaluate_cell(interval, &[required_penalty], vec![coverage], Vec::new()),
                Err(HealthEvaluationError::PenaltyWithIneligibleCoverage)
            );
        }

        let foreign = complete_coverage(DomainId::Contention, span(0, 2_000_000));
        assert_eq!(
            p.evaluate_cell(interval, &[], vec![foreign], Vec::new()),
            Err(HealthEvaluationError::CoverageIntervalMismatch)
        );
    }

    #[test]
    fn every_strict_coverage_axis_is_enforced() {
        let interval = span(0, 1_000_000);
        let policy = policy(&[DomainId::Contention]);
        let required_penalty = penalty(DomainId::Contention, 0.0, 1);
        let mut cases = Vec::new();

        let mut no_epoch = complete_coverage(DomainId::Contention, interval);
        no_epoch.cadence_epoch_id = None;
        cases.push(no_epoch);
        let mut inexact = complete_coverage(DomainId::Contention, interval);
        inexact.retained_exactness = RetainedExactness::LowerBound;
        cases.push(inexact);
        let mut incomplete = complete_coverage(DomainId::Contention, interval);
        incomplete.source_completeness = SourceCompleteness::BoundedSubset;
        cases.push(incomplete);
        let mut lower_bound_count = complete_coverage(DomainId::Contention, interval);
        lower_bound_count.physical_count_semantics = PhysicalCountSemantics::LowerBound;
        cases.push(lower_bound_count);
        let mut unknown_boundary = complete_coverage(DomainId::Contention, interval);
        unknown_boundary.boundary_quality = BoundaryQuality::Unknown;
        cases.push(unknown_boundary);
        let mut incomplete_population = complete_coverage(DomainId::Contention, interval);
        incomplete_population.source_population = Some(SourcePopulation {
            collected: 1,
            total: Some(2),
            total_quality: PopulationTotalQuality::Exact,
        });
        cases.push(incomplete_population);

        for coverage in cases {
            assert_eq!(
                policy.evaluate_cell(interval, &[required_penalty], vec![coverage], Vec::new(),),
                Err(HealthEvaluationError::PenaltyWithIneligibleCoverage)
            );
        }
    }

    #[test]
    fn oversized_loss_reason_vector_is_rejected_before_sorting() {
        let interval = span(0, 1_000_000);
        let mut coverage = complete_coverage(DomainId::Contention, interval);
        coverage.loss_reasons = vec![LossReason::ParserBound; LossReason::ALL.len() + 1];
        assert_eq!(
            policy(&[DomainId::Contention]).evaluate_cell(
                interval,
                &[],
                vec![coverage],
                Vec::new(),
            ),
            Err(HealthEvaluationError::LimitExceeded(
                HealthResource::LossReasons,
            ))
        );
    }

    #[test]
    fn invalid_optional_coverage_is_rejected_without_a_penalty() {
        let interval = span(0, 1_000_000);
        let mut coverage = complete_coverage(DomainId::CpuPressure, interval);
        coverage.factor_id = FactorId(999);
        coverage.covered_duration_us = 0;
        assert_eq!(
            policy(&[DomainId::Contention]).evaluate_cell(
                interval,
                &[],
                vec![coverage],
                Vec::new(),
            ),
            Err(HealthEvaluationError::InvalidCoverage)
        );
    }

    #[test]
    fn explicit_zero_penalty_with_strict_coverage_can_be_normal() {
        let interval = span(0, 1_000_000);
        let point = policy(&[DomainId::Contention])
            .evaluate_cell(
                interval,
                &[penalty(DomainId::Contention, 0.0, 1)],
                vec![complete_coverage(DomainId::Contention, interval)],
                Vec::new(),
            )
            .expect("consistent input");
        assert_eq!(point.overall_score(), Some(1.0));
        assert_eq!(point.overall_state(), HealthState::Normal);
    }

    #[test]
    fn inapplicable_required_factor_does_not_invent_a_measurement() {
        let interval = span(0, 1_000_000);
        let mut coverage = complete_coverage(DomainId::Replication, interval);
        coverage.applicability = Applicability::NotApplicable;
        coverage.state = CoverageState::NotCollected;
        coverage.expected_period_us = None;
        coverage.period_quality = PeriodQuality::Unknown;
        coverage.cadence_epoch_id = None;
        coverage.present_samples = 0;
        coverage.covered_duration_us = 0;
        coverage.retained_exactness = RetainedExactness::Unknown;
        coverage.source_completeness = SourceCompleteness::Unknown;
        coverage.physical_count_semantics = PhysicalCountSemantics::NotApplicable;
        coverage.boundary_quality = BoundaryQuality::Unknown;
        let point = policy(&[DomainId::Replication])
            .evaluate_cell(interval, &[], vec![coverage], Vec::new())
            .expect("consistent input");
        assert_eq!(point.overall_score(), Some(1.0));
        assert!(point.factor_penalties().is_empty());
    }

    #[test]
    fn contradictory_not_applicable_evidence_cannot_turn_green() {
        let interval = span(0, 1_000_000);
        let mut coverage = complete_coverage(DomainId::Replication, interval);
        coverage.applicability = Applicability::NotApplicable;
        coverage.state = CoverageState::NotCollected;
        coverage.present_samples = 0;
        coverage.covered_duration_us = 0;
        coverage.loss_reasons.push(LossReason::TailerBound);
        assert_eq!(
            policy(&[DomainId::Replication]).evaluate_cell(
                interval,
                &[],
                vec![coverage],
                Vec::new(),
            ),
            Err(HealthEvaluationError::InvalidCoverage)
        );
    }

    #[test]
    fn domain_maxima_multiply_across_domains() {
        let interval = span(0, 1_000_000);
        let required = [DomainId::Contention, DomainId::MemoryPressure];
        let coverage = required
            .iter()
            .map(|&domain| complete_coverage(domain, interval))
            .collect();
        let point = policy(&required)
            .evaluate_cell(
                interval,
                &[
                    penalty(DomainId::Contention, 0.5, 1),
                    penalty(DomainId::MemoryPressure, 0.25, 2),
                ],
                coverage,
                Vec::new(),
            )
            .expect("consistent input");
        assert_eq!(point.continuous_score(), Some(0.5 * 0.75));
    }

    #[test]
    fn fixed_factor_set_scores_are_bounded_and_monotonic() {
        let interval = span(0, 1_000_000);
        let required = [DomainId::Contention, DomainId::MemoryPressure];
        let coverage: Vec<FactorCoverage> = required
            .iter()
            .map(|&domain| complete_coverage(domain, interval))
            .collect();
        let policy = policy(&required);
        let grid = [0.0, 0.25, 0.5, 0.75, 1.0];
        for low in grid {
            for high in grid.into_iter().filter(|high| *high >= low) {
                let evaluate = |contention| {
                    policy
                        .evaluate_cell(
                            interval,
                            &[
                                penalty(DomainId::Contention, contention, 1),
                                penalty(DomainId::MemoryPressure, 0.25, 2),
                            ],
                            coverage.clone(),
                            Vec::new(),
                        )
                        .expect("consistent input")
                        .continuous_score()
                        .expect("required evidence is complete")
                };
                let weaker = evaluate(low);
                let stronger = evaluate(high);
                assert!(weaker.is_finite() && (0.0..=1.0).contains(&weaker));
                assert!(stronger.is_finite() && (0.0..=1.0).contains(&stronger));
                assert!(stronger <= weaker);
            }
        }
    }

    #[test]
    fn factor_coverage_and_floor_permutations_do_not_change_a_point() {
        let interval = span(0, 1_000_000);
        let required = [DomainId::Contention, DomainId::MemoryPressure];
        let factors = [
            penalty(DomainId::Contention, 0.5, 1),
            penalty(DomainId::MemoryPressure, 0.25, 2),
        ];
        let coverage = [
            complete_coverage(DomainId::Contention, interval),
            complete_coverage(DomainId::MemoryPressure, interval),
        ];
        let floors = [
            floor(FloorClass::Availability, 8),
            floor(FloorClass::Integrity, 9),
        ];
        let point = policy(&required)
            .evaluate_cell(interval, &factors, coverage.to_vec(), floors.to_vec())
            .expect("consistent input");
        let reversed = policy(&required)
            .evaluate_cell(
                interval,
                &[factors[1], factors[0]],
                vec![coverage[1].clone(), coverage[0].clone()],
                vec![floors[1], floors[0]],
            )
            .expect("consistent input");
        assert_eq!(point, reversed);
    }

    #[test]
    fn floor_with_unknown_coverage_is_critical_without_numeric_zero() {
        let interval = span(0, 1_000_000);
        let point = policy(&[DomainId::MemoryPressure])
            .evaluate_cell(
                interval,
                &[],
                Vec::new(),
                vec![floor(FloorClass::OomKill, 3)],
            )
            .expect("consistent input");
        assert_eq!(point.continuous_score(), None);
        assert_eq!(point.overall_score(), None);
        assert_eq!(point.overall_state(), HealthState::Critical);
    }

    #[test]
    fn conflicting_floor_classes_for_one_fact_are_rejected() {
        let interval = span(0, 1_000_000);
        assert_eq!(
            policy(&[DomainId::Contention]).evaluate_cell(
                interval,
                &[],
                Vec::new(),
                vec![
                    floor(FloorClass::Availability, 1),
                    floor(FloorClass::Integrity, 1),
                ],
            ),
            Err(HealthEvaluationError::ConflictingFloorEvidence)
        );
    }

    #[test]
    fn downsample_selects_earliest_floor_cell_before_numeric_minimum() {
        let p = policy(&[DomainId::Contention]);
        let bucket = span(0, 3_000_000);
        let unknown_floor = p
            .evaluate_cell(
                span(0, 1_000_000),
                &[],
                Vec::new(),
                vec![floor(FloorClass::Availability, 9)],
            )
            .expect("consistent input");
        let severe = p
            .evaluate_cell(
                span(1_000_000, 2_000_000),
                &[penalty(DomainId::Contention, 0.9, 2)],
                vec![complete_coverage(
                    DomainId::Contention,
                    span(1_000_000, 2_000_000),
                )],
                Vec::new(),
            )
            .expect("consistent input");
        let result = downsample_worst(&[severe, unknown_floor], bucket, LIMITS)
            .expect("compatible points")
            .expect("nonempty");
        assert_eq!(result.bucket(), bucket);
        assert_eq!(
            result.representative().overall_state(),
            HealthState::Critical
        );
        assert_eq!(result.representative().overall_score(), None);
        assert!(result.representative().factor_penalties().is_empty());
        assert_eq!(result.representative().interval(), span(0, 1_000_000));
    }

    #[test]
    fn downsample_order_is_stable_and_bucket_floors_keep_their_time_scope() {
        let policy = policy(&[DomainId::Contention]);
        let bucket = span(0, 2_000_000);
        let early = policy
            .evaluate_cell(
                span(0, 1_000_000),
                &[],
                Vec::new(),
                vec![floor(FloorClass::Availability, 9)],
            )
            .expect("consistent input");
        let late = policy
            .evaluate_cell(
                span(1_000_000, 2_000_000),
                &[],
                Vec::new(),
                vec![floor(FloorClass::Integrity, 1)],
            )
            .expect("consistent input");
        let forward = downsample_worst(&[early.clone(), late.clone()], bucket, LIMITS)
            .expect("compatible points")
            .expect("nonempty");
        let reverse = downsample_worst(&[late, early], bucket, LIMITS)
            .expect("compatible points")
            .expect("nonempty");
        assert_eq!(forward, reverse);
        assert_eq!(forward.representative().interval(), span(0, 1_000_000));
        assert_eq!(
            forward.representative().floor_evidence(),
            &[floor(FloorClass::Availability, 9)]
        );
        assert_eq!(
            forward.floor_evidence(),
            &[
                floor(FloorClass::Integrity, 1),
                floor(FloorClass::Availability, 9),
            ]
        );
    }

    #[test]
    fn downsample_keeps_representative_coverage_interval() {
        let p = policy(&[DomainId::Contention]);
        let cell = span(1_000_000, 2_000_000);
        let point = p
            .evaluate_cell(
                cell,
                &[penalty(DomainId::Contention, 0.2, 1)],
                vec![complete_coverage(DomainId::Contention, cell)],
                Vec::new(),
            )
            .expect("consistent input");
        let result = downsample_worst(&[point], span(0, 3_000_000), LIMITS)
            .expect("compatible point")
            .expect("nonempty");
        assert_eq!(result.representative().interval(), cell);
        assert_eq!(result.representative().coverage()[0].interval, cell);
    }

    #[test]
    fn downsample_rejects_mixed_reduction_versions() {
        let interval = span(0, 1_000_000);
        let coverage = vec![complete_coverage(DomainId::Contention, interval)];
        let point_a = policy(&[DomainId::Contention])
            .evaluate_cell(
                interval,
                &[penalty(DomainId::Contention, 0.1, 1)],
                coverage.clone(),
                Vec::new(),
            )
            .expect("consistent input");
        let profile = profile(&[DomainId::Contention]);
        let point_b = HealthPolicy::new(1, 2, profile, 0.9, 0.5, LIMITS)
            .expect("valid policy")
            .evaluate_cell(
                interval,
                &[penalty(DomainId::Contention, 0.1, 1)],
                coverage,
                Vec::new(),
            )
            .expect("consistent input");
        assert_eq!(
            downsample_worst(&[point_a, point_b], interval, LIMITS),
            Err(DownsampleError::MixedPolicyAxes)
        );
    }

    #[test]
    fn downsample_rejects_mixed_profile_topologies() {
        let interval = span(0, 1_000_000);
        let contention = policy(&[DomainId::Contention])
            .evaluate_cell(interval, &[], Vec::new(), Vec::new())
            .expect("consistent input");
        let memory = policy(&[DomainId::MemoryPressure])
            .evaluate_cell(interval, &[], Vec::new(), Vec::new())
            .expect("consistent input");
        assert_eq!(
            downsample_worst(&[contention, memory], interval, LIMITS),
            Err(DownsampleError::MixedPolicyAxes)
        );
    }

    #[test]
    fn factor_set_preimage_is_order_independent_and_axis_sensitive() {
        let a = [
            (FactorId(2), DomainId::CpuPressure),
            (FactorId(1), DomainId::Contention),
        ];
        let b = [a[1], a[0]];
        assert_eq!(
            FactorSetId::derive(
                1,
                1,
                1,
                profile(&[DomainId::Contention]).topology_id(),
                &a,
                &a
            ),
            FactorSetId::derive(
                1,
                1,
                1,
                profile(&[DomainId::Contention]).topology_id(),
                &b,
                &b
            )
        );
        assert_ne!(
            FactorSetId::derive(
                1,
                1,
                1,
                profile(&[DomainId::Contention]).topology_id(),
                &a,
                &a
            ),
            FactorSetId::derive(
                1,
                2,
                1,
                profile(&[DomainId::Contention]).topology_id(),
                &a,
                &a
            )
        );
    }
}
