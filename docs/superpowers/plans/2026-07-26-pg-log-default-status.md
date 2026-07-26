# PostgreSQL Log Default Collection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Включить попытку сбора файлового `stderr` PostgreSQL по умолчанию и дать оператору однозначный, ограниченно читаемый status источника в PGM и `/v1/sources`.

**Architecture:** Коллектор разделяет состояние доступности источника и доказанную потерю данных: `pg_log_source_status` хранит первое наблюдение, переходы и heartbeat, а `pg_log_gap` остаётся журналом потерь. Машина status готовит транзакционное обновление во время collection и подтверждает его только после успешной записи окна; reader ищет последнюю строку status с конца по каталогам и не материализует всю историю heartbeat.

**Tech Stack:** Rust 1.96.0, Tokio, tokio-postgres, PGM v1, `kronika-registry` derive codecs, `kronika-writer`, `kronika-reader`, Axum, serde_json, Cucumber BDD.

**Design:** `docs/superpowers/specs/2026-07-26-pg-log-default-status-design.md`.

## Global Constraints

- Работать в текущем checkout и текущей ветке; отдельный worktree не создавать.
- Не добавлять внешние зависимости и не менять формат контейнера PGM v1.
- Поддерживаемый источник в этой работе: только файловый PostgreSQL `stderr`.
- `KRONIKA_PG_LOG_ENABLED` без значения означает `true`; явное ложное значение всегда имеет приоритет над `KRONIKA_LOG_PATH`.
- `KRONIKA_LOG_PATH` только переопределяет путь; коллектор не меняет GUC, владельца, группу или режим файла.
- Первый read нового файла начинается с EOF, если оператор не установил `KRONIKA_LOG_START_AT_BEGINNING=1`.
- `KRONIKA_PG_LOG_STATUS_INTERVAL_S` по умолчанию равен `300` и обязан быть больше нуля.
- `pg_log_source_status` имеет `type_id=1_039_001`, логическое имя `pg_log_source_status` и семантику `on_change`.
- Коды `state`: `0=collecting`, `1=collecting_degraded`, `2=unavailable`, `3=disabled`.
- Коды `reason`: `0=none`, `1=postgres_unavailable`, `2=no_current_logfile`, `3=unsupported_format`, `4=discovery_query_failed`, `5=missing_file`, `6=permission_denied`, `7=read_error`.
- Status создаётся при первой попытке, при изменении ключа состояния и каждые 300 секунд при неизменном ключе.
- Начальная недоступность не создаёт `pg_log_gap`; после ранее успешного чтения один непрерывный отказ создаёт не более одного outage-gap на процесс.
- Существующие числовые коды и семантика `pg_log_gap` не меняются.
- Status является свидетельством качества сбора и не влияет на оценку PostgreSQL, аномалии или их пороги.
- Английская и русская документация должны описывать одно поведение; русские идентификаторы, ключи JSON и переменные окружения не переводить.
- Перед каждым коммитом с Rust-кодом запускать `cargo +1.96.0 fmt --all`.
- Локальные Cargo-проверки запускать с host target, потому что репозиторий по умолчанию выбирает musl:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
```

---

## File Map

**Create**

- `crates/kronika-source-log/src/status.rs` — коды состояния, status row и транзакционный heartbeat/outage tracker.
- `crates/kronika-reader/src/query/latest.rs` — ограниченный поиск последней строки логической секции.

**Modify**

- `crates/kronika-registry/src/codec/pg_log.rs` — on-disk контракт `PgLogSourceStatusV1`.
- `crates/kronika-registry/src/lib.rs` — регистрация `1_039_001`.
- `crates/kronika-source-log/src/lib.rs` — публичные типы status и type id.
- `crates/kronika-source-log/src/collector.rs` — discovery cache, read/status state machine и outage-gap.
- `crates/kronika-source-log/src/parser.rs` — стабильное текстовое имя parser kind для process log.
- `crates/kronika-source-log/src/state.rs` — использование общего имени parser kind в tail state.
- `bins/pg_kronika-collector/src/config.rs` — default-on и heartbeat interval.
- `bins/pg_kronika-collector/src/tests/config.rs` — чистые тесты разрешения env-контракта.
- `bins/pg_kronika-demo/src/collector.rs` — удаление локального default override.
- `bins/pg_kronika-collector/src/pg_log_source.rs` — буферизация status и transition/heartbeat logging.
- `bins/pg_kronika-collector/src/tests/buffering.rs` — проверка status section в PGM.
- `crates/kronika-reader/src/query/mod.rs`, `crates/kronika-reader/src/lib.rs` — экспорт latest query.
- `bins/pg_kronika-web/src/handlers/v1.rs` — поле `pg_log` в `/v1/sources`.
- `bins/pg_kronika-web/src/tests/anomalies.rs` — старые и новые status fixtures.
- `bins/pg_kronika-web/openapi.json` — аддитивная схема ответа.
- `crates/kronika-bdd/src/cluster.rs` — файловый logging collector в тестовом PostgreSQL.
- `crates/kronika-bdd/src/harness/snapshot.rs` — длительный сценарий ротации.
- `crates/kronika-bdd/src/harness/web.rs`, `crates/kronika-bdd/src/steps/web.rs` — BDD-проверка `/v1/sources`.
- `crates/kronika-bdd/src/steps/log.rs`, `crates/kronika-bdd/features/pg_log.feature` — discovery, quiet read и ротация.
- `bins/pg_kronika-collector/README.md`, `bins/pg_kronika-collector/README.ru.md` — операторский контракт.
- `crates/kronika-registry/README.md`, `crates/kronika-registry/README.ru.md` — назначение новой секции.
- `docs/type-registry.md`, `docs/type-registry/postgresql.md` — диапазон type id, поля, коды и связь status/gap.

### Task 1: Register `pg_log_source_status`

**Files:**
- Modify: `crates/kronika-registry/src/codec/pg_log.rs`
- Modify: `crates/kronika-registry/src/lib.rs:149-158`
- Test: `crates/kronika-registry/src/codec/pg_log.rs`

**Interfaces:**
- Consumes: существующие `Section`, `StrId`, `Ts`, `Semantics::OnChange`.
- Produces: `kronika_registry::pg_log::PgLogSourceStatusV1` с `CONTRACT.type_id=1_039_001`.

- [ ] **Step 1: Write failing contract and round-trip tests**

Добавить в существующий `mod tests`:

```rust
#[test]
fn source_status_contract_shape() {
    let contract = PgLogSourceStatusV1::CONTRACT;
    assert_eq!(contract.type_id.get(), 1_039_001);
    assert_eq!(contract.name, "pg_log_source_status");
    assert_eq!(contract.semantics, crate::Semantics::OnChange);
    assert_eq!(contract.sort_key, ["ts"]);
    assert_eq!(contract.columns.len(), 6);
    assert_eq!(
        contract.column("source_path").map(|column| column.nullable),
        Some(true)
    );
}

#[test]
fn source_status_roundtrip_preserves_every_state_and_nullable_path() {
    crate::assert_roundtrips(&[
        PgLogSourceStatusV1 {
            ts: Ts(10),
            state: 0,
            reason: 0,
            parser_kind: 0,
            source_path: Some(StrId(7)),
            dict_dropped_fields: 0,
        },
        PgLogSourceStatusV1 {
            ts: Ts(20),
            state: 1,
            reason: 4,
            parser_kind: 0,
            source_path: Some(StrId(7)),
            dict_dropped_fields: 0,
        },
        PgLogSourceStatusV1 {
            ts: Ts(30),
            state: 2,
            reason: 6,
            parser_kind: 2,
            source_path: None,
            dict_dropped_fields: 1,
        },
        PgLogSourceStatusV1 {
            ts: Ts(40),
            state: 3,
            reason: 0,
            parser_kind: 2,
            source_path: None,
            dict_dropped_fields: 0,
        },
    ]);
}
```

- [ ] **Step 2: Run the focused tests and confirm the red state**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-registry pg_log::tests::source_status --target "$HOST"
```

Expected: compilation fails because `PgLogSourceStatusV1` does not exist.

- [ ] **Step 3: Add the exact on-disk row and register it**

Add beside the other log-domain rows:

```rust
/// Type `1_039_001`: availability of the PostgreSQL log source.
///
/// One row is emitted on first observation, on a state-key change, and as a
/// bounded heartbeat. It describes collection quality, not PostgreSQL health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_039_001,
    name = "pg_log_source_status",
    semantics = on_change,
    sort_key("ts")
)]
pub struct PgLogSourceStatusV1 {
    /// Observation time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// `0` collecting, `1` collecting_degraded, `2` unavailable, `3` disabled.
    #[column(l)]
    pub state: u8,
    /// `0` none through `7` read_error, as documented in the registry.
    #[column(l)]
    pub reason: u8,
    /// `0` stderr, `1` csvlog, `2` unknown.
    #[column(l)]
    pub parser_kind: u8,
    /// Current or last known source path.
    #[column(l)]
    pub source_path: Option<StrId>,
    /// String fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}
```

Insert `pg_log::PgLogSourceStatusV1::CONTRACT` in `registry()` after the other
log contracts and before Linux contracts. Do not renumber or reorder existing
type ids.

- [ ] **Step 4: Run registry tests and lint**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-registry pg_log --target "$HOST"
cargo +1.96.0 test -p kronika-registry registry_lints_cleanly --target "$HOST"
```

Expected: both commands pass; registry lint sees `1_039_001` exactly once.

- [ ] **Step 5: Commit the registry contract**

```sh
git add crates/kronika-registry/src/codec/pg_log.rs crates/kronika-registry/src/lib.rs
git commit -m "feat(registry): add PostgreSQL log source status"
```

### Task 2: Make Log Collection Default-On

**Files:**
- Modify: `crates/kronika-source-log/src/collector.rs:18-51`
- Modify: `bins/pg_kronika-collector/src/config.rs:77-92,255-286`
- Modify: `bins/pg_kronika-collector/src/tests/config.rs`
- Modify: `bins/pg_kronika-demo/src/collector.rs:44-61`
- Test: `bins/pg_kronika-collector/src/tests/config.rs`

**Interfaces:**
- Consumes: existing boolean spelling accepted by `env_bool`.
- Produces: `LogConfig::status_interval: Duration`, `resolve_log_enabled(Option<&str>)`, `resolve_log_status_interval(Option<&str>)`.

- [ ] **Step 1: Write pure failing configuration tests**

Export the two pure helpers as `pub(crate)` and add:

```rust
use crate::config::{resolve_log_enabled, resolve_log_status_interval};
use std::time::Duration;

