# Multi-db пул соединений — план реализации

> **Для исполнителей:** реализуйте план по задачам. Обязательный sub-skill:
> `superpowers:subagent-driven-development` (предпочтительно) или
> `superpowers:executing-plans`. Чекбоксы `- [ ]` используются для статуса.

**Цель:** добавить в коллектор пул постоянных соединений: одно главное
соединение для instance-wide метрик и по одному соединению к каждой доступной
базе инстанса. Все соединения получают одинаковые session-настройки, главное
соединение переоткрывается после failover, тяжёлые запросы получают адаптивный
`statement_timeout`. Это подготовка к database-local метрикам
(`user_tables`/`indexes`/`statio`); сами метрики идут отдельным эпиком.

**Архитектура:** новый модуль `pool` в `kronika-source-pg`. Главное соединение
остаётся источником instance-wide метрик и переоткрывается при разрыве. Per-db
соединения нужны для будущих database-local метрик. Session-настройки задаются
одинаково для всех соединений через `options=` в DSN. Спецификация:
`docs/connection-and-multidb.md`. Замечания DBA/DevOps/Rust-arch/perf/obs
учтены в задачах ниже.

**Стек:** Rust (edition 2024), `tokio-postgres` (async), `anyhow`,
`cucumber-rs` (BDD), Nix PG-матрица 15–18 в CI.

## Общие ограничения

- MSRV `1.96` (`rust-toolchain.toml`). Локальные проверки запускать только через
  `export PATH="$HOME/.cargo/bin:$PATH"` (системный `/usr/bin/cargo` = 1.91).
- Проверка каждой задачи: `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test -p kronika-source-pg -p pg_kronika-collector` · `cargo run -p xtask -- check-deps`.
- Линты workspace: `unused_crate_dependencies = "warn"` + CI `-D warnings`. Каждая новая зависимость в `Cargo.toml` должна использоваться в lib-таргете, иначе CI упадёт. `check-deps` проверяет только **internal**-граф крейтов; внешние зависимости (`anyhow`, `tokio`) проверяются при ревью `Cargo.toml`.
- `check-deps`: пул живёт в `kronika-source-pg`; `pg_kronika-collector` уже имеет его в allow-list; обратной зависимости нет; S3/reader не затрагиваются.
- Язык: код / rustdoc / комментарии / BDD — английский. Комментарий объясняет «почему», не «что», и фиксирует **верный** инвариант.
- Live-сценарии BDD запускаются только в CI (Docker/Nix-матрица 15–18); локально достаточно компиляции и clippy для `kronika-bdd`.
- **Соединения ко всем базам без лимита на их число** (решение спеки §8). Ограничение памяти вводится не на соединениях, а на данных: (1) top-N в SQL до материализации строк; (2) **database-local сбор интернирует строки инкрементально, по одной базе за раз, поэтому пик памяти равен одной базе, а не сумме по всем**. Это критично для будущего `user_tables` и фиксируется как обязательный инвариант.
- **Контракт прав:** коллектору нужна роль `pg_monitor` и `CONNECT` на собираемые базы. Базы без `CONNECT` для роли коллектора пропускаются при enumerate, иначе серверный лог будет получать `FATAL` на каждый refresh.
- **`max_connections`:** N баз = N backend-процессов в PG. Лимит не вводим: это зона ответственности оператора. Требование отдельно описать в README/операторской документации.
- **`source_id`:** один коллектор = один `source_id`; два коллектора на один `out_dir` с одинаковым `source_id` смешают сегменты. Это ограничение для оператора.
- Имя в `pg_stat_activity`: `pg_kronika-collector/<CARGO_PKG_VERSION>`.

---

## Структура файлов

- **Изменить** `crates/kronika-source-pg/Cargo.toml` — добавить `anyhow` и `tokio` (feature `rt`); `tokio::spawn` требует runtime-поддержку, её даёт бинарь, крейту достаточно `rt`.
- **Создать** `crates/kronika-source-pg/src/pool.rs` — весь multi-db слой:
  `replace_dbname`, `SessionConfig`/`session_options`/`apply_session_dsn`,
  `AdaptiveTimeout`, `enumerate_databases`, `DatabaseConn`, `ConnectionPool`
  (с `connect`/`ensure_main`/`refresh`/`expected`/`uncovered`).
