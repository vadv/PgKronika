//! Dormant diagnostic catalog metadata.

use super::evidence::ConfidenceCap;

const MAX_DORMANT_LENSES: usize = 28;
const MAX_LOG_DORMANT_LENSES: usize = 32;
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
    IncidentLogEventInput,
    LogDetailContinuation,
    LogSourceCoverage,
    EffectiveLogConfigCoverage,
    SensitiveLogRedaction,
    SourceClockProvenance,
    KernelOomVictimEvidence,
    StructuredLogIdentity,
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
            Self::LogDetailContinuation => "log_detail_continuation",
            Self::LogSourceCoverage => "log_source_coverage",
            Self::EffectiveLogConfigCoverage => "effective_log_config_coverage",
            Self::SensitiveLogRedaction => "sensitive_log_redaction",
            Self::SourceClockProvenance => "source_clock_provenance",
            Self::KernelOomVictimEvidence => "kernel_oom_victim_evidence",
            Self::StructuredLogIdentity => "structured_log_identity",
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
            Missing::IncidentLogEventInput,
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
            Missing::IncidentLogEventInput,
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
            Missing::IncidentLogEventInput,
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
            Missing::IncidentLogEventInput,
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
        lens_id: "cpu_saturation",
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
        lens_id: "cgroup_cpu_throttling",
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
        lens_id: "memory_reclaim",
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
        lens_id: "cgroup_memory_limit",
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
        lens_id: "block_device_latency",
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
        lens_id: "writeback_pressure",
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
        lens_id: "io_contender",
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
        lens_id: "filesystem_space",
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
        lens_id: "network_errors",
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