#[test]
fn pg_log_is_enabled_when_the_flag_is_absent() {
    assert!(resolve_log_enabled(None).expect("default log flag"));
}

#[test]
fn explicit_false_disables_pg_log_independently_of_a_path_override() {
    let path_override = Some("/var/lib/postgresql/log/postgresql.log");
    assert!(path_override.is_some());
    assert!(!resolve_log_enabled(Some("0")).expect("explicit false"));
}

#[test]
fn pg_log_status_interval_defaults_to_five_minutes() {
    assert_eq!(
        resolve_log_status_interval(None).expect("default status interval"),
        Duration::from_secs(300)
    );
}

#[test]
fn pg_log_status_interval_rejects_zero() {
    let error = resolve_log_status_interval(Some("0"))
        .expect_err("a zero heartbeat interval must fail");
    assert!(
        error
            .to_string()
            .contains("KRONIKA_PG_LOG_STATUS_INTERVAL_S")
    );
}
```

- [ ] **Step 2: Run the focused tests and confirm the red state**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p pg-kronika-collector pg_log_ --target "$HOST"
```

Expected: compilation fails because both resolver functions and
`LogConfig::status_interval` are absent.

- [ ] **Step 3: Implement value resolution without mutating process env in tests**

Add:

```rust
fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("{key} must be one of 1/0, true/false, yes/no, on/off"),
    }
}

pub(crate) fn resolve_log_enabled(raw: Option<&str>) -> Result<bool> {
    raw.map_or(Ok(true), |value| {
        parse_bool("KRONIKA_PG_LOG_ENABLED", value)
    })
}

pub(crate) fn resolve_log_status_interval(raw: Option<&str>) -> Result<Duration> {
    let seconds = raw.map_or(Ok(300), |value| {
        value
            .parse::<u64>()
            .context("KRONIKA_PG_LOG_STATUS_INTERVAL_S is not a u64")
    })?;
    anyhow::ensure!(
        seconds > 0,
        "KRONIKA_PG_LOG_STATUS_INTERVAL_S must be greater than 0"
    );
    Ok(Duration::from_secs(seconds))
}
```

Make `env_bool` delegate to `parse_bool`. In `log_config_from_env`, read the raw
values once and use the new helpers:

```rust
let enabled_raw = std::env::var("KRONIKA_PG_LOG_ENABLED").ok();
let enabled = resolve_log_enabled(enabled_raw.as_deref())?;
let status_interval_raw = std::env::var("KRONIKA_PG_LOG_STATUS_INTERVAL_S").ok();
let status_interval = resolve_log_status_interval(status_interval_raw.as_deref())?;
```

Add `pub status_interval: Duration` to `LogConfig`; assign it in
`log_config_from_env`, `LogConfig::disabled` and every test fixture. The disabled
constructor uses `Duration::from_mins(5)`.

- [ ] **Step 4: Remove the demo-only default**

Replace `Collector::spawn` with:

```rust
pub(crate) fn spawn(dsn: &str, paths: &StandPaths, config: &Config) -> Result<Self> {
    Self::spawn_with(dsn, paths, config, &[])
}
```

Delete the comment claiming that the stand enables a source which the collector
ships disabled. Keep `spawn_with` because `seal_tail` still passes
`KRONIKA_INTERVAL_S=0`.

- [ ] **Step 5: Run collector, source-log and demo checks**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p pg-kronika-collector config --target "$HOST"
cargo +1.96.0 test -p kronika-source-log --target "$HOST"
cargo +1.96.0 check -p pg-kronika-demo --target "$HOST"
```

Expected: all commands pass; no `LogConfig` initializer omits
`status_interval`.

- [ ] **Step 6: Commit the default and interval contract**

```sh
git add crates/kronika-source-log/src/collector.rs bins/pg_kronika-collector/src/config.rs bins/pg_kronika-collector/src/tests/config.rs bins/pg_kronika-demo/src/collector.rs
git commit -m "feat(collector): enable PostgreSQL log collection by default"
```

### Task 3: Add the Transactional Status Tracker

**Files:**
- Create: `crates/kronika-source-log/src/status.rs`
- Modify: `crates/kronika-source-log/src/lib.rs`
- Modify: `crates/kronika-source-log/src/parser.rs:16-39`
- Modify: `crates/kronika-source-log/src/state.rs:67-78`
- Test: `crates/kronika-source-log/src/status.rs`

**Interfaces:**
- Consumes: `ParserKind`, `PathBuf`, `Duration`, `Instant`.
- Produces: public `LogSourceState`, `LogSourceReason`, `LogSourceStatus`; crate-private `StatusTracker`, `StatusUpdate`.

- [ ] **Step 1: Write failing tracker tests**

Create the module with tests that pin first emission, change emission, heartbeat,
transactional commit and outage deduplication:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{
        LogSourceReason, LogSourceState, LogSourceStatus, StatusTracker,
    };
    use crate::ParserKind;

    fn status(ts: i64, state: LogSourceState, reason: LogSourceReason) -> LogSourceStatus {
        LogSourceStatus {
            ts,
            state,
            reason,
            parser_kind: ParserKind::Stderr,
            source_path: Some(PathBuf::from("/pg/log/postgresql.log")),
        }
    }

    #[test]
    fn first_observation_change_and_heartbeat_emit_exactly_once() {
        let started = Instant::now();
        let mut tracker = StatusTracker::new(Duration::from_secs(300), false);

        let first = tracker.observe(
            status(10, LogSourceState::Collecting, LogSourceReason::None),
            started,
        );
        assert!(first.changed);
        assert_eq!(first.row.as_ref().map(|row| row.ts), Some(10));
        tracker.commit(&first);

        let quiet = tracker.observe(
            status(20, LogSourceState::Collecting, LogSourceReason::None),
            started + Duration::from_secs(299),
        );
        assert!(quiet.row.is_none());
        tracker.commit(&quiet);

        let heartbeat = tracker.observe(
            status(30, LogSourceState::Collecting, LogSourceReason::None),
            started + Duration::from_secs(300),
        );
        assert!(!heartbeat.changed);
        assert_eq!(heartbeat.row.as_ref().map(|row| row.ts), Some(30));
    }

    #[test]
    fn an_uncommitted_transition_is_offered_again() {
        let now = Instant::now();
        let tracker = StatusTracker::new(Duration::from_secs(300), false);
        let first = tracker.observe(
            status(10, LogSourceState::Unavailable, LogSourceReason::MissingFile),
            now,
        );
        let retry = tracker.observe(
            status(11, LogSourceState::Unavailable, LogSourceReason::MissingFile),
            now + Duration::from_secs(1),
        );
        assert!(first.row.is_some());
        assert!(retry.row.is_some());
        assert!(retry.changed);
    }

    #[test]
    fn one_outage_gap_is_allowed_until_a_successful_recovery() {
        let now = Instant::now();
        let mut tracker = StatusTracker::new(Duration::from_secs(300), false);
        let healthy = tracker.observe(
            status(1, LogSourceState::Collecting, LogSourceReason::None),
            now,
        );
        tracker.commit(&healthy);

        let first_failure = tracker.observe(
            status(2, LogSourceState::Unavailable, LogSourceReason::MissingFile),
            now + Duration::from_secs(1),
        );
        assert!(first_failure.outage_started);
        tracker.commit(&first_failure);

        let repeated = tracker.observe(
            status(3, LogSourceState::Unavailable, LogSourceReason::MissingFile),
            now + Duration::from_secs(2),
        );
        assert!(!repeated.outage_started);

        let recovered = tracker.observe(
            status(4, LogSourceState::Collecting, LogSourceReason::None),
            now + Duration::from_secs(3),
        );
        tracker.commit(&recovered);
        let second_failure = tracker.observe(
            status(5, LogSourceState::Unavailable, LogSourceReason::ReadError),
            now + Duration::from_secs(4),
        );
        assert!(second_failure.outage_started);
    }

    #[test]
    fn persisted_tail_state_allows_one_restart_outage_gap() {
        let now = Instant::now();
        let tracker = StatusTracker::new(Duration::from_secs(300), true);
        let failure = tracker.observe(
            status(1, LogSourceState::Unavailable, LogSourceReason::PermissionDenied),
            now,
        );
        assert!(failure.outage_started);
    }
}
```

- [ ] **Step 2: Run the module tests and confirm the red state**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-source-log status::tests --target "$HOST"
```

Expected: compilation fails because the status types and tracker are not
implemented.

- [ ] **Step 3: Implement stable codes and names**

Add the public types with exhaustive mappings:

```rust
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::ParserKind;

/// Final result of one PostgreSQL log-source observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSourceState {
    /// The current supported file was opened and processed.
    Collecting,
    /// The last path was processed, but discovery could not be refreshed.
    CollectingDegraded,
    /// No supported file could be read.
    Unavailable,
    /// The operator explicitly disabled the source.
    Disabled,
}

impl LogSourceState {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Collecting => 0,
            Self::CollectingDegraded => 1,
            Self::Unavailable => 2,
            Self::Disabled => 3,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Collecting => "collecting",
            Self::CollectingDegraded => "collecting_degraded",
            Self::Unavailable => "unavailable",
            Self::Disabled => "disabled",
        }
    }

    const fn proves_read(self) -> bool {
        matches!(self, Self::Collecting | Self::CollectingDegraded)
    }
}