- **Изменить** `crates/kronika-source-pg/src/lib.rs` — `pub mod pool;`.
- **Изменить** `bins/pg_kronika-collector/src/main.rs` — `Config` (новые env),
  главное соединение через `pool`, `ensure_main` перед снимком, instance-wide
  метрики из `pool.main()`.
- **Создать** `crates/kronika-bdd/features/connection_pool.feature` и шаг в
  `crates/kronika-bdd/src/main.rs`.

Порядок выполнения: чистые функции (1–3) → enumerate (4) → зависимости и пул
(5) → refresh и coverage (6) → интеграция и reconnect (7) → live-BDD (8).

---

### Задача 1: `replace_dbname` — подмена базы в DSN

**Файлы:**
- Создать: `crates/kronika-source-pg/src/pool.rs`
- Изменить: `crates/kronika-source-pg/src/lib.rs`

**Интерфейс:**
- Даёт: `pub fn replace_dbname(dsn: &str, datname: &str) -> String`

- [ ] **Шаг 1: модуль.** В `lib.rs` после `pub mod wal;` добавить `pub mod pool;`.

- [ ] **Шаг 2: код и тест.** В `pool.rs`:

```rust
//! Multi-database connection pool: one main connection for instance-wide
//! metrics (reopened on failover), one per database for database-local
//! metrics.
//!
//! Pool setup returns `anyhow::Result`; per-query errors stay
//! `tokio_postgres::Error` via the handed-out `Client`, so callers can match
//! SQLSTATE 57014/55P03.

/// Replace (or append) `dbname=` in a libpq key=value connection string.
#[must_use]
pub fn replace_dbname(dsn: &str, datname: &str) -> String {
    let mut found = false;
    let mut parts: Vec<String> = dsn
        .split_whitespace()
        .map(|tok| {
            if tok.starts_with("dbname=") {
                found = true;
                format!("dbname={datname}")
            } else {
                tok.to_owned()
            }
        })
        .collect();
    if !found {
        parts.push(format!("dbname={datname}"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::replace_dbname;

    #[test]
    fn replaces_existing_dbname() {
        assert_eq!(replace_dbname("host=h dbname=old user=u", "new"), "host=h dbname=new user=u");
    }

    #[test]
    fn appends_when_absent() {
        assert_eq!(replace_dbname("host=h user=u", "new"), "host=h user=u dbname=new");
    }
}
```

- [ ] **Шаг 3:** `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p kronika-source-pg replace_dbname` должен пройти.

- [ ] **Шаг 4: коммит.** `git add crates/kronika-source-pg/src/pool.rs crates/kronika-source-pg/src/lib.rs && git commit -m "Завести модуль пула: replace_dbname"`

---

### Задача 2: session-настройки через `options=`

**Файлы:** изменить `crates/kronika-source-pg/src/pool.rs`

**Интерфейс:**
- Даёт: `pub struct SessionConfig { pub statement_timeout_ms: u64, pub lock_timeout_ms: u64, pub idle_in_tx_timeout_ms: u64 }`, `pub fn session_options(cfg: &SessionConfig) -> String`, `pub fn apply_session_dsn(base_dsn: &str, cfg: &SessionConfig) -> String`

- [ ] **Шаг 1: код и тест.**

