use std::sync::Arc;

use super::super::cluster::Cluster;
use super::super::dispatch::{LimitHit, SectionColumn};
use super::super::engine::EvalContext;
use super::super::evidence::sink::FindingSink;
use super::super::evidence::{
    ConfidenceCap, Evidence, FindingDraft, FindingScope, GaugeEntity, GaugeEvidence, GaugeRatio,
    GaugeUnit, Role, ThresholdKind,
};
use super::super::lens::Lens;
use super::super::series::SeriesSet;
use super::super::typed::{GaugeObjective, TypedInputs};
use super::shared::{OS_MEMINFO, PG_STAT_DATABASE, PG_STAT_USER_TABLES};

/// `PG-ANALYZE-004`: observed modifications relative to the planner row
/// estimate. This does not assert that a plan changed.
pub(crate) struct StaleStatisticsLens;

impl StaleStatisticsLens {
    const ID: &'static str = "PG-ANALYZE-004";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "n_mod_since_analyze",
        },
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "reltuples",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const MODIFIED_FRACTION_FLOOR: f64 = 0.2;
}

impl Lens for StaleStatisticsLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != PG_STAT_USER_TABLES
                || member.column != "n_mod_since_analyze"
            {
                continue;
            }
            let Some(window) = typed.paired_gauge_window(
                PG_STAT_USER_TABLES,
                &member.identity,
                ("n_mod_since_analyze", "reltuples"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(pair) = window.reduce(GaugeObjective::RatioAbsOneMax) else {
                continue;
            };
            if pair.samples < Self::MIN_SAMPLES {
                continue;
            }
            let denominator = pair.b.abs().max(1.0);
            if pair.a < 0.0 || pair.a / denominator < Self::MODIFIED_FRACTION_FLOOR {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "n_mod_since_analyze",
                    pair.a,
                    "abs_reltuples_floor_1",
                    denominator,
                    GaugeUnit::Count,
                ),
                Self::MODIFIED_FRACTION_FLOOR,
                ThresholdKind::AtLeast,
                pair.observed_at_us,
                pair.samples,
                GaugeEntity::new(PG_STAT_USER_TABLES, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-CONN-014`: observed occupancy of one database's `datconnlimit`.
pub(crate) struct ConnectionSaturationLens;

impl ConnectionSaturationLens {
    const ID: &'static str = "PG-CONN-014";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "numbackends",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "datconnlimit",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const SATURATION_FLOOR: f64 = 0.8;
}

impl Lens for ConnectionSaturationLens {
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
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != PG_STAT_DATABASE || member.column != "numbackends" {
                continue;
            }
            let Some(window) = typed.paired_gauge_window(
                PG_STAT_DATABASE,
                &member.identity,
                ("numbackends", "datconnlimit"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(pair) = window.reduce(GaugeObjective::RatioMax) else {
                continue;
            };
            if pair.samples < Self::MIN_SAMPLES {
                continue;
            }
            if pair.a < 0.0 || pair.b <= 0.0 || pair.a / pair.b < Self::SATURATION_FLOOR {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "numbackends",
                    pair.a,
                    "datconnlimit",
                    pair.b,
                    GaugeUnit::Count,
                ),
                Self::SATURATION_FLOOR,
                ThresholdKind::AtLeast,
                pair.observed_at_us,
                pair.samples,
                GaugeEntity::new(PG_STAT_DATABASE, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `OS-MEM-022`: an observed low `MemAvailable / MemTotal` sample.
pub(crate) struct MemoryReclaimLens;

impl MemoryReclaimLens {
    const ID: &'static str = "OS-MEM-022";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_MEMINFO,
            column: "mem_available",
        },
        SectionColumn {
            section: OS_MEMINFO,
            column: "mem_total",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const AVAILABLE_FRACTION_FLOOR: f64 = 0.05;
}

impl Lens for MemoryReclaimLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != OS_MEMINFO || member.column != "mem_available" {
                continue;
            }
            let Some(window) = typed.paired_gauge_window(
                OS_MEMINFO,
                &member.identity,
                ("mem_available", "mem_total"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(pair) = window.reduce(GaugeObjective::RatioMin) else {
                continue;
            };
            if pair.samples < Self::MIN_SAMPLES {
                continue;
            }
            if pair.b <= 0.0 || pair.a / pair.b >= Self::AVAILABLE_FRACTION_FLOOR {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "mem_available",
                    pair.a,
                    "mem_total",
                    pair.b,
                    GaugeUnit::Kibibytes,
                ),
                Self::AVAILABLE_FRACTION_FLOOR,
                ThresholdKind::Below,
                pair.observed_at_us,
                pair.samples,
                GaugeEntity::new(OS_MEMINFO, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `OS-WB-025`: an observed `(Dirty + Writeback) / MemTotal` crossing.
pub(crate) struct WritebackPressureLens;

impl WritebackPressureLens {
    const ID: &'static str = "OS-WB-025";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_MEMINFO,
            column: "dirty",
        },
        SectionColumn {
            section: OS_MEMINFO,
            column: "writeback",
        },
        SectionColumn {
            section: OS_MEMINFO,
            column: "mem_total",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const DIRTY_FRACTION_FLOOR: f64 = 0.1;
}

impl Lens for WritebackPressureLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != OS_MEMINFO || member.column != "dirty" {
                continue;
            }
            let Some(window) = typed.triple_gauge_window(
                OS_MEMINFO,
                &member.identity,
                ("dirty", "writeback", "mem_total"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.sum_ratio_max() else {
                continue;
            };
            if reading.samples < Self::MIN_SAMPLES {
                continue;
            }
            let numerator = reading.a + reading.b;
            if reading.denominator <= 0.0
                || numerator / reading.denominator < Self::DIRTY_FRACTION_FLOOR
            {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "dirty_plus_writeback",
                    numerator,
                    "mem_total",
                    reading.denominator,
                    GaugeUnit::Kibibytes,
                ),
                Self::DIRTY_FRACTION_FLOOR,
                ThresholdKind::AtLeast,
                reading.observed_at_us,
                reading.samples,
                GaugeEntity::new(OS_MEMINFO, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
            ))?;
        }
        Ok(())
    }
}