/// Why the source has its final state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSourceReason {
    /// No degradation.
    None,
    /// No PostgreSQL client was available for discovery.
    PostgresUnavailable,
    /// PostgreSQL reported no current stderr file.
    NoCurrentLogfile,
    /// The selected log format is unsupported.
    UnsupportedFormat,
    /// A discovery SQL query failed.
    DiscoveryQueryFailed,
    /// The known path did not exist.
    MissingFile,
    /// The collector lacked permission to open the path.
    PermissionDenied,
    /// Another I/O error prevented reading.
    ReadError,
}

impl LogSourceReason {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::PostgresUnavailable => 1,
            Self::NoCurrentLogfile => 2,
            Self::UnsupportedFormat => 3,
            Self::DiscoveryQueryFailed => 4,
            Self::MissingFile => 5,
            Self::PermissionDenied => 6,
            Self::ReadError => 7,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PostgresUnavailable => "postgres_unavailable",
            Self::NoCurrentLogfile => "no_current_logfile",
            Self::UnsupportedFormat => "unsupported_format",
            Self::DiscoveryQueryFailed => "discovery_query_failed",
            Self::MissingFile => "missing_file",
            Self::PermissionDenied => "permission_denied",
            Self::ReadError => "read_error",
        }
    }
}

/// One source-status row before dictionary interning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSourceStatus {
    /// Observation time, unix microseconds.
    pub ts: i64,
    /// Final availability state.
    pub state: LogSourceState,
    /// Reason for the final state.
    pub reason: LogSourceReason,
    /// Parser selected for the known source.
    pub parser_kind: ParserKind,
    /// Current or last known path.
    pub source_path: Option<PathBuf>,
}
```

- [ ] **Step 4: Implement staged tracker updates**

The tracker compares all fields except `ts`. `observe` clones tracker state and
never mutates the committed tracker; `commit` applies the staged clone:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusKey {
    state: LogSourceState,
    reason: LogSourceReason,
    parser_kind: ParserKind,
    source_path: Option<PathBuf>,
}

impl From<&LogSourceStatus> for StatusKey {
    fn from(status: &LogSourceStatus) -> Self {
        Self {
            state: status.state,
            reason: status.reason,
            parser_kind: status.parser_kind,
            source_path: status.source_path.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StatusTracker {
    heartbeat_interval: Duration,
    current: Option<LogSourceStatus>,
    next_heartbeat: Option<Instant>,
    ever_collected: bool,
    outage_reported: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusUpdate {
    pub(crate) row: Option<LogSourceStatus>,
    pub(crate) previous: Option<LogSourceStatus>,
    pub(crate) changed: bool,
    pub(crate) outage_started: bool,
    next: StatusTracker,
}

impl StatusTracker {
    pub(crate) const fn new(heartbeat_interval: Duration, had_success: bool) -> Self {
        Self {
            heartbeat_interval,
            current: None,
            next_heartbeat: None,
            ever_collected: had_success,
            outage_reported: false,
        }
    }

    pub(crate) fn observe(&self, status: LogSourceStatus, now: Instant) -> StatusUpdate {
        let mut next = self.clone();
        let previous = next.current.clone();
        let key = StatusKey::from(&status);
        let changed = previous
            .as_ref()
            .map(StatusKey::from)
            .as_ref()
            != Some(&key);
        let heartbeat_due = next.next_heartbeat.is_some_and(|deadline| now >= deadline);
        let emit = changed || previous.is_none() || heartbeat_due;

        let outage_started = status.state == LogSourceState::Unavailable
            && next.ever_collected
            && !next.outage_reported;
        if status.state.proves_read() {
            next.ever_collected = true;
            next.outage_reported = false;
        } else if outage_started {
            next.outage_reported = true;
        }

        next.current = Some(status.clone());
        if emit {
            next.next_heartbeat = Some(now + next.heartbeat_interval);
        }
        StatusUpdate {
            row: emit.then_some(status),
            previous,
            changed,
            outage_started,
            next,
        }
    }

    pub(crate) fn commit(&mut self, update: &StatusUpdate) {
        *self = update.next.clone();
    }
}
```

- [ ] **Step 5: Export the source contract types**

In `lib.rs`, add `mod status`, re-export the three public types, and define:

```rust
/// Type id for PostgreSQL log source availability.
pub const PG_LOG_SOURCE_STATUS_TYPE_ID: u32 = 1_039_001;
```

Expose and reuse one parser name mapping:

```rust
#[must_use]
pub const fn as_str(self) -> &'static str {
    match self {
        Self::Stderr => "stderr",
        Self::Csvlog => "csvlog",
        Self::Unknown => "unknown",
    }
}
```

Replace `as_state_value()` with `as_str()` in `TailState::render`; delete
`as_state_value` so process logs and persisted state cannot drift.

- [ ] **Step 6: Run tests and commit**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-source-log status::tests --target "$HOST"
```

Expected: all four tracker tests pass.

```sh
git add crates/kronika-source-log/src/status.rs crates/kronika-source-log/src/lib.rs crates/kronika-source-log/src/parser.rs crates/kronika-source-log/src/state.rs
git commit -m "feat(source-log): track PostgreSQL log source status"
```

### Task 4: Integrate Discovery, Read State and Gap Deduplication

**Files:**
- Modify: `crates/kronika-source-log/src/collector.rs`
- Modify: `bins/pg_kronika-collector/src/pg_log_source.rs:139-151`
- Test: `crates/kronika-source-log/src/collector.rs`

**Interfaces:**
- Consumes: `StatusTracker::new`, `StatusTracker::observe`, `StatusTracker::commit`, `LogSourceStatus`.
- Produces: `LogCollection::{source_status,previous_source_status,source_status_changed,next_discovery_in}` and a staged update committed by `LogCollector::commit`.

- [ ] **Step 1: Add failing end-to-end state-machine tests**

Extend the existing collector tests. Use the existing `fixture_config`, add
`status_interval`, and expose a test-only deterministic
`collect_at(client, ts, now)`:

```rust
fn source_state(collection: &LogCollection) -> (LogSourceState, LogSourceReason) {
    let status = collection
        .source_status
        .as_ref()
        .expect("this observation emits status");
    (status.state, status.reason)
}

#[tokio::test]
async fn quiet_read_emits_collecting_then_waits_for_heartbeat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("postgresql.log");
    std::fs::write(&log, "").expect("write empty log");
    let mut config = fixture_config(log, dir.path().join("state"));
    config.status_interval = Duration::from_secs(300);
    let mut collector = LogCollector::new(config).expect("collector");
    let now = Instant::now();

    let first = collector.collect_at(None, 10, now).await;
    assert_eq!(
        source_state(&first),
        (LogSourceState::Collecting, LogSourceReason::None)
    );
    assert!(first.gaps.is_empty());
    collector.commit(&first).expect("commit first");

    let quiet = collector
        .collect_at(None, 20, now + Duration::from_secs(299))
        .await;
    assert!(quiet.source_status.is_none());
    collector.commit(&quiet).expect("commit quiet observation");

    let heartbeat = collector
        .collect_at(None, 30, now + Duration::from_secs(300))
        .await;
    assert_eq!(
        source_state(&heartbeat),
        (LogSourceState::Collecting, LogSourceReason::None)
    );
    assert!(!heartbeat.source_status_changed);
}

#[tokio::test]
async fn a_continuous_missing_file_emits_one_gap_after_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("postgresql.log");
    std::fs::write(&log, "").expect("write log");
    let mut collector =
        LogCollector::new(fixture_config(log.clone(), dir.path().join("state")))
            .expect("collector");
    let now = Instant::now();
    let healthy = collector.collect_at(None, 1, now).await;
    collector.commit(&healthy).expect("commit healthy read");

    std::fs::remove_file(&log).expect("remove log");
    let first = collector
        .collect_at(None, 2, now + Duration::from_secs(1))
        .await;
    assert_eq!(
        source_state(&first),
        (LogSourceState::Unavailable, LogSourceReason::MissingFile)
    );
    assert_eq!(
        first
            .gaps
            .iter()
            .filter(|gap| gap.reason == GapReason::MissingFile)
            .count(),
        1
    );
    collector.commit(&first).expect("commit first outage");

    let repeated = collector
        .collect_at(None, 3, now + Duration::from_secs(2))
        .await;
    assert!(repeated.source_status.is_none());
    assert!(repeated.gaps.is_empty());
}

#[tokio::test]
async fn recovery_allows_a_later_outage_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("postgresql.log");
    std::fs::write(&log, "").expect("write log");
    let mut collector =
        LogCollector::new(fixture_config(log.clone(), dir.path().join("state")))
            .expect("collector");
    let now = Instant::now();
    let healthy = collector.collect_at(None, 1, now).await;
    collector.commit(&healthy).expect("commit healthy");
    std::fs::remove_file(&log).expect("remove first");
    let first = collector
        .collect_at(None, 2, now + Duration::from_secs(1))
        .await;
    collector.commit(&first).expect("commit first outage");
    std::fs::write(&log, "").expect("restore log");
    let recovery = collector
        .collect_at(None, 3, now + Duration::from_secs(2))
        .await;
    collector.commit(&recovery).expect("commit recovery");
    std::fs::remove_file(&log).expect("remove second");
    let second = collector
        .collect_at(None, 4, now + Duration::from_secs(3))
        .await;
    assert_eq!(
        second
            .gaps
            .iter()
            .filter(|gap| gap.reason == GapReason::MissingFile)
            .count(),
        1
    );
}

#[tokio::test]
async fn discovery_deadline_is_honored_before_any_source_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = LogConfig::disabled(dir.path());
    config.enabled = true;
    config.discovery_interval = Duration::from_secs(60);
    let mut collector = LogCollector::new(config).expect("collector");
    let now = Instant::now();

    let first = collector.collect_at(None, 1, now).await;
    assert_eq!(
        source_state(&first),
        (
            LogSourceState::Unavailable,
            LogSourceReason::PostgresUnavailable
        )
    );
    assert!(first.gaps.is_empty());
    collector.commit(&first).expect("commit first");
    let deadline = collector.next_discovery;

    let second = collector
        .collect_at(None, 2, now + Duration::from_secs(5))
        .await;
    assert_eq!(collector.next_discovery, deadline);
    assert!(second.source_status.is_none());
}