```rust
/// Session GUCs applied to every pool connection (main and per-db) via the
/// connection string, so they take effect before the first query.
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    pub statement_timeout_ms: u64,
    pub lock_timeout_ms: u64,
    pub idle_in_tx_timeout_ms: u64,
}

/// `jit=off`: collector queries are short, JIT costs more than it saves.
/// `lock_timeout` must stay below `statement_timeout` or it never fires.
#[must_use]
pub fn session_options(cfg: &SessionConfig) -> String {
    format!(
        "options='-c statement_timeout={} -c lock_timeout={} \
         -c idle_in_transaction_session_timeout={} -c jit=off'",
        cfg.statement_timeout_ms, cfg.lock_timeout_ms, cfg.idle_in_tx_timeout_ms
    )
}

/// Append session options and keepalives to a base DSN. Keepalives let a dead
/// connection to a failed primary surface in seconds, not the system default.
#[must_use]
pub fn apply_session_dsn(base_dsn: &str, cfg: &SessionConfig) -> String {
    format!(
        "{base_dsn} {} connect_timeout=5 \
         keepalives=1 keepalives_idle=30 keepalives_interval=10 keepalives_count=3",
        session_options(cfg)
    )
}
```

```rust
    #[test]
    fn session_options_carry_timeouts_and_jit_off() {
        let cfg = SessionConfig { statement_timeout_ms: 15_000, lock_timeout_ms: 1_000, idle_in_tx_timeout_ms: 10_000 };
        let o = session_options(&cfg);
        assert!(o.contains("statement_timeout=15000") && o.contains("lock_timeout=1000"));
        assert!(o.contains("idle_in_transaction_session_timeout=10000") && o.contains("jit=off"));
    }

    #[test]
    fn apply_session_dsn_adds_keepalives() {
        let cfg = SessionConfig { statement_timeout_ms: 15_000, lock_timeout_ms: 1_000, idle_in_tx_timeout_ms: 10_000 };
        let d = apply_session_dsn("host=h dbname=d", &cfg);
        assert!(d.starts_with("host=h dbname=d ") && d.contains("keepalives_idle=30") && d.contains("connect_timeout=5"));
    }
```

- [ ] **Шаг 2:** `cargo test -p kronika-source-pg session` должен пройти.
- [ ] **Шаг 3: коммит.** `git commit -am "Session-настройки пула через options= в DSN"`

---

### Задача 3: адаптивный `statement_timeout` — монотонный рост

**Файлы:** изменить `crates/kronika-source-pg/src/pool.rs`

**Интерфейс:**
- Даёт: `pub struct AdaptiveTimeout` + `new(start_ms,cap_ms)`, `current_ms()`, `grow()`, `at_cap()`

- [ ] **Шаг 1: код и тест.**

```rust
/// Adaptive `statement_timeout` for heavy queries (sizes/schema): one per
/// PgKronika instance, ratchets up only. The server-side timeout is a backstop
/// — Postgres kills the query even if the collector hangs or is OOM. The caller
/// calls `grow` only on a `57014` (statement_timeout) kill; on `55P03`
/// (lock_timeout) it does NOT — that is a foreign lock, not query cost.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveTimeout {
    current_ms: u64,
    cap_ms: u64,
}

impl AdaptiveTimeout {
    #[must_use]
    pub fn new(start_ms: u64, cap_ms: u64) -> Self {
        Self { current_ms: start_ms.min(cap_ms), cap_ms }
    }
    #[must_use]
    pub fn current_ms(&self) -> u64 {
        self.current_ms
    }
    /// Double, clamped to the cap. No-op at the cap.
    pub fn grow(&mut self) {
        self.current_ms = self.current_ms.saturating_mul(2).min(self.cap_ms);
    }
    #[must_use]
    pub fn at_cap(&self) -> bool {
        self.current_ms >= self.cap_ms
    }
}
```

```rust
    #[test]
    fn adaptive_doubles_up_to_cap() {
        let mut t = AdaptiveTimeout::new(15_000, 60_000);
        assert_eq!(t.current_ms(), 15_000);
        t.grow(); assert_eq!(t.current_ms(), 30_000);
        t.grow(); assert_eq!(t.current_ms(), 60_000);
        t.grow(); assert_eq!(t.current_ms(), 60_000);
        assert!(t.at_cap());
    }

    #[test]
    fn adaptive_start_above_cap_clamps() {
        let t = AdaptiveTimeout::new(120_000, 60_000);
        assert_eq!(t.current_ms(), 60_000);
        assert!(t.at_cap());
    }
```