/// Dormant lenses reading events visible only in the `PostgreSQL` log, kept in
/// a sub-catalog separate from the metric one so each keeps its own size bound.
const LOG_DORMANT_CATALOG: &[DormantLens] = &[
    // Batch 1 (core): availability, resources, integrity. Self-contained, high.
    DormantLens {
        lens_id: "oom_kill",
        domain: Domain::Pg,
        title: "SIGKILL бэкенда",
        detects: "Был ли backend завершён сигналом 9? Жертва kernel-OOM — отдельный сигнал, signal 9 её не доказывает.",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogDetailContinuation,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "backend_crash",
        domain: Domain::Pg,
        title: "Аварийное завершение backend",
        detects: "Упал ли backend по сигналу (SIGSEGV/SIGABRT) с каскадом восстановления?",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogDetailContinuation,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "panic_shutdown",
        domain: Domain::Pg,
        title: "PANIC / аварийная остановка",
        detects: "Была ли запись severity PANIC и отдельный crash/restart? Не помечает повреждение данных автоматически.",
        confidence: ConfidenceCap::High,
        missing: &[Missing::IncidentLogEventInput, Missing::SourcePeriod],
    },
    DormantLens {
        lens_id: "disk_full_log",
        domain: Domain::Pg,
        title: "Нет места на диске (по логу)",
        detects: "Отказала ли запись из-за ENOSPC?",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "out_of_memory_log",
        domain: Domain::Pg,
        title: "Ошибка аллокации PostgreSQL (по логу)",
        detects: "Отказала ли аллокация PostgreSQL (SQLSTATE 53200)? Это ошибка аллокатора, не исчерпание физической RAM.",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "connection_slots_exhausted",
        domain: Domain::Pg,
        title: "Исчерпание слотов соединений",
        detects: "Отклонялись ли подключения по лимиту?",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "deadlock",
        domain: Domain::Pg,
        title: "Взаимоблокировка",
        detects: "Обнаружил ли PostgreSQL цикл блокировок с жертвой? Факт события, не доказанная причина инцидента.",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogDetailContinuation,
            Missing::EntityJoin,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "data_corruption_log",
        domain: Domain::Pg,
        title: "Повреждение данных (по логу)",
        detects: "Дала ли сбой завершённая проверка checksum/страницы? Не generic ошибка чтения или I/O.",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogDetailContinuation,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    // Batch 2 (locks and query performance): correlation/rate, medium.
    DormantLens {
        lens_id: "lock_wait_logged",
        domain: Domain::Pg,
        title: "Длительное ожидание блокировки",
        detects: "Кто и как долго ждал блокировку до её выдачи?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::LogSourceCoverage,
            Missing::EffectiveLogConfigCoverage,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "lock_timeout_log",
        domain: Domain::Pg,
        title: "Отмена по lock_timeout",
        detects: "Отменялись ли запросы по `lock_timeout`? Факт отмены, не доказанная причина инцидента.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "statement_timeout_log",
        domain: Domain::Pg,
        title: "Отмена по statement_timeout",
        detects: "Упирались ли запросы в `statement_timeout`? Факт отмены; таймаут не доказывает медленный сервер.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::LogDetailContinuation,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "temp_file_spill_log",
        domain: Domain::Pg,
        title: "Пролив во временные файлы",
        detects: "Сливались ли сортировки/хеши в temp-файлы, какого размера?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::LogSourceCoverage,
            Missing::EffectiveLogConfigCoverage,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "slow_query_logged",
        domain: Domain::Pg,
        title: "Медленный запрос (по логу)",
        detects: "Превышен ли настроенный порог длительности конкретным запросом? Сам по себе не аномалия.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::LogSourceCoverage,
            Missing::EffectiveLogConfigCoverage,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "serialization_failure",
        domain: Domain::Pg,
        title: "Сбой сериализации транзакций",
        detects: "Всплеск ли откатов по конфликту сериализации?",
        confidence: ConfidenceCap::Medium,
        missing: &[Missing::IncidentLogEventInput, Missing::SourcePeriod],
    },
    DormantLens {
        lens_id: "idle_in_transaction_abort",
        domain: Domain::Pg,
        title: "Обрыв по idle-in-transaction",
        detects: "Убивались ли зависшие в транзакции сессии?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    // Batch 3 (maintenance, security, replication): medium/low.
    DormantLens {
        lens_id: "checkpoint_too_frequent",
        domain: Domain::Pg,
        title: "Слишком частые контрольные точки",
        detects: "Форсирует ли WAL-давление внеплановые чекпоинты?",
        confidence: ConfidenceCap::Medium,
        missing: &[Missing::IncidentLogEventInput, Missing::SourcePeriod],
    },
    DormantLens {
        lens_id: "aggressive_autovacuum_wraparound",
        domain: Domain::Pg,
        title: "Агрессивный autovacuum против wraparound",
        detects: "Запускался ли аварийный anti-wraparound freeze?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "autovacuum_cancel",
        domain: Domain::Pg,
        title: "Отмена autovacuum под блокировкой",
        detects: "Отменяется ли autovacuum конфликтующими локами (DDL)?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "auth_password_failures",
        domain: Domain::Pg,
        title: "Всплеск неверных паролей",
        detects: "Всплеск ли отказов аутентификации по паролю?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogSourceCoverage,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "pg_hba_rejections",
        domain: Domain::Pg,
        title: "Отказы по pg_hba",
        detects: "Стучится ли неизвестный хост/БД/пользователь мимо pg_hba?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "permission_denied_burst",
        domain: Domain::Pg,
        title: "Всплеск отказов доступа (RBAC)",
        detects: "Всплеск ли `permission denied` (обычно кривой деплой грантов)?",
        confidence: ConfidenceCap::Low,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "connection_storm_log",
        domain: Domain::Pg,
        title: "Шторм подключений",
        detects: "Резкий churn коннектов без упора в лимит?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogSourceCoverage,
            Missing::EffectiveLogConfigCoverage,
            Missing::EntityJoin,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "archive_command_failure",
        domain: Domain::Pg,
        title: "Сбой archive_command (по логу)",
        detects: "Почему падает архивация WAL (exit-код, stderr)?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "replication_disconnect",
        domain: Domain::Pg,
        title: "Обрыв соединения репликации",
        detects: "Оборвался ли поток репликации? Событие для стороны: walsender на primary, walreceiver на standby.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::EntityJoin,
            Missing::SensitiveLogRedaction,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "recovery_conflict",
        domain: Domain::Pg,
        title: "Конфликт восстановления на реплике",
        detects: "Отменяются ли запросы на реплике конфликтом с replay?",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::LogDetailContinuation,
            Missing::SourcePeriod,
        ],
    },
    DormantLens {
        lens_id: "wal_integrity_log",
        domain: Domain::Pg,
        title: "Проблемы целостности WAL",
        detects: "Сбой валидации WAL из archive/stream? Локальный конец WAL (invalid record length в pg_wal) легитимен, не finding.",
        confidence: ConfidenceCap::High,
        missing: &[Missing::IncidentLogEventInput, Missing::SourcePeriod],
    },
    // Batch 4: finer-grained siblings that separate causes the core lenses merge.
    DormantLens {
        lens_id: "kernel_oom_victim",
        domain: Domain::Os,
        title: "Жертва OOM-killer ядра",
        detects: "Убил ли OOM-killer ядра конкретный процесс (victim PID)? Signal 9 у backend этого не доказывает.",
        confidence: ConfidenceCap::Medium,
        missing: &[
            Missing::KernelOomVictimEvidence,
            Missing::EntityJoin,
            Missing::SourceClockProvenance,
        ],
    },
    DormantLens {
        lens_id: "lock_table_exhaustion",
        domain: Domain::Pg,
        title: "Исчерпание таблицы блокировок",
        detects: "Отказала ли операция из-за нехватки shared memory под таблицу блокировок (\"out of shared memory\")?",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::StructuredLogIdentity,
            Missing::LogSourceCoverage,
            Missing::SourceClockProvenance,
        ],
    },
    DormantLens {
        lens_id: "shared_memory_alloc_failure",
        domain: Domain::Pg,
        title: "Сбой аллокации разделяемой памяти",
        detects: "Не удалось выделить или изменить сегмент разделяемой памяти (DSM)?",
        confidence: ConfidenceCap::High,
        missing: &[
            Missing::IncidentLogEventInput,
            Missing::StructuredLogIdentity,
            Missing::LogSourceCoverage,
            Missing::SourceClockProvenance,
        ],
    },
];