#[tokio::test]
async fn a_saved_readable_path_degrades_when_postgres_is_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("postgresql.log");
    std::fs::write(&log, "").expect("write log");
    let state_path = dir.path().join("state");
    let now = Instant::now();
    let mut first_process =
        LogCollector::new(fixture_config(log.clone(), state_path.clone()))
            .expect("first collector");
    let first = first_process.collect_at(None, 1, now).await;
    first_process.commit(&first).expect("persist valid tail state");
    drop(first_process);

    let mut config = fixture_config(log, state_path);
    config.path_override = None;
    let mut collector = LogCollector::new(config).expect("collector");

    let batch = collector
        .collect_at(None, 2, now + Duration::from_secs(1))
        .await;
    assert_eq!(
        source_state(&batch),
        (
            LogSourceState::CollectingDegraded,
            LogSourceReason::PostgresUnavailable
        )
    );
    assert!(batch.gaps.is_empty());
}

#[test]
fn discovery_outcomes_map_to_status_reasons() {
    assert_eq!(
        discovery_reason(DiscoveryStatus::NoCurrentLogfile),
        LogSourceReason::NoCurrentLogfile
    );
    assert_eq!(
        discovery_reason(DiscoveryStatus::UnsupportedFormat),
        LogSourceReason::UnsupportedFormat
    );
    assert_eq!(
        readable_status(DiscoveryStatus::QueryFailed),
        (
            LogSourceState::CollectingDegraded,
            LogSourceReason::DiscoveryQueryFailed
        )
    );
}

#[tokio::test]
async fn disabled_collection_emits_status_without_a_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut collector =
        LogCollector::new(LogConfig::disabled(dir.path())).expect("collector");
    let now = Instant::now();
    let first = collector.collect_at(None, 1, now).await;
    assert_eq!(
        source_state(&first),
        (LogSourceState::Disabled, LogSourceReason::None)
    );
    assert!(first.gaps.is_empty());
    collector.commit(&first).expect("commit disabled status");
    let second = collector
        .collect_at(None, 2, now + Duration::from_secs(1))
        .await;
    assert!(second.source_status.is_none());
    assert!(second.gaps.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn initial_permission_denial_is_status_without_a_gap() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("postgresql.log");
    std::fs::write(&log, "").expect("write log");
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000))
        .expect("remove read permission");
    if std::fs::File::open(&log).is_ok() {
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600))
            .expect("restore root-readable fixture");
        return;
    }
    let mut collector =
        LogCollector::new(fixture_config(log.clone(), dir.path().join("state")))
            .expect("collector");
    let batch = collector.collect_at(None, 1, Instant::now()).await;
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600))
        .expect("restore read permission");

    assert_eq!(
        source_state(&batch),
        (
            LogSourceState::Unavailable,
            LogSourceReason::PermissionDenied
        )
    );
    assert!(batch.gaps.is_empty());
}
```

Delete the old `disabled_collection_emits_explicit_gap_once` test after the new
disabled test covers the revised contract.

- [ ] **Step 2: Run the focused tests and confirm semantic failures**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-source-log collector::tests::quiet_read --target "$HOST"
cargo +1.96.0 test -p kronika-source-log collector::tests::a_continuous_missing --target "$HOST"
cargo +1.96.0 test -p kronika-source-log collector::tests::discovery_deadline --target "$HOST"
```

Expected: tests fail because current code emits repeated gaps, suppresses the
discovery deadline without a source, and has no status row.

- [ ] **Step 3: Expand discovery outcomes and cache failures**

Use this exhaustive discovery enum:

```rust
/// Result of the latest path-discovery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStatus {
    /// A supported current file was discovered.
    Available,
    /// This cycle had no PostgreSQL client for discovery.
    PostgresUnavailable,
    /// PostgreSQL reported no current stderr file.
    NoCurrentLogfile,
    /// `log_destination` does not expose a supported file.
    UnsupportedFormat,
    /// A discovery SQL query failed.
    QueryFailed,
    /// The source is explicitly disabled.
    Disabled,
}
```

Add `last_discovery: Option<DiscoveryStatus>` and `status_tracker:
StatusTracker` to `LogCollector`. Before moving `config` and `state` into the
struct, initialise:

```rust
let status_tracker = StatusTracker::new(config.status_interval, state.is_some());
```

Refactor discovery to `refresh_source_at(client, now)`. The deadline check must
not depend on `self.source`:

```rust
if let Some(path) = &self.config.path_override {
    self.source = Some(LogSource {
        path: path.clone(),
        parser_kind: self.config.parser_kind,
    });
    self.last_discovery = Some(DiscoveryStatus::Available);
    return DiscoveryStatus::Available;
}
if self.next_discovery.is_some_and(|deadline| now < deadline) {
    return self
        .last_discovery
        .unwrap_or(DiscoveryStatus::PostgresUnavailable);
}
self.next_discovery = Some(now + self.config.discovery_interval);
```

Map absence of a client to `PostgresUnavailable`. Map a failed SQL request to
`QueryFailed`. Map `pg_current_logfile('stderr') IS NULL` to
`NoCurrentLogfile`, and a `log_destination` without `stderr` to
`UnsupportedFormat`. Retain the last source only for
`PostgresUnavailable|QueryFailed`; clear it for
`NoCurrentLogfile|UnsupportedFormat`.

In the collector binary's temporary `discovery_status_name` match, remove
`SourceUnavailable` and add:

```rust
LogDiscoveryStatus::PostgresUnavailable => "postgres_unavailable",
LogDiscoveryStatus::NoCurrentLogfile => "no_current_logfile",
LogDiscoveryStatus::QueryFailed => "query_failed",
```

Keep the existing `Available`, `UnsupportedFormat` and `Disabled` arms. Task 5
removes this per-cycle adapter after transition logging is available; this
intermediate update keeps the Task 4 commit buildable.

- [ ] **Step 4: Stage status and outage state in `LogCollection`**

Add these public diagnostic fields and one private pending update:

```rust
/// Status row emitted by this observation, if first/change/heartbeat is due.
pub source_status: Option<LogSourceStatus>,
/// Previous observed status when this observation emitted a row.
pub previous_source_status: Option<LogSourceStatus>,
/// Whether the emitted row changes the status key.
pub source_status_changed: bool,
/// Time remaining before the next SQL discovery attempt.
pub next_discovery_in: Option<Duration>,
pending_status: Option<StatusUpdate>,
```

Keep `collect(client, ts)` as the public wall-clock wrapper:

```rust
pub async fn collect(&mut self, client: Option<&Client>, ts: i64) -> LogCollection {
    self.collect_at(client, ts, Instant::now()).await
}
```

Add a private helper that stages the status and optionally appends the single
outage gap:

```rust
fn stage_status(
    &self,
    collection: &mut LogCollection,
    status: LogSourceStatus,
    ts: i64,
    now: Instant,
) {
    let status_reason = status.reason;
    let update = self.status_tracker.observe(status, now);
    if update.outage_started {
        let reason = match status_reason {
            LogSourceReason::MissingFile => GapReason::MissingFile,
            LogSourceReason::PermissionDenied => GapReason::PermissionDenied,
            _ => GapReason::SourceUnavailable,
        };
        collection.gaps.push(self.simple_gap(ts, reason));
    }
    collection.source_status = update.row.clone();
    collection.previous_source_status = update.row.as_ref().and(update.previous.clone());
    collection.source_status_changed = update.row.is_some() && update.changed;
    collection.next_discovery_in = self
        .next_discovery
        .map(|deadline| deadline.saturating_duration_since(now));
    collection.pending_status = Some(update);
}
```

For an explicit path `next_discovery` remains `None`, which is represented as
zero only by the process-log adapter.

- [ ] **Step 5: Apply read-result precedence**

Restructure `collect_at` in this order:

1. `enabled=false`: do no discovery and no file access; stage
   `disabled/none`.
2. Refresh or reuse discovery outcome.
3. If no source exists, stage `unavailable` with the discovery reason and
   return without a gap.
4. If parser is not `Stderr`, stage `unavailable/unsupported_format`.
5. Read the known path.
6. A tail batch with `missing_files>0` is a failed read:
   `unavailable/missing_file`.
7. `PermissionDenied` becomes `unavailable/permission_denied`; every other
   I/O error becomes `unavailable/read_error`.
8. A successful read after `Available` becomes `collecting/none`.
9. A successful read after `PostgresUnavailable` becomes
   `collecting_degraded/postgres_unavailable`.
10. A successful read after `QueryFailed` becomes
    `collecting_degraded/discovery_query_failed`.

Use an exhaustive helper so read errors always override discovery errors:

```rust
const fn discovery_reason(status: DiscoveryStatus) -> LogSourceReason {
    match status {
        DiscoveryStatus::Available | DiscoveryStatus::Disabled => LogSourceReason::None,
        DiscoveryStatus::PostgresUnavailable => LogSourceReason::PostgresUnavailable,
        DiscoveryStatus::NoCurrentLogfile => LogSourceReason::NoCurrentLogfile,
        DiscoveryStatus::UnsupportedFormat => LogSourceReason::UnsupportedFormat,
        DiscoveryStatus::QueryFailed => LogSourceReason::DiscoveryQueryFailed,
    }
}

const fn readable_status(
    discovery: DiscoveryStatus,
) -> (LogSourceState, LogSourceReason) {
    match discovery {
        DiscoveryStatus::Available => {
            (LogSourceState::Collecting, LogSourceReason::None)
        }
        DiscoveryStatus::PostgresUnavailable => (
            LogSourceState::CollectingDegraded,
            LogSourceReason::PostgresUnavailable,
        ),
        DiscoveryStatus::QueryFailed => (
            LogSourceState::CollectingDegraded,
            LogSourceReason::DiscoveryQueryFailed,
        ),
        DiscoveryStatus::NoCurrentLogfile => (
            LogSourceState::Unavailable,
            LogSourceReason::NoCurrentLogfile,
        ),
        DiscoveryStatus::UnsupportedFormat => (
            LogSourceState::Unavailable,
            LogSourceReason::UnsupportedFormat,
        ),
        DiscoveryStatus::Disabled => {
            (LogSourceState::Disabled, LogSourceReason::None)
        }
    }
}

fn read_error_status(kind: io::ErrorKind) -> (LogSourceState, LogSourceReason) {
    let reason = match kind {
        io::ErrorKind::NotFound => LogSourceReason::MissingFile,
        io::ErrorKind::PermissionDenied => LogSourceReason::PermissionDenied,
        _ => LogSourceReason::ReadError,
    };
    (LogSourceState::Unavailable, reason)
}
```