- [ ] **Шаг 2:** `cargo test -p kronika-source-pg adaptive` должен пройти.
- [ ] **Шаг 3: коммит.** `git commit -am "Адаптивный statement_timeout: ratchet до потолка"`

---

### Задача 4: `enumerate_databases` — список баз с `exclude` и CONNECT-фильтром

**Файлы:** изменить `crates/kronika-source-pg/src/pool.rs`

**Интерфейс:**
- Даёт: `pub const ENUMERATE_SQL: &str`, `pub async fn enumerate_databases(client: &Client, exclude: &std::collections::HashSet<String>) -> Result<Vec<String>, tokio_postgres::Error>`

- [ ] **Шаг 1: код и тест.** `has_database_privilege(datname,'CONNECT')`
  обязателен: `datallowconn` может быть истинным, а у роли коллектора всё равно
  не будет `CONNECT`. Без фильтра серверный лог получает `FATAL` на каждый
  refresh.

```rust
use std::collections::HashSet;
use tokio_postgres::Client;

/// Databases this role may actually connect to (not just `datallowconn`),
/// templates excluded, deterministic order.
pub const ENUMERATE_SQL: &str = "/* pg_kronika pool */ SELECT datname \
    FROM pg_catalog.pg_database \
    WHERE datallowconn AND NOT datistemplate \
      AND pg_catalog.has_database_privilege(datname, 'CONNECT') \
    ORDER BY datname";

/// List target databases for the pool, minus the configured exclude set.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn enumerate_databases(
    client: &Client,
    exclude: &HashSet<String>,
) -> Result<Vec<String>, tokio_postgres::Error> {
    let rows = client.query(ENUMERATE_SQL, &[]).await?;
    Ok(rows
        .iter()
        .map(|r| r.get::<_, String>(0))
        .filter(|db| !exclude.contains(db))
        .collect())
}
```

```rust
    #[test]
    fn enumerate_sql_filters_templates_noconn_and_privilege() {
        assert!(ENUMERATE_SQL.contains("datallowconn"));
        assert!(ENUMERATE_SQL.contains("NOT datistemplate"));
        assert!(ENUMERATE_SQL.contains("has_database_privilege"));
        assert!(ENUMERATE_SQL.contains("ORDER BY datname"));
    }
```

- [ ] **Шаг 2:** `cargo test -p kronika-source-pg enumerate` должен пройти. Live-фильтрация проверяется в задаче 8.
- [ ] **Шаг 3: коммит.** `git commit -am "enumerate_databases: CONNECT-фильтр + exclude"`

---

### Задача 5: зависимости и `ConnectionPool`

**Файлы:**
- Изменить: `crates/kronika-source-pg/Cargo.toml`
- Изменить: `crates/kronika-source-pg/src/pool.rs`

**Интерфейс:**
- Даёт: `pub struct DatabaseConn { pub datname: String, client, _conn }` + `client()`; `pub struct ConnectionPool`; `connect(base_dsn,&str; session; exclude) -> anyhow::Result<Self>`; `main()->&Client`; `per_db()->&[DatabaseConn]`; `server_major()->u32`; `ensure_main()->anyhow::Result<()>`

- [ ] **Шаг 1: `Cargo.toml`.** В `[dependencies]` `kronika-source-pg` добавить:

```toml
anyhow = "1"
tokio = { version = "1", features = ["rt"] }
```

- [ ] **Шаг 2: код.** В `pool.rs`. **`JoinHandle` + `Drop`-abort** повторяет
  эталон `kronika-bdd/src/cluster.rs::Conn`: drop `JoinHandle` не отменяет
  задачу, поэтому `Drop` вызывает `abort()` явно.

