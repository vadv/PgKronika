//! Dormant diagnostic catalog metadata.

use super::evidence::ConfidenceCap;

const MAX_DORMANT_LENSES: usize = 28;
const MAX_MISSING_PER_LENS: usize = 6;
const MAX_CATALOG_TOKEN_BYTES: usize = 40;
const MAX_CATALOG_TEXT_BYTES: usize = 200;

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum MissingCapability {
    CounterDeltas,
    GaugeSamples,
    PairedIntervals,
    SourcePeriod,
    InputCoverage,
    EntityJoin,
    TrackPlanningGate,
    StorePlansBridge,
    BlockedByEdges,
    LockSnapshotCoverage,
    ActivityRows,
    PidCgroupMapping,
    LogEvents,
}

impl MissingCapability {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CounterDeltas => "typed_counter_deltas",
            Self::GaugeSamples => "typed_gauge_samples",
            Self::PairedIntervals => "paired_interval_inputs",
            Self::SourcePeriod => "source_period_provenance",
            Self::InputCoverage => "request_input_coverage",
            Self::EntityJoin => "cross_section_entity_join",
            Self::TrackPlanningGate => "track_planning_gate",
            Self::StorePlansBridge => "store_plans_bridge",
            Self::BlockedByEdges => "sampled_blocked_by_edges",
            Self::LockSnapshotCoverage => "lock_snapshot_coverage",
            Self::ActivityRows => "sampled_activity_rows",
            Self::PidCgroupMapping => "pid_cgroup_mapping",
            Self::LogEvents => "typed_log_events",
        }
    }
}

/// Diagnostic domain a lens belongs to.
#[derive(Clone, Copy)]
pub(crate) enum Domain {
    Pg,
    Os,
}

impl Domain {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pg => "pg",
            Self::Os => "os",
        }
    }
}

/// A design-catalog entry with a human name and known missing capabilities.
pub(crate) struct DormantLens {
    lens_id: &'static str,
    domain: Domain,
    title: &'static str,
    detects: &'static str,
    confidence: ConfidenceCap,
    missing: &'static [MissingCapability],
}

impl DormantLens {
    pub(crate) const fn lens_id(&self) -> &'static str {
        self.lens_id
    }

    pub(crate) const fn domain(&self) -> Domain {
        self.domain
    }

    pub(crate) const fn title(&self) -> &'static str {
        self.title
    }

    pub(crate) const fn detects(&self) -> &'static str {
        self.detects
    }

    pub(crate) const fn confidence(&self) -> ConfidenceCap {
        self.confidence
    }

    pub(crate) const fn missing(&self) -> &'static [MissingCapability] {
        self.missing
    }
}

use MissingCapability as Missing;