Remove the `missing_files` branch from `gaps_from_tail`; outage tracking now
owns that gap. Preserve backlog, truncate, invalid UTF-8, binary, sparse,
rotation, dictionary-full, budget, parser-drop and timestamp-fallback branches.

- [ ] **Step 6: Commit staged tracker state only after output handling**

Remove the existing early return when `next_state` is absent. Persist tail
state first, then confirm the tracker update:

```rust
pub fn commit(&mut self, collection: &LogCollection) -> io::Result<()> {
    if let Some(state) = &collection.next_state {
        state.save(&self.config.state_path)?;
        self.state = Some(state.clone());
        self.source = Some(LogSource {
            path: state.path.clone(),
            parser_kind: state.parser_kind,
        });
    }
    if let Some(update) = &collection.pending_status {
        self.status_tracker.commit(update);
    }
    Ok(())
}
```

If state-file persistence fails, the function returns before committing the
status tracker. If no tail state exists, disabled and unavailable status still
commit, so the next 5-second cycle does not repeat the row before heartbeat.

- [ ] **Step 7: Run the entire source-log suite**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-source-log --target "$HOST"
cargo +1.96.0 check -p pg-kronika-collector --target "$HOST"
```

Expected: all parser/tailer tests retain their old results; new status tests
pass; the disabled test no longer expects `GapReason::Disabled`.

- [ ] **Step 8: Commit the collector state machine**

```sh
git add crates/kronika-source-log/src/collector.rs bins/pg_kronika-collector/src/pg_log_source.rs
git commit -m "feat(source-log): report log source availability"
```

### Task 5: Encode Status and Log Only Transitions or Heartbeats

**Files:**
- Modify: `bins/pg_kronika-collector/src/pg_log_source.rs`
- Modify: `bins/pg_kronika-collector/src/tests/buffering.rs`
- Test: `bins/pg_kronika-collector/src/tests/buffering.rs`

**Interfaces:**
- Consumes: `LogCollection::source_status`, `PgLogSourceStatusV1`, `PG_LOG_SOURCE_STATUS_TYPE_ID`.
- Produces: one buffered status row when `source_status=Some`, plus `info` transition and `debug` heartbeat process logs.

- [ ] **Step 1: Write failing buffering and log-level tests**

Add the required imports and these tests to `tests/buffering.rs`:

```rust
use crate::logging::LogLevel;
use crate::pg_log_source::{
    push_log_collection, push_log_source_status, source_status_log_level,
};
use kronika_format::DictLimits;
use kronika_source_log::{
    LogCollection, LogCollector, LogConfig, LogSourceReason, LogSourceState,
    LogSourceStatus, ParserKind,
};
use std::path::PathBuf;

#[test]
fn push_log_collection_buffers_source_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let collector =
        LogCollector::new(LogConfig::disabled(dir.path())).expect("log collector");
    let mut collection = LogCollection::default();
    collection.source_status = Some(LogSourceStatus {
        ts: 1_000,
        state: LogSourceState::Collecting,
        reason: LogSourceReason::None,
        parser_kind: ParserKind::Stderr,
        source_path: Some(PathBuf::from("/pg/log/postgresql.log")),
    });
    collection.source_status_changed = true;
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());

    push_log_collection(
        &mut buffers,
        &mut interner,
        &collector,
        &mut collection,
        1_000,
    )
    .expect("buffer status");
    let dictionaries = dict::encode(interner.window()).expect("encode dictionary");
    let part = buffers
        .flush(&dictionaries, 7)
        .expect("flush status")
        .expect("status creates a part");
    let catalog = kronika_format::validate_part(&part).expect("valid PGM");
    assert!(
        catalog.entries.iter().any(|entry| {
            entry.type_id == 1_039_001 && entry.rows == 1
        })
    );
}

#[test]
fn source_status_survives_a_full_dictionary_without_its_path() {
    let limits = DictLimits::new(1, 1)
        .expect("minimal valid limits")
        .with_max_total_bytes(1)
        .expect("one-byte dictionary");
    let mut interner = Interner::new(limits);
    interner.intern(b"x").expect("fill dictionary");
    let mut buffers = SectionBuffers::new();
    let dropped = push_log_source_status(
        &mut buffers,
        &mut interner,
        &LogSourceStatus {
            ts: 1_000,
            state: LogSourceState::Unavailable,
            reason: LogSourceReason::PermissionDenied,
            parser_kind: ParserKind::Stderr,
            source_path: Some(PathBuf::from("/pg/log/postgresql.log")),
        },
    )
    .expect("buffer status without path");
    assert_eq!(dropped, 1);
    let dictionaries = dict::encode(interner.window()).expect("encode dictionary");
    let part = buffers
        .flush(&dictionaries, 7)
        .expect("flush status")
        .expect("status row remains");
    let catalog = kronika_format::validate_part(&part).expect("valid PGM");
    assert!(catalog.entries.iter().any(|entry| {
        entry.type_id == 1_039_001 && entry.rows == 1
    }));
}

#[test]
fn status_process_log_level_distinguishes_transition_and_heartbeat() {
    let mut collection = LogCollection::default();
    assert_eq!(source_status_log_level(&collection), None);
    collection.source_status = Some(LogSourceStatus {
        ts: 1,
        state: LogSourceState::Collecting,
        reason: LogSourceReason::None,
        parser_kind: ParserKind::Stderr,
        source_path: None,
    });
    collection.source_status_changed = true;
    assert_eq!(source_status_log_level(&collection), Some(LogLevel::Info));
    collection.source_status_changed = false;
    assert_eq!(source_status_log_level(&collection), Some(LogLevel::Debug));
}
```

- [ ] **Step 2: Run the focused tests and confirm the red state**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p pg-kronika-collector push_log_collection_buffers_source_status --target "$HOST"
cargo +1.96.0 test -p pg-kronika-collector status_process_log_level --target "$HOST"
```

Expected: the first test finds no `1_039_001` entry and the second cannot import
`source_status_log_level`.

- [ ] **Step 3: Buffer a status row without dropping it on dictionary failure**

Import `PgLogSourceStatusV1` and add:

```rust
pub(crate) fn push_log_source_status(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    status: &LogSourceStatus,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let source_path = status.source_path.as_ref().and_then(|path| {
        let value = path.to_string_lossy();
        intern_log_text(interner, &value, MAX_TEXT_BYTES, &mut dropped)
    });
    buffer_row(
        buffers,
        PgLogSourceStatusV1 {
            ts: Ts(status.ts),
            state: status.state.code(),
            reason: status.reason.code(),
            parser_kind: status.parser_kind.code(),
            source_path,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}
```

Call it at the start of `push_log_sections` when `collection.source_status` is
present. Include its dropped field count in the existing dictionary-full
accounting; the status row remains buffered with `source_path=NULL`.

- [ ] **Step 4: Replace per-cycle discovery logging**

Add:

```rust
pub(crate) const fn source_status_log_level(
    collection: &LogCollection,
) -> Option<LogLevel> {
    if collection.source_status.is_none() {
        None
    } else if collection.source_status_changed {
        Some(LogLevel::Info)
    } else {
        Some(LogLevel::Debug)
    }
}
```

In `collect_log_batch`, remove the old unconditional
`discovery_status_name` debug event. When a status row is emitted, write one
`pg_log_discovery` event with:

```rust
let previous = collection
    .previous_source_status
    .as_ref()
    .map_or("unknown", |status| status.state.as_str());
let next_discovery_ms = collection
    .next_discovery_in
    .map(duration_ms)
    .unwrap_or(0);
```

The event fields are `previous_state`, `state`, `reason`, `parser`,
`source_path`, `next_discovery_ms`, all row counts and `elapsed_ms`. Use
`LogLevel::Info` for a changed key and `LogLevel::Debug` for a heartbeat.
Represent a missing path as an empty string. Do not include PostgreSQL message
content, parsed SQL, detail or sample fields.

Also call `log_collection_finish` for `PG_LOG_SOURCE_STATUS_TYPE_ID` with one
row whenever `source_status.is_some()`.

- [ ] **Step 5: Run collector tests**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p pg-kronika-collector buffering --target "$HOST"
cargo +1.96.0 test -p pg-kronika-collector --target "$HOST"
```

Expected: status creates a PGM part even with no log events; existing event and
gap buffering tests pass.

- [ ] **Step 6: Commit the writer integration**

```sh
git add bins/pg_kronika-collector/src/pg_log_source.rs bins/pg_kronika-collector/src/tests/buffering.rs
git commit -m "feat(collector): write PostgreSQL log source status"
```

### Task 6: Add a Bounded Latest-Row Reader Query

**Files:**
- Create: `crates/kronika-reader/src/query/latest.rs`
- Modify: `crates/kronika-reader/src/query/section.rs:14-17,535-565`
- Modify: `crates/kronika-reader/src/query/mod.rs`
- Modify: `crates/kronika-reader/src/lib.rs`
- Test: `crates/kronika-reader/src/query/latest.rs`

**Interfaces:**
- Consumes: `LocalDirSnapshot::{units,unit_catalog,open_unit,refresh}`, `logical_section`, `cell_to_value`.
- Produces: `pub fn latest_section_row(&mut LocalDirSnapshot, &str, u64) -> Result<Option<OutRow>, QueryError>`.

- [ ] **Step 1: Write failing latest-row tests**

In the new module, build real PGM parts with `PgLogSourceStatusV1`:

```rust
#[cfg(test)]
mod tests {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::pg_log::PgLogSourceStatusV1;
    use kronika_registry::Ts;