```rust
use crate::server_major;
use anyhow::Context;
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

/// One per-database connection. The spawned connection-future is aborted on
/// drop (a dropped `JoinHandle` does NOT cancel the task by itself), so a
/// removed database leaves no driver task running.
pub struct DatabaseConn {
    pub datname: String,
    client: Client,
    conn: JoinHandle<()>,
}

impl DatabaseConn {
    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Drop for DatabaseConn {
    fn drop(&mut self) {
        self.conn.abort();
    }
}

/// Pool: one main connection (instance-wide, reopened on failure) plus one per
/// database (database-local). `target` is the last enumerated database set, so
/// coverage (which databases were reachable) is computable from pool state.
pub struct ConnectionPool {
    base_dsn: String,
    session: SessionConfig,
    exclude: HashSet<String>,
    main: Client,
    main_conn: JoinHandle<()>,
    server_major: u32,
    per_db: Vec<DatabaseConn>,
    target: Vec<String>,
    last_refresh: Instant,
}

/// Open a connection with the session DSN already applied; spawn its driver.
async fn open(dsn: &str) -> anyhow::Result<(Client, JoinHandle<()>, Option<u32>)> {
    let cfg: tokio_postgres::Config = dsn.parse().context("parse DSN")?;
    let (client, connection) = cfg.connect(NoTls).await.context("connect")?;
    let major = server_major(connection.parameter("server_version"));
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, handle, major))
}

impl ConnectionPool {
    /// Open the main connection. The per-db pool is filled by `refresh`.
    ///
    /// # Errors
    /// Fails if the main connection cannot be established or reports no version.
    pub async fn connect(
        base_dsn: &str,
        session: SessionConfig,
        exclude: HashSet<String>,
    ) -> anyhow::Result<Self> {
        let dsn = apply_session_dsn(base_dsn, &session);
        let (main, main_conn, major) = open(&dsn).await?;
        let server_major = major.context("server reported no parseable server_version")?;
        Ok(Self {
            base_dsn: base_dsn.to_owned(),
            session,
            exclude,
            main,
            main_conn,
            server_major,
            per_db: Vec::new(),
            target: Vec::new(),
            last_refresh: Instant::now(),
        })
    }

    #[must_use]
    pub fn main(&self) -> &Client {
        &self.main
    }
    #[must_use]
    pub fn per_db(&self) -> &[DatabaseConn] {
        &self.per_db
    }
    #[must_use]
    pub fn server_major(&self) -> u32 {
        self.server_major
    }

    /// Reopen the main connection if it died (failover/restart). Refreshes
    /// `server_major` from the new handshake. Call before every snapshot —
    /// without it a collector survives a primary failover as a live-but-blind
    /// process and would tag metrics with a stale major.
    ///
    /// # Errors
    /// Fails if reconnection fails or the new server reports no version.
    pub async fn ensure_main(&mut self) -> anyhow::Result<()> {
        if !self.main.is_closed() {
            return Ok(());
        }
        let dsn = apply_session_dsn(&self.base_dsn, &self.session);
        let (client, conn, major) = open(&dsn).await?;
        self.main_conn.abort();
        self.main = client;
        self.main_conn = conn;
        self.server_major = major.context("server reported no parseable server_version")?;
        Ok(())
    }
}
```

- [ ] **Шаг 3: проверка.** `cargo build -p kronika-source-pg` → `cargo clippy -p kronika-source-pg --all-targets -- -D warnings`. Должно быть чисто, включая `unused_crate_dependencies`.
- [ ] **Шаг 4: коммит.** `git add crates/kronika-source-pg/Cargo.toml crates/kronika-source-pg/src/pool.rs && git commit -m "ConnectionPool: главное соединение, ensure_main, JoinHandle+Drop"`

---

### Задача 6: `refresh` и coverage-аксессоры

**Файлы:** изменить `crates/kronika-source-pg/src/pool.rs`

**Интерфейс:**
- Даёт: `pub async fn ConnectionPool::refresh(&mut self, interval: std::time::Duration) -> anyhow::Result<()>`; `pub fn expected(&self) -> &[String]`; `pub fn uncovered(&self) -> Vec<String>`

- [ ] **Шаг 1: код.** Интервал приходит параметром из env, без хардкода.
  `target` хранит ожидаемый набор баз. `uncovered` возвращает базы, которые
  ожидались, но не имеют живого соединения. Порядок `per_db` не гарантируется:
  потребитель должен обращаться по `datname`, а не по индексу.

