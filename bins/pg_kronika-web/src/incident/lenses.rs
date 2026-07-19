//! Dormant diagnostic catalog metadata.

use super::evidence::ConfidenceCap;

pub(crate) const MAX_DORMANT_LENSES: usize = 28;
pub(crate) const MAX_MISSING_PER_LENS: usize = 6;
pub(crate) const MAX_CATALOG_TOKEN_BYTES: usize = 40;
pub(crate) const MAX_CATALOG_TEXT_BYTES: usize = 200;

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
    IncidentLogEventInput,
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
            Self::IncidentLogEventInput => "incident_log_event_input",
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
    slug: &'static str,
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

    pub(crate) const fn slug(&self) -> &'static str {
        self.slug
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
        lens_id: "PG-QRY-001",
        slug: "query_workload_shift",
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
        lens_id: "PG-PLAN-002",
        slug: "plan_change",
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
        lens_id: "PG-TEMP-003",
        slug: "temp_spill",
        domain: Domain::Pg,
        title: "Спил во временные файлы",
        detects: "Рост работы через временные блоки и файлы.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-ANALYZE-004",
        slug: "stale_statistics",
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
        lens_id: "PG-VACUUM-005",
        slug: "vacuum_backlog",
        domain: Domain::Pg,
        title: "Отставание vacuum",
        detects: "Растёт долг мёртвых кортежей, cleanup не успевает.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-FREEZE-006",
        slug: "xid_wraparound_risk",
        domain: Domain::Pg,
        title: "Приближение wraparound XID/MXID",
        detects: "Headroom по возрасту XID/MXID тает, близко к форсированному aggressive vacuum.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::GaugeSamples,
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-HOT-007",
        slug: "hot_update_failure",
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
        lens_id: "PG-CHKPT-008",
        slug: "requested_checkpoints",
        domain: Domain::Pg,
        title: "Внеплановые контрольные точки",
        detects: "Растёт доля requested checkpoints и их write/sync-работа.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::IncidentLogEventInput,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-WAL-009",
        slug: "wal_amplification",
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
        lens_id: "PG-CACHE-010",
        slug: "shared_buffer_misses",
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
        lens_id: "PG-IO-011",
        slug: "backend_io_latency",
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
        lens_id: "PG-LOCK-012",
        slug: "lock_wait_graph",
        domain: Domain::Pg,
        title: "Граф ожидания блокировок",
        detects: "Кто блокировал ожидающего в момент снимка (`blocked_by` из `pg_locks`).",
        confidence: ConfidenceCap::High,
        missing: &[Missing::BlockedByEdges, Missing::LockSnapshotCoverage],
    },
    DormantLens {
        lens_id: "PG-HORIZON-013",
        slug: "xmin_horizon_hold",
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
        lens_id: "PG-CONN-014",
        slug: "connection_saturation",
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
        lens_id: "PG-REPL-015",
        slug: "replication_lag",
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
        lens_id: "PG-SLOT-016",
        slug: "slot_wal_retention",
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
        lens_id: "PG-ARCH-017",
        slug: "wal_archiving_failure",
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
        lens_id: "PG-SYNC-018",
        slug: "sync_replication_wait",
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
        lens_id: "PG-WAIT-019",
        slug: "internal_wait_concentration",
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
        slug: "cpu_saturation",
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
        slug: "cgroup_cpu_throttling",
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
        slug: "memory_reclaim",
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
        slug: "cgroup_memory_limit",
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
        slug: "block_device_latency",
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
        slug: "writeback_pressure",
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
        slug: "io_contender",
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
        slug: "filesystem_space",
        domain: Domain::Os,
        title: "Исчерпание места ФС",
        detects: "Точка монтирования близка к исчерпанию байтов.",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::IncidentLogEventInput,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-NET-028",
        slug: "network_errors",
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

#[cfg(test)]
#[derive(Clone, Copy)]
enum EventDisposition {
    Standalone,
    EvidenceFor(&'static str),
    RoadmapFor(&'static str),
    Split(&'static [&'static str]),
}

#[cfg(test)]
struct EventCandidate {
    proposal_id: &'static str,
    disposition: EventDisposition,
}

#[cfg(test)]
const EVENT_CANDIDATES: &[EventCandidate] = &[
    EventCandidate {
        proposal_id: "oom_kill",
        disposition: EventDisposition::Split(&["backend_sigkill", "kernel_oom_victim"]),
    },
    EventCandidate {
        proposal_id: "backend_crash",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "panic_shutdown",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "disk_full_log",
        disposition: EventDisposition::EvidenceFor("OS-FS-027"),
    },
    EventCandidate {
        proposal_id: "out_of_memory_log",
        disposition: EventDisposition::Split(&[
            "pg_allocator_oom",
            "lock_table_exhaustion",
            "dsm_allocation_failure",
        ]),
    },
    EventCandidate {
        proposal_id: "connection_slots_exhausted",
        disposition: EventDisposition::EvidenceFor("PG-CONN-014"),
    },
    EventCandidate {
        proposal_id: "deadlock",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "data_corruption_log",
        disposition: EventDisposition::Split(&[
            "checksum_page_validation_failure",
            "generic_block_io_failure",
        ]),
    },
    EventCandidate {
        proposal_id: "lock_wait_logged",
        disposition: EventDisposition::EvidenceFor("PG-LOCK-012"),
    },
    EventCandidate {
        proposal_id: "lock_timeout_log",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "statement_timeout_log",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "temp_file_spill_log",
        disposition: EventDisposition::EvidenceFor("PG-TEMP-003"),
    },
    EventCandidate {
        proposal_id: "slow_query_logged",
        disposition: EventDisposition::EvidenceFor("PG-QRY-001"),
    },
    EventCandidate {
        proposal_id: "serialization_failure",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "idle_in_transaction_abort",
        disposition: EventDisposition::EvidenceFor("PG-HORIZON-013"),
    },
    EventCandidate {
        proposal_id: "checkpoint_too_frequent",
        disposition: EventDisposition::EvidenceFor("PG-CHKPT-008"),
    },
    EventCandidate {
        proposal_id: "aggressive_autovacuum_wraparound",
        disposition: EventDisposition::RoadmapFor("PG-FREEZE-006"),
    },
    EventCandidate {
        proposal_id: "autovacuum_cancel",
        disposition: EventDisposition::EvidenceFor("PG-VACUUM-005"),
    },
    EventCandidate {
        proposal_id: "auth_password_failures",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "pg_hba_rejections",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "permission_denied_burst",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "connection_storm_log",
        disposition: EventDisposition::RoadmapFor("PG-CONN-014"),
    },
    EventCandidate {
        proposal_id: "archive_command_failure",
        disposition: EventDisposition::RoadmapFor("PG-ARCH-017"),
    },
    EventCandidate {
        proposal_id: "replication_disconnect",
        disposition: EventDisposition::Split(&[
            "walsender_timeout",
            "walreceiver_timeout_or_receive_failure",
        ]),
    },
    EventCandidate {
        proposal_id: "recovery_conflict",
        disposition: EventDisposition::Standalone,
    },
    EventCandidate {
        proposal_id: "wal_integrity_log",
        disposition: EventDisposition::Split(&["validated_wal_failure", "normal_local_end_of_wal"]),
    },
];

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

const fn slug_is_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_CATALOG_TOKEN_BYTES {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if !(bytes[at].is_ascii_lowercase() || bytes[at].is_ascii_digit() || bytes[at] == b'_') {
            return false;
        }
        at += 1;
    }
    true
}

const fn json_text_is_bounded(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_CATALOG_TEXT_BYTES {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_control() || bytes[at] == b'"' || bytes[at] == b'\\' {
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
            || !slug_is_valid(lens.slug)
            || !json_text_is_bounded(lens.title)
            || !json_text_is_bounded(lens.detects)
            || lens.missing.is_empty()
            || lens.missing.len() > MAX_MISSING_PER_LENS
        {
            return false;
        }
        let mut previous_lens = 0;
        while previous_lens < lens_at {
            if text_eq(catalog[previous_lens].lens_id, lens.lens_id)
                || text_eq(catalog[previous_lens].slug, lens.slug)
            {
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
    use std::collections::BTreeSet;

    use super::*;

    const EXPECTED_LENSES: [(&str, &str); 28] = [
        ("PG-QRY-001", "query_workload_shift"),
        ("PG-PLAN-002", "plan_change"),
        ("PG-TEMP-003", "temp_spill"),
        ("PG-ANALYZE-004", "stale_statistics"),
        ("PG-VACUUM-005", "vacuum_backlog"),
        ("PG-FREEZE-006", "xid_wraparound_risk"),
        ("PG-HOT-007", "hot_update_failure"),
        ("PG-CHKPT-008", "requested_checkpoints"),
        ("PG-WAL-009", "wal_amplification"),
        ("PG-CACHE-010", "shared_buffer_misses"),
        ("PG-IO-011", "backend_io_latency"),
        ("PG-LOCK-012", "lock_wait_graph"),
        ("PG-HORIZON-013", "xmin_horizon_hold"),
        ("PG-CONN-014", "connection_saturation"),
        ("PG-REPL-015", "replication_lag"),
        ("PG-SLOT-016", "slot_wal_retention"),
        ("PG-ARCH-017", "wal_archiving_failure"),
        ("PG-SYNC-018", "sync_replication_wait"),
        ("PG-WAIT-019", "internal_wait_concentration"),
        ("OS-CPU-020", "cpu_saturation"),
        ("OS-CGRP-021", "cgroup_cpu_throttling"),
        ("OS-MEM-022", "memory_reclaim"),
        ("OS-CGMEM-023", "cgroup_memory_limit"),
        ("OS-BLOCK-024", "block_device_latency"),
        ("OS-WB-025", "writeback_pressure"),
        ("OS-IOWHO-026", "io_contender"),
        ("OS-FS-027", "filesystem_space"),
        ("OS-NET-028", "network_errors"),
    ];

    fn fixture(
        lens_id: &'static str,
        slug: &'static str,
        missing: &'static [MissingCapability],
    ) -> DormantLens {
        DormantLens {
            lens_id,
            slug,
            domain: Domain::Pg,
            title: "title",
            detects: "detects",
            confidence: ConfidenceCap::Medium,
            missing,
        }
    }

    #[test]
    fn stable_ids_keep_their_readable_aliases() {
        let actual: Vec<_> = dormant_catalog()
            .iter()
            .map(|lens| (lens.lens_id(), lens.slug()))
            .collect();
        assert_eq!(actual, EXPECTED_LENSES);
    }

    #[test]
    fn domain_and_capability_strings_are_stable() {
        assert_eq!(Domain::Pg.as_str(), "pg");
        assert_eq!(Domain::Os.as_str(), "os");
        assert_eq!(
            Missing::IncidentLogEventInput.as_str(),
            "incident_log_event_input"
        );
    }

    #[test]
    fn duplicate_ids_and_aliases_are_invalid() {
        let duplicate_id = [
            fixture("PG-A", "first", &[Missing::BlockedByEdges]),
            fixture("PG-A", "second", &[Missing::LockSnapshotCoverage]),
        ];
        let duplicate_slug = [
            fixture("PG-A", "same", &[Missing::BlockedByEdges]),
            fixture("PG-B", "same", &[Missing::LockSnapshotCoverage]),
        ];
        assert!(!catalog_is_valid(&duplicate_id));
        assert!(!catalog_is_valid(&duplicate_slug));
    }

    #[test]
    fn static_bounds_cover_missing_tokens_and_text() {
        let too_many = [fixture(
            "PG-A",
            "first",
            &[
                Missing::CounterDeltas,
                Missing::GaugeSamples,
                Missing::PairedIntervals,
                Missing::SourcePeriod,
                Missing::InputCoverage,
                Missing::EntityJoin,
                Missing::ActivityRows,
            ],
        )];
        let duplicate = [fixture(
            "PG-A",
            "first",
            &[Missing::BlockedByEdges, Missing::BlockedByEdges],
        )];
        assert!(!catalog_is_valid(&too_many));
        assert!(!catalog_is_valid(&duplicate));
        assert!(!slug_is_valid("Not_Snake_Case"));
        assert!(!json_text_is_bounded("raw \\\"quoted\\\" text"));
    }

    #[test]
    fn lock_requirements_preserve_the_structural_contract() {
        let lock = dormant_catalog()
            .iter()
            .find(|lens| lens.lens_id() == "PG-LOCK-012")
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

    #[test]
    fn proposal_event_candidates_are_accounted_once() {
        let expected = [
            "oom_kill",
            "backend_crash",
            "panic_shutdown",
            "disk_full_log",
            "out_of_memory_log",
            "connection_slots_exhausted",
            "deadlock",
            "data_corruption_log",
            "lock_wait_logged",
            "lock_timeout_log",
            "statement_timeout_log",
            "temp_file_spill_log",
            "slow_query_logged",
            "serialization_failure",
            "idle_in_transaction_abort",
            "checkpoint_too_frequent",
            "aggressive_autovacuum_wraparound",
            "autovacuum_cancel",
            "auth_password_failures",
            "pg_hba_rejections",
            "permission_denied_burst",
            "connection_storm_log",
            "archive_command_failure",
            "replication_disconnect",
            "recovery_conflict",
            "wal_integrity_log",
        ];
        let actual: Vec<_> = EVENT_CANDIDATES
            .iter()
            .map(|candidate| candidate.proposal_id)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.iter().copied().collect::<BTreeSet<_>>().len(), 26);
    }

    #[test]
    fn event_evidence_targets_existing_metric_lenses() {
        let metric_ids: BTreeSet<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        let mut split_facts = BTreeSet::new();
        for candidate in EVENT_CANDIDATES {
            match candidate.disposition {
                EventDisposition::Standalone => {}
                EventDisposition::EvidenceFor(id) | EventDisposition::RoadmapFor(id) => {
                    assert!(metric_ids.contains(id), "unknown target `{id}`");
                }
                EventDisposition::Split(facts) => {
                    assert!(facts.len() >= 2);
                    for fact in facts {
                        assert!(split_facts.insert(*fact), "duplicate split fact `{fact}`");
                    }
                }
            }
        }
    }

    #[test]
    fn public_metadata_contains_no_raw_sensitive_examples() {
        const FORBIDDEN: &[&str] = &[
            "select ",
            "password=",
            "postgresql://",
            "/var/",
            "archive_command=",
            "192.168.",
        ];
        for lens in dormant_catalog() {
            let text = format!("{} {}", lens.title(), lens.detects()).to_lowercase();
            for marker in FORBIDDEN {
                assert!(!text.contains(marker), "sensitive marker `{marker}`");
            }
        }
    }
}
