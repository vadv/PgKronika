# Сбор по базам: соединения и граница PR #29

Статус: контракт PR #29, 2026-06-29.

Документ описывает подключение коллектора к PostgreSQL, session-настройки,
перечисление баз и границу текущего PR. PR добавляет библиотечный слой для
database-local метрик; сами database-local метрики и вызов per-db refresh из
демона в него не входят.

## Классы метрик

- **Instance-wide.** Одно соединение видит строки по всему инстансу:
  `pg_stat_database`, `pg_stat_activity`, `pg_stat_io`, `pg_stat_wal`,
  `pg_stat_bgwriter`, `pg_stat_archiver`, `pg_prepared_xacts`,
  `pg_stat_progress_*`, replication. Текущий демон собирает этот класс через
  главное соединение пула.
- **Database-local.** Данные видны только из выбранной базы:
  `pg_stat_user_tables`, `pg_stat_user_indexes`, `pg_statio_*`,
  `pg_stat_user_functions`, размеры объектов, bloat. Для полного покрытия нужен
  клиент на каждую базу, которую роль коллектора может открыть.

## Модель соединений

PgKronika использует `tokio-postgres`. Каждое соединение состоит из `Client` и
driver-задачи: она запускается через `tokio::spawn`, а `JoinHandle` хранится для
явного `abort` при удалении соединения из пула.

```text
DatabaseConn {
    datname: String,
    client: Client,
    conn: JoinHandle<()>,
}

ConnectionPool {
    base_dsn: String,
    application_name: String,
    session: SessionConfig,
    exclude: HashSet<String>,
    main: Client,
    main_conn: JoinHandle<()>,
    server_major: u32,
    per_db: Vec<DatabaseConn>,
    target: Vec<String>,
    last_refresh: Instant,
}
```

`ConnectionPool::connect` открывает главное соединение. Per-db соединения
создаёт `refresh(interval, max_databases)`.

## Подключение

DSN разбирается через `tokio_postgres::Config`, поэтому поддерживаются оба
формата: `key=value` и URI. Параметры не дописываются строковой склейкой:

- `dbname` задаётся структурным setter для per-db соединений;
- `application_name` задаётся через `Config::application_name`;
- `connect_timeout=5`;
- TCP keepalive: `idle=30`, `interval=10`, `retries=3`;
- startup options: `statement_timeout`, `lock_timeout`,
  `idle_in_transaction_session_timeout`.

JIT не настраивается в startup options: PostgreSQL 10 не знает этот GUC, а
текущий PR сохраняет совместимость с PG10.

## Session-настройки

| Параметр | Дефолт | Способ | Контракт |
|---|---:|---|---|
| `statement_timeout` | `15000` ms | startup options | ограничивает лёгкие collector-запросы |
| `lock_timeout` | `1000` ms | startup options | должен быть меньше `statement_timeout` |
| `idle_in_transaction_session_timeout` | `10000` ms | startup options | закрывает зависшую idle-in-transaction сессию |
| `application_name` | `pg_kronika-collector/<version>` | connection param | видимость в `pg_stat_activity` |
| TCP keepalive | `30/10/3` | connection param | быстрее обнаруживает разрыв TCP-сессии |

`SessionConfig::validate` отклоняет нулевые timeout-значения и
`lock_timeout >= statement_timeout`.

## Перечисление баз

`ENUMERATE_SQL` возвращает базы в стабильном порядке по имени:

```sql
SELECT datname
FROM pg_catalog.pg_database
WHERE datallowconn
  AND NOT datistemplate
  AND pg_catalog.has_database_privilege(datname, 'CONNECT')
ORDER BY datname
```

Фильтр `CONNECT` нужен, чтобы refresh не открывал заведомо недоступные базы и
не создавал `FATAL` в серверном логе. Exclude-список применяется после SQL.

Discovery не вызывает `pg_database_size()`: refresh не должен добавлять
filesystem I/O к каждому циклу.

## Refresh и coverage

`refresh(interval, max_databases)`:

1. пропускает работу, пока `interval` не истёк, если в пуле уже есть per-db
   соединения;
2. перечисляет все ожидаемые базы;
3. оставляет в `per_db` только открытые соединения, которые попадают в cap;
4. открывает недостающие соединения для первых `max_databases` баз в порядке
   имени;
5. сохраняет полный список `expected`, включая базы сверх cap.

Если баз больше cap, лишние базы остаются в `uncovered`; это не ошибка refresh.
`DEFAULT_MAX_DATABASES` сейчас равен `20`, env-переменной для него пока нет.

`uncovered()` возвращает ожидаемые базы без живого клиента: недоступные,
закрытые после failover или оставшиеся за cap.

## Failover

Перед snapshot демон вызывает `ensure_main()`. Если главное соединение закрыто,
пул открывает новое, проверяет `server_version` из handshake и только после
этого заменяет клиент и driver handle. Так snapshot после failover использует
актуальный PostgreSQL major.

Per-db refresh удаляет закрытые клиенты по `Client::is_closed()` и пробует
открыть их заново. Active ping для half-open соединений в PR #29 не входит.

## Переменные окружения коллектора

| Переменная | Дефолт | Назначение |
|---|---:|---|
| `KRONIKA_PG_DSN` | нет | базовая строка подключения, `key=value` или URI |
| `KRONIKA_OUT_DIR` | нет | каталог для запечатанных сегментов |
| `KRONIKA_SOURCE_ID` | `0` | идентификатор источника в сегменте |
| `KRONIKA_PG_STATEMENT_TIMEOUT_MS` | `15000` | `statement_timeout` |
| `KRONIKA_PG_LOCK_TIMEOUT_MS` | `1000` | `lock_timeout` |
| `KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS` | `10000` | `idle_in_transaction_session_timeout` |
| `KRONIKA_PG_EXCLUDE_DATABASES` | пусто | базы для исключения, разделитель `;` |

`KRONIKA_SOURCE_ID=0` опасен для нескольких коллекторов с общим
`KRONIKA_OUT_DIR`: проверка смешивания source id не различает два дефолтных
нулевых источника. Для multi-collector раскладки задавайте уникальный ненулевой
id каждому источнику.

## Что не входит в PR #29

- Вызов `pool.refresh()` в цикле демона. Демон пока использует `pool.main()` для
  instance-wide метрик.
- Сбор `pg_stat_user_tables` и других database-local метрик.
- Запись coverage в сегмент.
- Active ping для half-open per-db соединений.
- Backoff при массовом отказе per-db подключений.
- Параллельное открытие per-db соединений.
- Приоритет крупных баз при превышении cap.
- Size-цикл, `pg_database_size()` и применение `AdaptiveTimeout` к запросам.
- Env-переменная для `DEFAULT_MAX_DATABASES`.
- Single-database режим, default-excludes и include-regex.

## Проверка в PR

- Unit-тесты `kronika-source-pg::pool` проверяют session config, PG10-safe
  startup options без `jit`, URI DSN, `application_name`, validation и SQL
  enumeration.
- Live-BDD проверяет, что пул открывает per-db соединения для матричных
  PostgreSQL кластеров и не перечисляет template-базы.