```rust
    /// Reconcile the per-db pool with the live database list. Cheap to call
    /// every snapshot: works only once `interval` elapsed (or while the pool is
    /// empty). A database that fails to connect is logged and skipped — it
    /// retries next refresh. Pool order is not stable; address by `datname`.
    ///
    /// # Errors
    /// Fails only if enumerating databases on the main connection fails.
    pub async fn refresh(&mut self, interval: std::time::Duration) -> anyhow::Result<()> {
        if !self.per_db.is_empty() && self.last_refresh.elapsed() < interval {
            return Ok(());
        }
        let target = enumerate_databases(&self.main, &self.exclude)
            .await
            .context("enumerate databases")?;
        let target_set: HashSet<&str> = target.iter().map(String::as_str).collect();
        self.per_db.retain(|c| target_set.contains(c.datname.as_str()));
        let have: HashSet<String> = self.per_db.iter().map(|c| c.datname.clone()).collect();
        for db in &target {
            if have.contains(db) {
                continue;
            }
            let dsn = apply_session_dsn(&replace_dbname(&self.base_dsn, db), &self.session);
            match open(&dsn).await {
                Ok((client, conn, _)) => {
                    self.per_db.push(DatabaseConn { datname: db.clone(), client, conn });
                }
                Err(err) => eprintln!("pg_kronika: per-db connect to {db} failed: {err:#}"),
            }
        }
        self.target = target;
        self.last_refresh = Instant::now();
        Ok(())
    }

    /// Databases the pool last expected to cover.
    #[must_use]
    pub fn expected(&self) -> &[String] {
        &self.target
    }

    /// Expected databases with no live connection (failed/locked out).
    #[must_use]
    pub fn uncovered(&self) -> Vec<String> {
        let have: HashSet<&str> = self.per_db.iter().map(|c| c.datname.as_str()).collect();
        self.target.iter().filter(|d| !have.contains(d.as_str())).cloned().collect()
    }
```

- [ ] **Шаг 2: проверка.** `cargo clippy -p kronika-source-pg --all-targets -- -D warnings` должен пройти.
- [ ] **Шаг 3: коммит.** `git commit -am "ConnectionPool::refresh + coverage-аксессоры"`

---

### Задача 7: интеграция в коллектор — `ensure_main` и метрики инстанса через `pool.main()`

**Файлы:** изменить `bins/pg_kronika-collector/src/main.rs`

**Интерфейс:**
- Использует: `kronika_source_pg::pool::{ConnectionPool, SessionConfig}`

- [ ] **Шаг 1: `Config` и env.** Заменить `struct Config`/`from_env`:

```rust
struct Config {
    dsn: String,
    out_dir: PathBuf,
    source_id: u64,
    session: kronika_source_pg::pool::SessionConfig,
    exclude_databases: std::collections::HashSet<String>,
    pool_refresh: std::time::Duration,
}

fn env_u64(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Ok(v) => v.parse().with_context(|| format!("{key} is not a u64")),
        Err(_) => Ok(default),
    }
}

impl Config {
    fn from_env() -> Result<Self> {
        let dsn = std::env::var("KRONIKA_PG_DSN").context("KRONIKA_PG_DSN is not set")?;
        let out_dir = std::env::var("KRONIKA_OUT_DIR").context("KRONIKA_OUT_DIR is not set")?.into();
        let source_id = env_u64("KRONIKA_SOURCE_ID", 0)?;
        let session = kronika_source_pg::pool::SessionConfig {
            statement_timeout_ms: env_u64("KRONIKA_PG_STATEMENT_TIMEOUT_MS", 15_000)?,
            lock_timeout_ms: env_u64("KRONIKA_PG_LOCK_TIMEOUT_MS", 1_000)?,
            idle_in_tx_timeout_ms: env_u64("KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS", 10_000)?,
        };
        let exclude_databases: std::collections::HashSet<String> = std::env::var("KRONIKA_PG_EXCLUDE_DATABASES")
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !exclude_databases.is_empty() {
            eprintln!("pg_kronika: excluding databases: {exclude_databases:?}");
        }
        let pool_refresh = std::time::Duration::from_secs(env_u64("KRONIKA_PG_POOL_REFRESH_SECS", 600)?);
        Ok(Self { dsn, out_dir, source_id, session, exclude_databases, pool_refresh })
    }
}
```