/// Log lenses whose single record is a self-contained finding — activate first.
const CORE_LOG_LENS_IDS: &[&str] = &[
    "oom_kill",
    "backend_crash",
    "panic_shutdown",
    "disk_full_log",
    "out_of_memory_log",
    "connection_slots_exhausted",
    "deadlock",
    "data_corruption_log",
];

/// Returns dormant log-lens design entries, separate from the metric catalog.
pub(crate) const fn log_dormant_catalog() -> &'static [DormantLens] {
    LOG_DORMANT_CATALOG
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
    catalog_within_bounds(catalog, MAX_DORMANT_LENSES)
}

const fn log_catalog_is_valid(catalog: &[DormantLens]) -> bool {
    catalog_within_bounds(catalog, MAX_LOG_DORMANT_LENSES)
}

const fn catalog_within_bounds(catalog: &[DormantLens], max_lenses: usize) -> bool {
    if catalog.is_empty() || catalog.len() > max_lenses {
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

const _: () = assert!(
    log_catalog_is_valid(LOG_DORMANT_CATALOG),
    "log dormant catalog violates static bounds"
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
        "cpu_saturation",
        "cgroup_cpu_throttling",
        "memory_reclaim",
        "cgroup_memory_limit",
        "block_device_latency",
        "writeback_pressure",
        "io_contender",
        "filesystem_space",
        "network_errors",
    ];

    const LOG_EXPECTED_LENSES: [&str; 29] = [
        // Batch 1 (core)
        "oom_kill",
        "backend_crash",
        "panic_shutdown",
        "disk_full_log",
        "out_of_memory_log",
        "connection_slots_exhausted",
        "deadlock",
        "data_corruption_log",
        // Batch 2
        "lock_wait_logged",
        "lock_timeout_log",
        "statement_timeout_log",
        "temp_file_spill_log",
        "slow_query_logged",
        "serialization_failure",
        "idle_in_transaction_abort",
        // Batch 3
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
        // Batch 4 (audit splits)
        "kernel_oom_victim",
        "lock_table_exhaustion",
        "shared_memory_alloc_failure",
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
    fn log_capability_tokens_are_stable() {
        assert_eq!(
            Missing::IncidentLogEventInput.as_str(),
            "incident_log_event_input"
        );
        assert_eq!(
            Missing::LogDetailContinuation.as_str(),
            "log_detail_continuation"
        );
        assert_eq!(
            Missing::SensitiveLogRedaction.as_str(),
            "sensitive_log_redaction"
        );
        assert_eq!(Missing::LogSourceCoverage.as_str(), "log_source_coverage");
        assert_eq!(
            Missing::EffectiveLogConfigCoverage.as_str(),
            "effective_log_config_coverage"
        );
        assert_eq!(
            Missing::SourceClockProvenance.as_str(),
            "source_clock_provenance"
        );
        assert_eq!(
            Missing::KernelOomVictimEvidence.as_str(),
            "kernel_oom_victim_evidence"
        );
        assert_eq!(
            Missing::StructuredLogIdentity.as_str(),
            "structured_log_identity"
        );
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

    #[test]
    fn log_catalog_satisfies_its_static_bounds() {
        assert!(log_catalog_is_valid(log_dormant_catalog()));
        assert!(!log_dormant_catalog().is_empty());
    }

    #[test]
    fn log_lenses_are_pg_except_the_kernel_oom_victim() {
        for lens in log_dormant_catalog() {
            let expected = if lens.lens_id() == "kernel_oom_victim" {
                "os"
            } else {
                "pg"
            };
            assert_eq!(
                lens.domain().as_str(),
                expected,
                "unexpected domain for `{}`",
                lens.lens_id()
            );
        }
    }

    #[test]
    fn core_log_lens_ids_are_catalogued() {
        for id in CORE_LOG_LENS_IDS {
            assert!(
                log_dormant_catalog()
                    .iter()
                    .any(|lens| lens.lens_id() == *id),
                "core log lens `{id}` is missing from the catalog"
            );
        }
    }

    #[test]
    fn log_catalog_ids_match_the_contract_order() {
        let ids: Vec<_> = log_dormant_catalog()
            .iter()
            .map(DormantLens::lens_id)
            .collect();
        assert_eq!(ids, LOG_EXPECTED_LENSES);
    }
}