    use super::latest_section_row;
    use crate::query::Value;
    use crate::snapshot::OPEN_UNIT_CALLS;
    use crate::LocalDirSnapshot;

    fn write_status(
        dir: &std::path::Path,
        file: &str,
        source: u64,
        min_ts: i64,
        max_ts: i64,
        row_ts: i64,
        state: u8,
    ) {
        let body = PgLogSourceStatusV1::encode(&[PgLogSourceStatusV1 {
            ts: Ts(row_ts),
            state,
            reason: 0,
            parser_kind: 0,
            source_path: None,
            dict_dropped_fields: 0,
        }])
        .expect("encode status");
        let part = build_part(
            &[SectionInput {
                type_id: 1_039_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        );
        std::fs::write(dir.join(file), part).expect("write PGM");
    }

    fn field<'a>(row: &'a crate::OutRow, name: &str) -> &'a Value {
        row.iter()
            .find(|(column, _)| column == name)
            .map(|(_, value)| value)
            .expect("field")
    }

    #[test]
    fn latest_row_uses_row_timestamp_across_overlapping_units() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), "wide.pgm", 7, 0, 1_000, 100, 2);
        write_status(dir.path(), "older-max.pgm", 7, 0, 900, 800, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");

        let row = latest_section_row(&mut snapshot, "pg_log_source_status", 7)
            .expect("latest query")
            .expect("status row");
        assert_eq!(field(&row, "ts"), &Value::Ts(800));
        assert_eq!(field(&row, "state"), &Value::U64(0));
    }

    #[test]
    fn latest_row_stops_before_provably_older_units() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), "new.pgm", 7, 200, 300, 250, 0);
        write_status(dir.path(), "old.pgm", 7, 100, 200, 190, 2);
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");

        let row = latest_section_row(&mut snapshot, "pg_log_source_status", 7)
            .expect("latest query");
        assert!(row.is_some());
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn latest_row_returns_none_for_an_old_store_without_the_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), "other-source.pgm", 42, 0, 10, 5, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        assert_eq!(
            latest_section_row(&mut snapshot, "pg_log_source_status", 7)
                .expect("latest query"),
            None
        );
    }

    #[test]
    fn latest_row_rejects_an_unregistered_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        let error = latest_section_row(&mut snapshot, "not_registered", 7)
            .expect_err("unknown section");
        assert!(matches!(error, crate::QueryError::UnknownSection(name) if name == "not_registered"));
    }
}
```

- [ ] **Step 2: Run the focused tests and confirm the red state**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-reader latest::tests --target "$HOST"
```

Expected: compilation fails because `query::latest` and
`latest_section_row` do not exist.

- [ ] **Step 3: Implement catalog-first reverse search**

Make `MAX_REFRESH` and `compare_full` in `section.rs` visible to sibling query
modules with `pub(super)`. Implement this algorithm in `latest.rs`:

```rust
use super::logical::{LogicalSection, logical_section};
use super::section::{MAX_REFRESH, QueryError, compare_full};
use super::value::{OutRow, Value, cell_to_value};
use crate::{Cell, LocalDirSnapshot, ReadError, UnitMeta};

pub fn latest_section_row(
    snapshot: &mut LocalDirSnapshot,
    name: &str,
    source: u64,
) -> Result<Option<OutRow>, QueryError> {
    let logical =
        logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
    let mut refreshed = 0_u32;
    loop {
        let skip_stale = refreshed >= MAX_REFRESH;
        match latest_once(snapshot, &logical, source, skip_stale) {
            Ok(row) => return Ok(row),
            Err(LatestError::Stale) => {
                snapshot
                    .refresh()
                    .map_err(|error| QueryError::Read(ReadError::Io(error)))?;
                refreshed += 1;
            }
            Err(LatestError::Read(error)) => return Err(QueryError::Read(error)),
        }
    }
}
```

Define the bounded inner pass as follows:

```rust
enum LatestError {
    Stale,
    Read(ReadError),
}

fn latest_once(
    snapshot: &LocalDirSnapshot,
    logical: &LogicalSection,
    source: u64,
    skip_stale: bool,
) -> Result<Option<OutRow>, LatestError> {
    let units = snapshot.units();
    let mut candidates: Vec<(usize, UnitMeta)> = units
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, unit)| {
            unit.source_id == source
                && snapshot.unit_catalog(*index).is_some_and(|catalog| {
                    catalog.entries.iter().any(|entry| {
                        entry.rows != 0 && logical.type_ids.contains(&entry.type_id)
                    })
                })
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .1
            .max_ts
            .cmp(&left.1.max_ts)
            .then_with(|| right.1.live.cmp(&left.1.live))
            .then_with(|| right.0.cmp(&left.0))
    });

    let union_columns: Vec<&str> = logical
        .columns
        .iter()
        .map(|column| column.name)
        .collect();
    let mut best: Option<(i64, OutRow)> = None;
    for (index, unit_meta) in candidates {
        if best
            .as_ref()
            .is_some_and(|(best_ts, _)| unit_meta.max_ts < *best_ts)
        {
            break;
        }
        let unit = match snapshot.open_unit(index) {
            Ok(unit) => unit,
            Err(ReadError::StaleSnapshot { .. }) if skip_stale => continue,
            Err(ReadError::StaleSnapshot { .. }) => return Err(LatestError::Stale),
            Err(error) => return Err(LatestError::Read(error)),
        };
        let dictionary = unit.dictionary().map_err(LatestError::Read)?;
        for entry in &unit.catalog().entries {
            if entry.rows == 0 || !logical.type_ids.contains(&entry.type_id) {
                continue;
            }
            let rows = unit.decode_rows(entry).map_err(LatestError::Read)?;
            let Some(first) = rows.first() else {
                continue;
            };
            let contract_columns = first.contract().columns;
            let ts_at = contract_columns
                .iter()
                .position(|column| column.name == "ts");
            let cell_at: Vec<Option<usize>> = logical
                .columns
                .iter()
                .map(|column| {
                    contract_columns
                        .iter()
                        .position(|candidate| candidate.name == column.name)
                })
                .collect();
            for row in rows {
                let cells = row.cells();
                let Some(&Cell::Ts(ts)) = ts_at.and_then(|at| cells.get(at)) else {
                    continue;
                };
                let output: OutRow = logical
                    .columns
                    .iter()
                    .zip(&cell_at)
                    .map(|(column, at)| {
                        let value = at
                            .and_then(|at| cells.get(at))
                            .map_or(Value::Null, |cell| {
                                cell_to_value(cell, &dictionary).0
                            });
                        (column.name.to_owned(), value)
                    })
                    .collect();
                let replace = best.as_ref().is_none_or(|(best_ts, best_row)| {
                    ts > *best_ts
                        || (ts == *best_ts
                            && compare_full(
                                &output,
                                best_row,
                                &union_columns,
                                logical.sort_key,
                            ) == std::cmp::Ordering::Greater)
                });
                if replace {
                    best = Some((ts, output));
                }
            }
        }
    }
    Ok(best.map(|(_, row)| row))
}
```

This opens only catalog candidates and stops once every remaining unit has
`max_ts` below the best decoded row timestamp. A store without the status type
returns `None` after catalog inspection and opens no section bodies.

- [ ] **Step 4: Export the query and run reader tests**

Add `mod latest` plus a `pub use` in `query/mod.rs`; re-export
`latest_section_row` from `crates/kronika-reader/src/lib.rs`.

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-reader latest::tests --target "$HOST"
cargo +1.96.0 test -p kronika-reader query::section --target "$HOST"
```

Expected: latest-row tests pass; batch section query still opens every selected
unit only once.

- [ ] **Step 5: Commit the bounded reader query**

```sh
git add crates/kronika-reader/src/query/latest.rs crates/kronika-reader/src/query/section.rs crates/kronika-reader/src/query/mod.rs crates/kronika-reader/src/lib.rs
git commit -m "feat(reader): read the latest logical section row"
```

### Task 7: Expose the Latest Status in `/v1/sources`

**Files:**
- Modify: `bins/pg_kronika-web/src/handlers/v1.rs:1-90`
- Modify: `bins/pg_kronika-web/src/tests/anomalies.rs:1-24`
- Modify: `bins/pg_kronika-web/openapi.json:91-112,644-665`
- Test: `bins/pg_kronika-web/src/tests/anomalies.rs`

**Interfaces:**
- Consumes: `latest_section_row`, `OutRow`, reader `Value`.
- Produces: required per-source JSON property `pg_log` with
  `{state,reason,observed_at,parser,source_path}`.

- [ ] **Step 1: Update the old-store test first**

Change the existing `sources_fold_each_source_into_one_span` expected body so
both sources retain their spans and receive:

```json
"pg_log": {
  "state": "unknown",
  "reason": "no_status",
  "observed_at": null,
  "parser": null,
  "source_path": null
}
```

Add the fixture helper and the latest-status test:

```rust
use kronika_registry::pg_log::PgLogSourceStatusV1;

fn write_status_segment(
    dir: &std::path::Path,
    file: &str,
    source: u64,
    ts: i64,
    state: u8,
    reason: u8,
    parser_kind: u8,
) {
    let body = PgLogSourceStatusV1::encode(&[PgLogSourceStatusV1 {
        ts: Ts(ts),
        state,
        reason,
        parser_kind,
        source_path: None,
        dict_dropped_fields: 0,
    }])
    .expect("encode status");
    let part = build_part(
        &[SectionInput {
            type_id: 1_039_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: ts,
            max_ts: ts,
            source_id: source,
        },
    );
    std::fs::write(dir.join(file), part).expect("write status segment");
}

#[tokio::test]
async fn sources_returns_the_latest_pg_log_status_per_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_status_segment(dir.path(), "first.pgm", 7, 1_000, 0, 0, 0);
    write_status_segment(dir.path(), "second.pgm", 7, 2_000, 2, 6, 0);

    let (status, body) = serve(dir.path(), "/v1/sources").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["sources"][0]["pg_log"],
        serde_json::json!({
            "state": "unavailable",
            "reason": "permission_denied",
            "observed_at": 2_000,
            "parser": "stderr",
            "source_path": null
        })
    );
}