- [ ] **Шаг 2: пул в `main`.** Заменить ручной блок `pg_config.connect` на:

```rust
    let mut pool = kronika_source_pg::pool::ConnectionPool::connect(
        &config.dsn,
        config.session,
        config.exclude_databases.clone(),
    )
    .await
    .context("connect pool")?;
    let major = pool.server_major();
```

- [ ] **Шаг 3: `ensure_main` перед снимком.** В обработчике SIGUSR2, до
  `snapshot_and_seal`, добавить переподключение и обновление мажора:

```rust
                if let Err(err) = pool.ensure_main().await {
                    eprintln!("pg_kronika-collector: main reconnect failed: {err:#}");
                    continue;
                }
                let major = pool.server_major();
                match snapshot_and_seal(pool.main(), major, &mut journal, &config.out_dir, config.source_id).await {
```

`pool` теперь `mut`. `major` берётся заново на каждый снимок, чтобы failover на
другой мажор не записал данные с устаревшей схемой. Сигнатура
`snapshot_and_seal` не меняется: `client: &Client`.

- [ ] **Шаг 4: проверка.** `export PATH="$HOME/.cargo/bin:$PATH"`, затем:
  `cargo test -p kronika-source-pg -p pg_kronika-collector` ·
  `cargo clippy --workspace --all-targets -- -D warnings` ·
  `cargo run -p xtask -- check-deps`. Все проверки должны пройти; существующие
  тесты коллектора сохраняют прежнее поведение сегмента.
- [ ] **Шаг 5: коммит.** `git commit -am "Коллектор: ConnectionPool + ensure_main; instance-wide через pool.main()"`

---

### Задача 8: BDD — пул и перечисление баз на матрице

**Файлы:**
- Создать: `crates/kronika-bdd/features/connection_pool.feature`
- Изменить: `crates/kronika-bdd/src/main.rs`

- [ ] **Шаг 1: feature-файл.** Live-матрица — PG 15–18 (10–13 вне nixpkgs, golden):

```gherkin
Feature: Collector pools a connection to every database
  The pool opens one connection per non-template database the role may connect
  to, and enumeration excludes templates. Live matrix is PG 15-18.

  Scenario: matrix clusters pool every database
    Given the PostgreSQL matrix is booted
    Then each matrix cluster pools one connection per database
```

- [ ] **Шаг 2: реализация BDD-шага.** DSN кластера берётся из `Cluster::conn_string()`
  (`pub(crate)`, отдаёт `host=... dbname=postgres` — `replace_dbname` в `refresh`
  заменит `dbname=postgres` на каждую базу). Проверить три факта: пул непуст,
  enumerate не возвращает `template0` и `template1`, `uncovered` пуст на чистой
  матрице.

```rust
#[then("each matrix cluster pools one connection per database")]
async fn every_cluster_pools_databases(world: &mut BddWorld) -> anyhow::Result<()> {
    use kronika_source_pg::pool::{ConnectionPool, SessionConfig, enumerate_databases};
    use std::collections::HashSet;
    use std::time::Duration;
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    let session = SessionConfig { statement_timeout_ms: 15_000, lock_timeout_ms: 1_000, idle_in_tx_timeout_ms: 10_000 };
    for db in &world.clusters {
        let dsn = db.conn_string();
        let mut pool = ConnectionPool::connect(&dsn, session, HashSet::new()).await?;
        pool.refresh(Duration::from_secs(0)).await?;
        anyhow::ensure!(!pool.per_db().is_empty(), "postgres {}: empty pool", db.major());
        anyhow::ensure!(pool.uncovered().is_empty(), "postgres {}: uncovered {:?}", db.major(), pool.uncovered());
        let names = enumerate_databases(pool.main(), &HashSet::new()).await?;
        anyhow::ensure!(!names.iter().any(|n| n == "template0" || n == "template1"),
            "postgres {}: enumeration leaked a template db", db.major());
    }
    Ok(())
}
```

  Если `conn_string()` недостаточно видим из модуля шага, расширить видимость
  до `pub(crate)` (сейчас она уже такая) либо добавить тонкий аксессор. Ориентир:
  как `collector::Collector::spawn` получает подключение к кластеру.

