//! Active diagnostic lenses over typed metric evidence.

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::sink::FindingSink;
use super::evidence::{ConfidenceCap, Evidence, FindingDraft, FindingScope, Role};
use super::lens::Lens;
use super::series::SeriesSet;
use super::typed::TypedInputs;

const PG_STAT_DATABASE: &str = "pg_stat_database";

/// `PG-CACHE-010`: shared-buffer miss pressure. Reports an elevated
/// `sum(d(blks_read)) / sum(d(blks_read) + d(blks_hit))` over the incident as an
/// amplifier. Direction needs clock provenance, so the lens never leads.
pub(crate) struct CacheMissLens;

impl CacheMissLens {
    const ID: &'static str = "PG-CACHE-010";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "blks_read",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "blks_hit",
        },
    ];
    /// A regular signal needs at least three valid pairs (data-quality policy).
    const MIN_INTERVALS: usize = 3;
    /// Only an unusually cold cache is worth an incident finding.
    const MISS_THRESHOLD: f64 = 0.2;
}

impl Lens for CacheMissLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        _context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != PG_STAT_DATABASE || member.column != "blks_read" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_DATABASE,
                &member.identity,
                "blks_read",
                "blks_hit",
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            let total = sums.sum_a + sums.sum_b;
            if total <= 0.0 || sums.sum_a / total < Self::MISS_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// The lenses whose typed inputs are wired, applied to every request.
pub(crate) fn active_catalog() -> Vec<Box<dyn Lens>> {
    vec![Box::new(CacheMissLens)]
}

/// The ids of the active lenses, in catalog order.
pub(crate) fn active_catalog_ids() -> Vec<&'static str> {
    active_catalog().iter().map(|lens| lens.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::model::{EnrichedEpisode, EpisodeRefV1, IdentityValue};
    use crate::incident::{ClockRelation, IncidentConfig, analyze};
    use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Scalar};
    use std::sync::Arc;

    fn id() -> Arc<[IdentityValue]> {
        Arc::from(vec![IdentityValue::U64(5)])
    }

    fn episode() -> EnrichedEpisode {
        EnrichedEpisode {
            episode: Episode {
                start: 0,
                end: 0,
                peak_ts: 0,
                peak: Evaluated {
                    m: 0.0,
                    dir: Direction::Up,
                    med_cur: 0.0,
                    med_ref: 0.0,
                    mad_ref: 1.0,
                    sigma_used: 1.4826,
                    n_cur: 0,
                    n_ref: 0,
                },
            },
            reference: EpisodeRefV1 {
                logical_section: PG_STAT_DATABASE,
                column: "blks_read",
                identity: id(),
                start_us: 0,
                end_us: 10,
            },
        }
    }

    // One-second interval, so the recovered delta equals `delta`.
    fn point(delta: f64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Int(0),
            rate: delta,
            dt_micros: 1_000_000,
        }
    }

    fn typed(read: &[f64], hit: &[f64]) -> TypedInputs {
        let mut typed = TypedInputs::new();
        let read_points = read.iter().zip(0_i64..).map(|(&d, ts)| (ts, point(d)));
        let hit_points = hit.iter().zip(0_i64..).map(|(&d, ts)| (ts, point(d)));
        typed.insert_counter(PG_STAT_DATABASE, "blks_read", id(), read_points.collect());
        typed.insert_counter(PG_STAT_DATABASE, "blks_hit", id(), hit_points.collect());
        typed
    }

    fn run(typed: &TypedInputs) -> usize {
        let lens = CacheMissLens;
        let lenses: [&dyn Lens; 1] = [&lens];
        let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
        let outcome = analyze(
            vec![episode()],
            &SeriesSet::for_test(0),
            typed,
            &lenses,
            &config,
        )
        .expect("valid analysis");
        outcome.incidents[0].findings.len()
    }

    #[test]
    fn a_cold_cache_over_enough_intervals_reports_an_amplifier() {
        // miss ratio 80/(80+20) = 0.8, three valid intervals.
        let findings = run(&typed(&[30.0, 30.0, 20.0], &[5.0, 5.0, 10.0]));
        assert_eq!(findings, 1);
    }

    #[test]
    fn a_warm_cache_reports_nothing() {
        // miss ratio 3/(3+297) = 0.01, below the threshold.
        let findings = run(&typed(&[1.0, 1.0, 1.0], &[99.0, 99.0, 99.0]));
        assert_eq!(findings, 0);
    }

    #[test]
    fn too_few_valid_intervals_report_nothing() {
        // A cold cache but only two intervals: below the data-quality minimum.
        let findings = run(&typed(&[50.0, 50.0], &[1.0, 1.0]));
        assert_eq!(findings, 0);
    }

    #[test]
    fn the_active_catalog_carries_the_cache_lens() {
        let catalog = active_catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id(), "PG-CACHE-010");
    }
}