#[tokio::test]
async fn sections_catalog_exposes_pg_log_source_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);
    let (status, body) = serve(dir.path(), "/v1/sections").await;
    assert_eq!(status, StatusCode::OK);
    let source_status = body["sections"]
        .as_array()
        .expect("sections array")
        .iter()
        .find(|section| section["name"] == "pg_log_source_status")
        .expect("registered status section");
    assert_eq!(source_status["semantics"], "on_change");
    assert_eq!(source_status["sort_key"], serde_json::json!(["ts"]));
}
```

- [ ] **Step 2: Run the focused web tests and confirm the red state**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p pg-kronika-web sources_ --target "$HOST"
cargo +1.96.0 test -p pg-kronika-web sections_catalog_exposes_pg_log_source_status --target "$HOST"
```

Expected: the old-store expected body differs and the new fixture has no
`pg_log` field in the response.

- [ ] **Step 3: Add strict PGM-code to API-name mapping**

Alias the reader value type and add private helpers in `handlers/v1.rs`:

```rust
fn unknown_pg_log_status() -> Value {
    json!({
        "state": "unknown",
        "reason": "no_status",
        "observed_at": null,
        "parser": null,
        "source_path": null,
    })
}

fn reader_field<'a>(
    row: &'a kronika_reader::OutRow,
    name: &str,
) -> Option<&'a kronika_reader::Value> {
    row.iter()
        .find(|(column, _)| column == name)
        .map(|(_, value)| value)
}

fn pg_log_status_json(row: Option<&kronika_reader::OutRow>) -> Value {
    let Some(row) = row else {
        return unknown_pg_log_status();
    };
    let Some(kronika_reader::Value::Ts(observed_at)) = reader_field(row, "ts") else {
        return unknown_pg_log_status();
    };
    let Some(kronika_reader::Value::U64(state)) = reader_field(row, "state") else {
        return unknown_pg_log_status();
    };
    let Some(kronika_reader::Value::U64(reason)) = reader_field(row, "reason") else {
        return unknown_pg_log_status();
    };
    let Some(kronika_reader::Value::U64(parser)) = reader_field(row, "parser_kind") else {
        return unknown_pg_log_status();
    };
    let state = match state {
        0 => "collecting",
        1 => "collecting_degraded",
        2 => "unavailable",
        3 => "disabled",
        _ => return unknown_pg_log_status(),
    };
    let reason = match reason {
        0 => "none",
        1 => "postgres_unavailable",
        2 => "no_current_logfile",
        3 => "unsupported_format",
        4 => "discovery_query_failed",
        5 => "missing_file",
        6 => "permission_denied",
        7 => "read_error",
        _ => return unknown_pg_log_status(),
    };
    let parser = match parser {
        0 => "stderr",
        1 => "csvlog",
        2 => "unknown",
        _ => return unknown_pg_log_status(),
    };
    let source_path = match reader_field(row, "source_path") {
        Some(kronika_reader::Value::Str(path)) => Value::String(path.clone()),
        Some(kronika_reader::Value::Blob { text, .. }) => {
            Value::String(text.clone())
        }
        Some(kronika_reader::Value::Null) | None => Value::Null,
        _ => return unknown_pg_log_status(),
    };
    json!({
        "state": state,
        "reason": reason,
        "observed_at": observed_at,
        "parser": parser,
        "source_path": source_path,
    })
}
```

The fallback for an invalid stored row is intentionally the same conservative
`unknown/no_status` shape; it never asserts that collection is healthy.

- [ ] **Step 4: Query one latest row per catalogued source**

Clone the published snapshot once because the reader may refresh on a stale
active part:

```rust
let published = state.snapshot();
let mut snapshot = published.as_ref().clone();
```

Build spans from `snapshot.units()` as today. Replace the iterator-only JSON
map with a loop so each source can call:

```rust
let status = latest_section_row(
    &mut snapshot,
    "pg_log_source_status",
    source_id,
)
.map_err(|error| query_error_response_without_cursor(&error))?;
let pg_log = pg_log_status_json(status.as_ref());
```

Return the unchanged `source_id`, `min_ts`, `max_ts`, `segments` fields plus
`pg_log`. Do not derive staleness from wall-clock time.

- [ ] **Step 5: Make the OpenAPI change additive**

Require `pg_log` on each item in `Sources` and add a referenced object schema:

```json
"PgLogSourceStatus": {
  "type": "object",
  "additionalProperties": false,
  "required": ["state", "reason", "observed_at", "parser", "source_path"],
  "properties": {
    "state": {
      "type": "string",
      "enum": ["collecting", "collecting_degraded", "unavailable", "disabled", "unknown"]
    },
    "reason": {
      "type": "string",
      "enum": ["none", "postgres_unavailable", "no_current_logfile", "unsupported_format", "discovery_query_failed", "missing_file", "permission_denied", "read_error", "no_status"]
    },
    "observed_at": { "type": ["integer", "null"], "format": "int64" },
    "parser": {
      "type": ["string", "null"],
      "enum": ["stderr", "csvlog", "unknown", null]
    },
    "source_path": { "type": ["string", "null"] }
  }
}
```

Set the per-source property to
`{ "$ref": "#/components/schemas/PgLogSourceStatus" }`.

- [ ] **Step 6: Run web and OpenAPI tests**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p pg-kronika-web sources_ --target "$HOST"
cargo +1.96.0 test -p pg-kronika-web sections_catalog_exposes_pg_log_source_status --target "$HOST"
cargo +1.96.0 test -p pg-kronika-web openapi --target "$HOST"
```

Expected: old PGM fixtures return `unknown/no_status`; the latest stored row
wins; OpenAPI remains valid JSON and lists the new required object.

- [ ] **Step 7: Commit the HTTP surface**

```sh
git add bins/pg_kronika-web/src/handlers/v1.rs bins/pg_kronika-web/src/tests/anomalies.rs bins/pg_kronika-web/openapi.json
git commit -m "feat(web): expose PostgreSQL log source status"
```

### Task 8: Prove Live Discovery, Quiet Reads and Rotation in BDD

**Files:**
- Modify: `crates/kronika-bdd/src/cluster.rs:272-284`
- Modify: `crates/kronika-bdd/src/harness/mod.rs:585-620`
- Modify: `crates/kronika-bdd/src/harness/snapshot.rs`
- Modify: `crates/kronika-bdd/src/harness/web.rs`
- Modify: `crates/kronika-bdd/src/steps/log.rs`
- Modify: `crates/kronika-bdd/src/steps/web.rs`
- Modify: `crates/kronika-bdd/features/pg_log.feature`
- Test: `crates/kronika-bdd/features/pg_log.feature`

**Interfaces:**
- Consumes: live `pg_current_logfile('stderr')`, collector timer and forced seal, in-process web router.
- Produces: BDD evidence for default-on discovery, empty successful read, `/v1/sources`, and path change after rotation.

- [ ] **Step 1: Add failing feature scenarios**

Add near the start of `pg_log.feature`:

```gherkin
  @pg16 @serial
  Scenario: default collection discovers a quiet PostgreSQL stderr file
    Given a fresh database on PostgreSQL 16
    When the collector snapshots the segment
    Then section pg_log_source_status has a row with state = 0:
      | reason              | 0 |
      | parser_kind         | 0 |
      | dict_dropped_fields | 0 |
    And section pg_log_errors is absent from the segment
    And the web API reports PostgreSQL log state collecting for the only source

  @pg16 @serial
  Scenario: collection follows a PostgreSQL stderr log rotation
    Given a fresh database on PostgreSQL 16
    When the running collector observes a PostgreSQL stderr log rotation
    Then section pg_log_source_status has 2 rows
    And pg_log_source_status contains two distinct source_path values