- [ ] **Шаг 3: локальная проверка.** `cargo clippy -p kronika-bdd --all-targets -- -D warnings` должен пройти. Live-прогон остаётся в CI.
- [ ] **Шаг 4: коммит.** `git add crates/kronika-bdd/features/connection_pool.feature crates/kronika-bdd/src/main.rs && git commit -m "BDD: пул открывает соединение на каждую базу матрицы"`

---

## Вне этого плана

- **Применение `AdaptiveTimeout`** в цикле сбора: `SET statement_timeout` перед
  тяжёлой группой, `grow()` по `57014`, пропуск базы по `55P03`. Добавляется с
  первым тяжёлым запросом, например с размерами. Конфиг потолка:
  `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` (60s).
- **Проверка здоровья и пересоздание мёртвых per-db соединений** — с первым
  database-local потребителем, где ошибка запроса наблюдаема.
- **`!Send`-инвариант database-local сбора** (зафиксировать при `user_tables`):
  интернировать строки инкрементально, по одной базе за раз. Пик памяти должен
  равняться одной базе; сырьё со всех баз нельзя копить перед общим
  interning-проходом.
- **Coverage в сегменте:** писать `expected`/`uncovered` (тип `1_023_001`
  coverage), чтобы недосбор был виден при разборе, а не только в stderr.
- **Первый потребитель — `pg_stat_user_tables`** (класс B): top-N, skip locked,
  relpages-размер, метка `datname`. Отдельный план.
- **Size-цикл** и пересмотр `database_size_bytes` (#27). Учесть: при обёртке
  size-запроса в транзакцию `idle_in_transaction_session_timeout` (10s) меньше
  потолка heavy-таймаута (60s) — выставлять idle-timeout локально или не
  оборачивать в явный BEGIN.
- **Reconcile-сценарий BDD** (создать/удалить базу → пул догоняет) здесь не
  покрыт. Добавить, когда появится дешёвый способ проверить это в матрице.
- **Режим одной базы** (`PGDATABASE`/явный `dbname=` → пул из одной базы).
- **README оператора**: роль `pg_monitor` + `CONNECT`; связь «N баз = N backend»
  и `max_connections`; уникальность `source_id` на коллектор.

## Самопроверка

- **Покрытие спеки:** пул (§2), session+keepalives (§3), структура адаптивного
  таймаута (§3.1; применение вынесено), enumerate+exclude+CONNECT (§2.4),
  reconnect main (§6), env-конфиг включая refresh (§7) покрыты задачами 1–8.
  Ветвление по репликам (§4) уже находится в метрике replication; размеры (§5),
  health-check и coverage-в-сегмент вынесены отдельно.
- **Проверка памяти:** соединения остаются без cap (решение
  §8). Пик памяти инфраструктуры: `target: Vec<String>` + N `Client`,
  ограничено числом баз, которое коллектор уже хранит как вход. Реальный риск
  по памяти возникает на database-local потребителях из-за строк × баз; он
  закрывается инвариантом «инкрементальное интернирование по одной базе»
  (см. общие ограничения и раздел «Вне этого плана»).
- **Неполные места:** задачи 1–4 дают полный код и тесты без I/O; задачи 5–7
  дают async-код; live-проверка идёт через BDD в задаче 8. Единственная точка
  адаптации — `conn_string()` в задаче 8, она помечена явно.
- **Согласованность типов:** `SessionConfig` (задача 2) одинаково используется в `connect`/
  `ensure_main` (5), `Config` (7), BDD (8); `refresh(interval)` (6) зовётся с
  `config.pool_refresh` (7) и `Duration::from_secs(0)` (8); `JoinHandle`+`Drop`
  единообразны для main и per-db.