const DORMANT_CATALOG: &[DormantLens] = &[
    DormantLens {
        lens_id: "query_workload_shift",
        domain: Domain::Pg,
        title: "Сдвиг профиля запроса",
        detects: "У нормализованного запроса изменились частота, работа на вызов или время исполнения.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "plan_change",
        domain: Domain::Pg,
        title: "Смена плана запроса",
        detects: "Деградация запроса совпала с появлением или сменой `planid`.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::GaugeSamples,
            Missing::StorePlansBridge,
            Missing::TrackPlanningGate,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "temp_spill",
        domain: Domain::Pg,
        title: "Спил во временные файлы",
        detects: "Рост работы через временные блоки и файлы.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::LogEvents,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "stale_statistics",
        domain: Domain::Pg,
        title: "Устаревшая статистика планировщика",
        detects: "`n_mod_since_analyze` высок, свежего analyze нет, план/работа поехали.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "vacuum_backlog",
        domain: Domain::Pg,
        title: "Отставание vacuum",
        detects: "Растёт долг мёртвых кортежей, cleanup не успевает.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::LogEvents,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "xid_wraparound_risk",
        domain: Domain::Pg,
        title: "Приближение wraparound XID/MXID",
        detects: "Headroom по возрасту XID/MXID тает, близко к форсированному aggressive vacuum.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::LogEvents,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "hot_update_failure",
        domain: Domain::Pg,
        title: "Срыв HOT-обновлений",
        detects: "Доля non-HOT updates растёт вместе с работой по индексам и WAL.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "requested_checkpoints",
        domain: Domain::Pg,
        title: "Внеплановые контрольные точки",
        detects: "Растёт доля requested checkpoints и их write/sync-работа.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::LogEvents,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "wal_amplification",
        domain: Domain::Pg,
        title: "Раздувание WAL и FPI",
        detects: "Растут WAL bytes на запись, доля FPI, `wal_buffers_full`.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "shared_buffer_misses",
        domain: Domain::Pg,
        title: "Промахи shared buffers",
        detects: "Растёт доля промахов shared buffers по базе/отношению/контексту.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "backend_io_latency",
        domain: Domain::Pg,
        title: "Задержка I/O внутри PostgreSQL",
        detects: "Растёт время на операцию или блок (`pg_stat_io`, PG16+).",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "lock_wait_graph",
        domain: Domain::Pg,
        title: "Граф ожидания блокировок",
        detects: "Кто блокировал ожидающего в момент снимка (`blocked_by` из `pg_locks`).",
        confidence: ConfidenceCap::High,
        missing: &[Missing::BlockedByEdges, Missing::LockSnapshotCoverage],
    },
    DormantLens {
        lens_id: "xmin_horizon_hold",
        domain: Domain::Pg,
        title: "Удержание горизонта xmin",
        detects: "Долгая или idle-in-transaction транзакция держит vacuum-горизонт.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::ActivityRows,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "connection_saturation",
        domain: Domain::Pg,
        title: "Насыщение по соединениям",
        detects: "Backends подходят к `max_connections`, churn растёт при падении throughput.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::ActivityRows,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "replication_lag",
        domain: Domain::Pg,
        title: "Отставание физической репликации",
        detects: "На каком LSN-этапе растёт байтовый разрыв (sent/write/flush/replay).",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "slot_wal_retention",
        domain: Domain::Pg,
        title: "Удержание WAL слотом репликации",
        detects: "Слот держит растущий WAL, `retained_bytes` со склоном вверх.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "wal_archiving_failure",
        domain: Domain::Pg,
        title: "Ошибки архивации WAL",
        detects: "Подтверждённые ошибки archive command/library (`failed_count`).",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "sync_replication_wait",
        domain: Domain::Pg,
        title: "Ожидание синхронной репликации",
        detects: "Backends висят на `wait_event='SyncRep'` при настроенной синхронной репликации.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::ActivityRows,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "internal_wait_concentration",
        domain: Domain::Pg,
        title: "Концентрация внутренних ожиданий",
        detects: "Растёт доля active backends на `LWLock`/`BufferPin`/`IO` wait.",
        confidence: ConfidenceCap::Low,
        missing: &[
            Missing::ActivityRows,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-CPU-020",
        domain: Domain::Os,
        title: "Насыщение CPU хоста",
        detects: "Runnable pressure, iowait, steal.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-CGRP-021",
        domain: Domain::Os,
        title: "Троттлинг CPU в cgroup",
        detects: "Реальный throttling cgroup при доступном CPU хоста.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::PidCgroupMapping,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-MEM-022",
        domain: Domain::Os,
        title: "Нехватка памяти хоста",
        detects: "Memory pressure, direct reclaim, swap, OOM.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-CGMEM-023",
        domain: Domain::Os,
        title: "Лимит памяти cgroup",
        detects: "Достижение `memory.high`/`max`/OOM в cgroup.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::PidCgroupMapping,
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-BLOCK-024",
        domain: Domain::Os,
        title: "Задержка блочного устройства",
        detects: "Растут время завершения и очередь устройства.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-WB-025",
        domain: Domain::Os,
        title: "Давление dirty/writeback",
        detects: "Повышенные Dirty/Writeback совпали с write/sync-задержкой PostgreSQL.",
        confidence: ConfidenceCap::Low,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-IOWHO-026",
        domain: Domain::Os,
        title: "Внешний потребитель I/O",
        detects: "Какой процесс или cgroup нарастил block I/O рядом с давлением.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PidCgroupMapping,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-FS-027",
        domain: Domain::Os,
        title: "Исчерпание места ФС",
        detects: "Точка монтирования близка к исчерпанию байтов.",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::LogEvents,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-NET-028",
        domain: Domain::Os,
        title: "Сетевые ошибки и ретрансмиты",
        detects: "Растут счётчики ошибок интерфейса и TCP-ретрансмиссий.",
        confidence: ConfidenceCap::Low,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
];

/// Returns design entries with non-exhaustive readiness requirements.
pub(crate) const fn dormant_catalog() -> &'static [DormantLens] {
    DORMANT_CATALOG
}

const fn text_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut at = 0;
    while at < left.len() {
        if left[at] != right[at] {
            return false;
        }
        at += 1;
    }
    true
}