```

Neither scenario may set `KRONIKA_PG_LOG_ENABLED` or `KRONIKA_LOG_PATH`.

- [ ] **Step 2: Run the feature and confirm the red state**

Run:

```sh
TAGS='@pg_log and @pg16' make test-bdd
```

Expected: the cluster has no discoverable current file and the two new step
phrases are undefined.

- [ ] **Step 3: Configure the throwaway PostgreSQL clusters for file logging**

Append these exact GUCs after `initdb` and before PostgreSQL starts:

```text
track_io_timing = on
logging_collector = on
log_destination = 'stderr'
log_directory = 'log'
log_filename = 'postgresql-%Y%m%d%H%M%S.log'
log_rotation_age = 0
log_truncate_on_rotation = off
```

Keep `server.log` as the pre-logging-collector startup diagnostic. The BDD
process owns the temporary data directory, so the collector can read files
created with PostgreSQL's default `0600` mode.

Remove the redundant
`KRONIKA_PG_LOG_ENABLED=1` insertion from `HarnessState::write_log_fixture`;
keep explicit `KRONIKA_LOG_FORMAT=stderr`,
`KRONIKA_LOG_START_AT_BEGINNING=1`, state path and fixture path.

- [ ] **Step 4: Add one long-running collector rotation helper**

Add `snapshot::take_across_log_rotation`. It must:

1. Spawn one collector with scenario env plus
   `KRONIKA_INTERVAL_S=1`,
   `KRONIKA_PG_LOG_INTERVAL_S=1`,
   `KRONIKA_LOG_DISCOVERY_INTERVAL_S=1`,
   `KRONIKA_PG_LOG_STATUS_INTERVAL_S=300` and
   `KRONIKA_SEGMENT_MAX_AGE_S=900`.
2. Wait 1500 ms so a normal timer cycle records the first path without sealing.
3. Query `pg_current_logfile('stderr')` as `before`.
4. Wait until the next wall-clock second, execute `SELECT pg_rotate_logfile()`,
   and poll for at most 10 seconds until `pg_current_logfile('stderr') != before`.
5. Wait 1500 ms so discovery and a quiet read observe the new path.
6. Call the existing forced `collector.snapshot()` once; this seals both
   on-change rows in one PGM.
7. Preserve stderr and the output `TempDir` and set the resulting segment on
   `HarnessState`, exactly as `snapshot::take` does.

Use this polling core:

```rust
let after = tokio::time::timeout(Duration::from_secs(10), async {
    loop {
        let row = connection
            .client()
            .query_one(
                "SELECT pg_current_logfile('stderr')",
                &[],
            )
            .await?;
        let current: Option<String> = row.get(0);
        if current.as_deref().is_some_and(|path| path != before) {
            return Ok::<String, tokio_postgres::Error>(
                current.expect("checked as Some"),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
})
.await
.context("PostgreSQL did not rotate stderr within 10 seconds")??;
anyhow::ensure!(after != before, "stderr path did not change");
```

The helper returns an error if `before` is `NULL`, `pg_rotate_logfile()` returns
`false`, or the path does not change.

- [ ] **Step 5: Implement the BDD assertions through production readers**

Add these production-router helpers to `harness/web.rs`:

```rust
pub(crate) async fn only_pg_log_status(dir: &Path) -> Result<Value> {
    let response = request(dir, "/v1/sources", &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "/v1/sources returned status {}: {}",
        response.status,
        response.body
    );
    let sources = response.body["sources"]
        .as_array()
        .context("`sources` is not an array")?;
    let [source] = sources.as_slice() else {
        bail!("expected exactly one source, got {}", sources.len());
    };
    source
        .get("pg_log")
        .cloned()
        .context("the source has no `pg_log` object")
}

pub(crate) async fn assert_two_log_source_paths(dir: &Path) -> Result<()> {
    let source = only_source(dir).await?;
    let page = section_page(dir, "pg_log_source_status", source).await?;
    let rows = page["rows"].as_array().context("`rows` is not an array")?;
    let paths: BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row["source_path"].as_str())
        .collect();
    anyhow::ensure!(
        paths.len() == 2,
        "expected two distinct source paths, got {paths:?}"
    );
    Ok(())
}
```

Bind the rotation phrases in `steps/log.rs`:

```rust
use anyhow::{Context, Result};
use cucumber::{given, then, when};

#[when("the running collector observes a PostgreSQL stderr log rotation")]
async fn observe_stderr_rotation(world: &mut BddWorld) -> Result<()> {
    crate::harness::snapshot::take_across_log_rotation(&mut world.harness).await?;
    Ok(())
}

#[then("pg_log_source_status contains two distinct source_path values")]
async fn distinct_status_paths(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    crate::harness::web::assert_two_log_source_paths(dir).await
}
```

Bind the default-discovery assertion in `steps/web.rs`:

```rust
#[then("the web API reports PostgreSQL log state collecting for the only source")]
async fn web_pg_log_collecting(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    let status = web::only_pg_log_status(dir).await?;
    anyhow::ensure!(status["state"] == "collecting");
    anyhow::ensure!(status["reason"] == "none");
    anyhow::ensure!(status["parser"] == "stderr");
    anyhow::ensure!(status["observed_at"].as_i64().is_some());
    anyhow::ensure!(status["source_path"].as_str().is_some());
    Ok(())
}
```

The two-path assertion proves the second row came from a path transition rather
than the 300-second heartbeat.

- [ ] **Step 6: Run the log feature**

Run:

```sh
TAGS='@pg_log and @pg16' make test-bdd
```

Expected: explicit fixture scenarios still pass; default discovery produces
`collecting` with no event; rotation produces two distinct paths in one
segment; `/v1/sources` reports the latest one.

- [ ] **Step 7: Commit the live integration coverage**

```sh
git add crates/kronika-bdd/src/cluster.rs crates/kronika-bdd/src/harness/mod.rs crates/kronika-bdd/src/harness/snapshot.rs crates/kronika-bdd/src/harness/web.rs crates/kronika-bdd/src/steps/log.rs crates/kronika-bdd/src/steps/web.rs crates/kronika-bdd/features/pg_log.feature
git commit -m "test(bdd): cover PostgreSQL log discovery and rotation"
```

### Task 9: Document the Operator Contract and Verify the Workspace

**Files:**
- Modify: `bins/pg_kronika-collector/README.md`
- Modify: `bins/pg_kronika-collector/README.ru.md`
- Modify: `crates/kronika-registry/README.md`
- Modify: `crates/kronika-registry/README.ru.md`
- Modify: `docs/type-registry.md`
- Modify: `docs/type-registry/postgresql.md`

**Interfaces:**
- Consumes: implemented env defaults, exact state/reason codes and API shape.
- Produces: English/Russian operator reference that distinguishes quiet, degraded, unavailable and disabled collection.

- [ ] **Step 1: Rewrite the collector source section from observed behavior**

Rename “Optional PostgreSQL log source” and
“Необязательный источник журналов PostgreSQL” so they describe a default
source. In both languages, put this sequence before the variable table:

1. The collector tries `log_destination` and
   `pg_current_logfile('stderr')` by default.
2. A readable file is `collecting` even when it has no new lines.
3. A last known readable path plus failed discovery is
   `collecting_degraded`.
4. Missing path, unsupported format, missing file, permission denial or read
   error is `unavailable` with a concrete reason.
5. `KRONIKA_PG_LOG_ENABLED=0` is the explicit opt-out.
6. The collector never changes PostgreSQL settings or file permissions.
7. The first read begins at EOF unless
   `KRONIKA_LOG_START_AT_BEGINNING=1`.

Update the table rows exactly:

| Variable | Default | Meaning |
| --- | --- | --- |
| `KRONIKA_PG_LOG_ENABLED` | `true` | Attempt supported file-log discovery and reading; explicit false disables it. |
| `KRONIKA_PG_LOG_INTERVAL_S` | `5` | Attempt to read the known file. |
| `KRONIKA_LOG_DISCOVERY_INTERVAL_S` | `60` | Re-run PostgreSQL path discovery, including while no source exists. |
| `KRONIKA_PG_LOG_STATUS_INTERVAL_S` | `300` | Emit an unchanged status heartbeat; must be greater than zero. |
| `KRONIKA_LOG_PATH` | unset | Override the discovered path; does not override explicit disable. |

Keep the existing tail caps and parser limitations. Do not add host-target test
commands to either product README.

- [ ] **Step 2: Document the registry contract and compatibility**

In `docs/type-registry.md`, extend the PostgreSQL range to
`1_001_001–1_039_001`.

In `docs/type-registry/postgresql.md`:

- add `1_039_001` to the summary table as
  “состояние источника журнала PostgreSQL”, semantics `on_change`;
- add the six-column layout with classes `T/L/L/L/L/G`;
- list every numeric state, reason and parser code;
- explain first/change/300-second heartbeat emission;
- explain that a quiet read is `collecting`;
- explain that `collecting_degraded` means successful reading with stale
  discovery, not proven loss;
- explain initial unavailability versus the one-gap-after-success rule;
- state that old `pg_log_gap` codes remain readable;
- state that status does not participate in anomaly or health scoring.

Add concise matching rows to both registry README files. Use
“записывает” or “завершает часть” in Russian prose; do not use
“запечатывает”.

- [ ] **Step 3: Review English/Russian parity and factual wording**

Perform four passes:

1. Facts: every default and code matches source constants and registry fields.
2. Information architecture: default behavior and opt-out appear before edge
   cases.
3. Terminology: use one Russian term “состояние источника”; retain `status`,
   `state`, `reason`, env names and JSON names when referring to interfaces.
4. Markup: tables render, relative links resolve, headings are unique, no PR
   number or mutable branch-history reference is introduced.

- [ ] **Step 4: Run formatting and focused workspace tests**

Run:

```sh
cargo +1.96.0 fmt --all
cargo +1.96.0 fmt --all -- --check
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-registry --target "$HOST"
cargo +1.96.0 test -p kronika-source-log --target "$HOST"
cargo +1.96.0 test -p pg-kronika-collector --target "$HOST"
cargo +1.96.0 test -p kronika-reader --target "$HOST"
cargo +1.96.0 test -p pg-kronika-web --target "$HOST"
cargo +1.96.0 test --workspace --target "$HOST"
git diff --check
```

Expected: formatting makes no further change on the check pass, all commands
pass and `git diff --check` prints nothing.

- [ ] **Step 5: Run workspace lint and BDD acceptance**

Run:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 clippy --workspace --all-targets --target "$HOST" -- -D warnings
TAGS='@pg_log and @pg16' make test-bdd
```

Expected: clippy reports no warnings; the PostgreSQL 16 log feature passes.
If the local machine lacks the Docker/Nix BDD image, record the exact missing
prerequisite in the PR and rely on CI for this command rather than claiming it
passed.

- [ ] **Step 6: Inspect the final diff against the agreed design**

Run:

```sh
git diff --stat main...HEAD
git diff --check main...HEAD
git grep -n "KRONIKA_PG_LOG_ENABLED" -- bins crates docs
git grep -n "1_039_001" -- crates docs bins
```

Confirm:

- no demo-only default remains;
- no path override silently defeats explicit disable;
- no initial disabled/unavailable cycle emits repeated gaps;
- no API query materializes all historical status rows;
- no analytics threshold or health score changed;
- both languages describe the same defaults and limitations.

- [ ] **Step 7: Commit documentation and final verification fixes**

```sh
git add bins/pg_kronika-collector/README.md bins/pg_kronika-collector/README.ru.md crates/kronika-registry/README.md crates/kronika-registry/README.ru.md docs/type-registry.md docs/type-registry/postgresql.md
git commit -m "docs: explain PostgreSQL log source status"
```

- [ ] **Step 8: Prepare the pull request**

Use the repository PR template when present. The PR body must include:

```markdown
## Summary

- enable supported PostgreSQL stderr discovery by default with an explicit opt-out
- persist source availability transitions and bounded heartbeats in `pg_log_source_status`
- expose the latest status through `/v1/sources` without scanning all status history

## Compatibility

- PGM v1 is unchanged; `1_039_001` is an additive registered section
- existing `pg_log_gap` codes and old PGM files remain readable
- `KRONIKA_PG_LOG_ENABLED=0` still disables discovery and reads

## Verification

- registry, source-log, collector, reader and web unit tests
- workspace fmt and clippy
- PostgreSQL 16 `@pg_log` BDD discovery and rotation scenarios
```