const fn catalog_is_valid(catalog: &[DormantLens]) -> bool {
    if catalog.is_empty() || catalog.len() > MAX_DORMANT_LENSES {
        return false;
    }
    let mut lens_at = 0;
    while lens_at < catalog.len() {
        let lens = &catalog[lens_at];
        if lens.lens_id.is_empty()
            || lens.lens_id.len() > MAX_CATALOG_TOKEN_BYTES
            || lens.title.is_empty()
            || lens.title.len() > MAX_CATALOG_TEXT_BYTES
            || lens.detects.is_empty()
            || lens.detects.len() > MAX_CATALOG_TEXT_BYTES
            || lens.missing.is_empty()
            || lens.missing.len() > MAX_MISSING_PER_LENS
        {
            return false;
        }
        let mut previous_lens = 0;
        while previous_lens < lens_at {
            if text_eq(catalog[previous_lens].lens_id, lens.lens_id) {
                return false;
            }
            previous_lens += 1;
        }
        let mut capability_at = 0;
        while capability_at < lens.missing.len() {
            let capability = lens.missing[capability_at];
            if capability.as_str().len() > MAX_CATALOG_TOKEN_BYTES {
                return false;
            }
            let mut previous_capability = 0;
            while previous_capability < capability_at {
                if lens.missing[previous_capability] as u8 == capability as u8 {
                    return false;
                }
                previous_capability += 1;
            }
            capability_at += 1;
        }
        lens_at += 1;
    }
    true
}

const _: () = assert!(
    catalog_is_valid(DORMANT_CATALOG),
    "dormant catalog violates static bounds"
);

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_LENSES: [&str; 28] = [
        "query_workload_shift",
        "plan_change",
        "temp_spill",
        "stale_statistics",
        "vacuum_backlog",
        "xid_wraparound_risk",
        "hot_update_failure",
        "requested_checkpoints",
        "wal_amplification",
        "shared_buffer_misses",
        "backend_io_latency",
        "lock_wait_graph",
        "xmin_horizon_hold",
        "connection_saturation",
        "replication_lag",
        "slot_wal_retention",
        "wal_archiving_failure",
        "sync_replication_wait",
        "internal_wait_concentration",
        "OS-CPU-020",
        "OS-CGRP-021",
        "OS-MEM-022",
        "OS-CGMEM-023",
        "OS-BLOCK-024",
        "OS-WB-025",
        "OS-IOWHO-026",
        "OS-FS-027",
        "OS-NET-028",
    ];

    fn fixture(lens_id: &'static str, missing: &'static [MissingCapability]) -> DormantLens {
        DormantLens {
            lens_id,
            domain: Domain::Pg,
            title: "title",
            detects: "detects",
            confidence: ConfidenceCap::Medium,
            missing,
        }
    }

    #[test]
    fn catalog_ids_match_the_contract_order() {
        let ids: Vec<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        assert_eq!(ids, EXPECTED_LENSES);
    }

    #[test]
    fn domain_strings_are_stable() {
        assert_eq!(Domain::Pg.as_str(), "pg");
        assert_eq!(Domain::Os.as_str(), "os");
    }

    #[test]
    fn duplicate_ids_are_invalid() {
        let duplicate = [
            fixture("lock_wait_graph", &[Missing::BlockedByEdges]),
            fixture("lock_wait_graph", &[Missing::LockSnapshotCoverage]),
        ];
        assert!(!catalog_is_valid(&duplicate));
    }

    #[test]
    fn duplicate_capabilities_are_invalid() {
        let duplicate = [fixture(
            "lock_wait_graph",
            &[Missing::BlockedByEdges, Missing::BlockedByEdges],
        )];
        assert!(!catalog_is_valid(&duplicate));
    }

    #[test]
    fn catalog_growth_requires_a_new_bound() {
        let oversized = std::array::from_fn::<_, { MAX_DORMANT_LENSES + 1 }, _>(|_| {
            fixture("x", &[Missing::InputCoverage])
        });
        assert!(!catalog_is_valid(&oversized));
    }

    #[test]
    fn lock_requirements_preserve_pr75() {
        let lock = dormant_catalog()
            .iter()
            .find(|lens| lens.lens_id() == "lock_wait_graph")
            .expect("lock catalog entry");
        let requirements: Vec<_> = lock
            .missing()
            .iter()
            .map(|capability| capability.as_str())
            .collect();
        assert_eq!(
            requirements,
            ["sampled_blocked_by_edges", "lock_snapshot_coverage"]
        );
    }
}
