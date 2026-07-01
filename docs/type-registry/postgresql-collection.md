# Приёмы сбора PostgreSQL

Этот файл фиксирует приёмы из первой реализации, которые должны сохраниться при
переходе на PGM. Это не байтовая раскладка типов, а контракт поведения
коллектора.

## Общие правила

- Каждый SQL-запрос коллектора начинается с комментария-маркера
  `/* pg_kronika:<version> <source-file> */`. Маркер отделяет собственные
  запросы коллектора в `pg_stat_activity`, `pg_stat_statements`, логах и внешней
  аналитике.
- Запросы к системным каталогам выполняются через `query_with_lock_retry`:
  DDL может временно держать lock на каталогах.
- При ошибке соединения сбрасывается клиент и весь кэш окружения:
  `server_version_num`, версии расширений, наличие функций и прочие признаки.
- Multidb-режим использует по одному соединению на каждую не-template базу с
  `datallowconn`. Пул обновляется примерно раз в 10 минут.
- Режима одной базы нет: `dbname`/`PGDATABASE` из DSN задаёт только стартовый
  коннект, а per-db соединения всё равно открываются по всем базам.

## Права PostgreSQL

Коллектор должен работать с минимально достаточными правами. `pg_monitor`
может использоваться как простой профиль развёртывания, но для рабочей
установки желателен набор минимальных прав.

| Источник | Минимальное требование | Деградация |
|----------|------------------------|------------|
| `pg_stat_activity`, wait events, locks | доступ к `pg_stat_*`; для чужих запросов может требоваться `pg_read_all_stats` или `pg_monitor` | скрытые поля пишутся `NULL`, генерируется permission-событие |
| `pg_stat_statements` | расширение установлено, права на view/function | тип отсутствует, version поля в `reset_metadata` = `NULL` |
| `pg_store_plans` | расширение установлено в одной из БД пула, права на view/function | тип отсутствует или plan/reset поля `NULL` |
| `pg_settings` | доступ к `pg_settings`; часть sourcefile может быть скрыта | скрытые поля `NULL` |
| представления репликации | права на `pg_stat_replication`, `pg_replication_slots`; часто нужна роль мониторинга | типы репликации отсутствуют или частично `NULL` |
| PostgreSQL logs | файловый доступ к `log_directory` или sidecar shipping | логовые event_stream-типы отсутствуют, генерируется permission-событие |

Нули не используются как замена недоступных значений. Недоступность источника
должна быть видна в metadata, событии или self-metrics коллектора.

## `1_001_001` activity

```sql
/* pg_kronika:<version> <source-file> */
SELECT
  pid,
  datname,
  usename,
  application_name,
  host(client_addr) AS client_addr,
  state,
  LEFT(query, :max_text_len) AS query,
  query_id,
  wait_event_type,
  wait_event,
  backend_type,
  backend_start,
  xact_start,
  query_start,
  state_change,
  now() AS collected_at
FROM pg_stat_activity;
```

`query_id` доступен не на всех версиях PostgreSQL и при
`compute_query_id = off` может быть нулем.

## `1_002_001`..`1_002_006` statements

`pg_stat_statements` — представление уровня кластера: строка содержит
`(userid, dbid, queryid)`, а `dbid` указывает базу выполнения. Коллектор выполняет
один запрос из базы, где установлено расширение. Источник выбирается и кэшируется:
сначала `pool.main()`, затем покрытые соединения `per_db()`. На ошибке запроса,
закрытом соединении, пропавшем расширении или изменении `extversion` кэш
сбрасывается и запускается ограниченное повторное обнаружение. Поиск источника
видит только `pool.main()` и покрытые per-db соединения; базы вне лимита пула
останутся невидимыми до появления явной настройки исходной БД.

Раскладка выбирается по версии расширения, а не по мажору сервера. Проба:

```sql
/* pg_kronika:<version> <source-file> */
SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements';
```

`extversion` (`"1.11"` и т. п.) разбирается в `major.minor` и отображается в тип:
≤ 1.7 → `1_002_001`, 1.8 → `_002`, 1.9 → `_003`, 1.10 → `_004`, 1.11 → `_005`,
≥ 1.12 → `_006`. Нераспознанная строка выбирает legacy-раскладку.

Один запрос со встроенным усечением текста (без второй фазы):

```sql
/* pg_kronika:<version> <source-file> */
WITH candidates AS (
  (SELECT userid, dbid, queryid FROM pg_stat_statements
     ORDER BY total_exec_time DESC NULLS LAST LIMIT $1)
  UNION
  (SELECT userid, dbid, queryid FROM pg_stat_statements
     ORDER BY calls DESC NULLS LAST LIMIT $1)
)
SELECT
  s.queryid, s.userid, s.dbid,
  d.datname::text AS datname, r.rolname::text AS usename,
  LEFT(s.query, 5000) AS query,
  ...
FROM pg_stat_statements s
JOIN candidates c ON c.userid = s.userid AND c.dbid = s.dbid
  AND c.queryid IS NOT DISTINCT FROM s.queryid
LEFT JOIN pg_database d ON d.oid = s.dbid
LEFT JOIN pg_roles r ON r.oid = s.userid;
```

Отбор кандидатов объединяет две оси top-N: по `total_exec_time` (в
legacy-раскладке — по `total_time`) и по `calls`, без порогов и вердиктов в SQL
(см. `1_002` в `postgresql.md`). Лимит на ось — `KRONIKA_PG_MAX_STATEMENTS`, по
умолчанию 500. Текст берётся прямо в `SELECT` как `LEFT(query, 5000)` и
интернируется. `IS NOT DISTINCT FROM` в join нужен, чтобы строка с `NULL`
`queryid` (выключенный `compute_query_id`) совпала со своим кандидатом. Для
legacy-раскладки используются старые имена колонок времени (`total_time` и т.
п.); переименование `blk_*_time` → `shared_blk_*_time` начинается с версии
расширения 1.11.

## `1_003_001` / `1_004_001` store_plans

Fork определяется при старте по наличию функций в `pg_proc`.

- ossc: план доступен готовой колонкой `s.plan`;
- vadv-форк: план реконструируется через
  `pg_store_plans_textplan(pg_store_plans_get_plan(userid, dbid, queryid, planid))`.

Время IO у vadv-форка нормализуется как сумма shared, local и temp
`blk_*_time`.

## `1_011_001` / `1_011_002` граф ожиданий lock

Сбор двухступенчатый. Класс A — соединение из `pool.main()`.

**Ступень 1:** дешёвая предварительная проверка по `pg_stat_activity` с кэшем
около 1 секунды.

```sql
/* pg_kronika:<version> <source-file> */
SELECT EXISTS (
  SELECT 1
  FROM pg_stat_activity
  WHERE wait_event_type = 'Lock'
);
```

`pg_blocking_pids` вызывается только для backend'ов с
`wait_event_type = 'Lock'` (семена графа). Так коллектор не берёт lock manager
для backend'ов, которые не ждут lock.

Если предварительная проверка не нашла ожиданий, ступень 2 не выполняется;
секция в сегмент не пишется.

**Ступень 2:** cycle-safe рекурсивный CTE поднимается от заблокированных
backend'ов к корням через `pg_blocking_pids`, дедуплицирует узлы и
джойнится с `pg_locks NOT GRANTED`, чтобы получить тип, режим и цель
ожидаемой блокировки. `LEFT JOIN pg_locks` по `pid` и `NOT granted`
даёт ожидаемый lock конкретного backend'а.

```sql
WITH RECURSIVE
  waiters AS (SELECT pid, pg_blocking_pids(pid) AS bp
              FROM pg_stat_activity WHERE wait_event_type = 'Lock'),
  edges AS (SELECT pid AS waiter, b AS blocker FROM waiters, unnest(bp) AS b),
  roots AS (SELECT DISTINCT blocker AS pid FROM edges
            WHERE blocker <> 0 AND blocker NOT IN (SELECT pid FROM waiters)),
  tree AS (SELECT pid, 0 AS depth, pid AS root_pid FROM roots
           UNION ALL
           SELECT e.waiter, t.depth + 1, t.root_pid
           FROM tree t JOIN edges e ON e.blocker = t.pid)
           CYCLE pid SET is_cycle USING path,  -- PG14+; PG10-13: WHERE NOT ... = ANY(path)
  nodes AS (SELECT pid, min(depth) AS depth,
                   (array_agg(root_pid ORDER BY depth))[1] AS root_pid
            FROM tree GROUP BY pid)
SELECT n.pid, n.depth, n.root_pid, /* + backend- и lock-колонки */
       pg_blocking_pids(n.pid) AS blocked_by
FROM nodes n JOIN pg_stat_activity USING (pid)
LEFT JOIN pg_locks l ON l.pid = n.pid AND NOT l.granted
LIMIT :max_rows;
```

Семена рекурсии — корни (блокеры, сами не ждущие), от них спуск к ждущим.
PID `0` в `blocked_by` — подготовленная (двухфазная) транзакция: её владелец
виден в `pg_locks`, но строки в `pg_stat_activity` нет, поэтому она исключена
из `roots` и не имеет собственной строки-узла — остаётся только внутри
массивов `blocked_by`.

Имя таблицы (`lock_relname`) берётся через `pg_class`, когда отношение видно из
базы подключения коллектора. Блокировки в других базах кластера дают пустое
имя при сохранённом `lock_relation`.

Лимит строк результата — `KRONIKA_PG_MAX_LOCK_ROWS` (по умолчанию 1000).
Отдельная coverage-строка для усечений пока не пишется.

## `1_013_001`..`1_013_004` / `1_014_001`..`1_014_002` tables и indexes

Для таблиц выполняется один запрос на базу (через пул соединений, итерация по
`per_db()`): `pg_stat_user_tables` с `LEFT JOIN pg_statio_user_tables` по `relid`,
размерами через `pg_relation_size`/`pg_total_relation_size` и `xid_age`/`mxid_age`/
`reltuples` из `pg_class`. Отбор кандидатов — чисто механический: top-N по сырым
колонкам (активность чтения ∪ запись `n_tup_ins+upd+del` ∪ `relpages` ∪
dead tuples `n_dead_tup` ∪ `age(relfrozenxid)` ∪ `mxid_age(relminmxid)`), без
порогов и вердиктов (см. `1_013` в `postgresql.md`). `heap_blks_read`/
`heap_blks_hit` из `pg_statio_user_tables` — счётчики shared-буферов PostgreSQL
(промах/попадание буфера), а не I/O ОС или блочного устройства. Запрос идёт под
адаптивным `statement_timeout`. Усечённый сбор пока не пишет coverage
`1_023_001`.

Для индексов (`1_014`) схема аналогична и симметрична таблицам: один запрос на
базу по `per_db()` под тем же адаптивным `statement_timeout`, свой env
`KRONIKA_PG_MAX_INDEXES` как per-axis top-N (отдельно от `KRONIKA_PG_MAX_TABLES`).
Источники строки:

- `pg_stat_user_indexes`;
- `pg_statio_user_indexes` (`idx_blks_read`/`idx_blks_hit` под `COALESCE(..., 0)`,
  `LEFT JOIN` по `indexrelid` — гонка каталога и статистики не даст `NULL`);
- `main_fork_bytes` = `pg_relation_size(indexrelid)`;
- `left(pg_get_indexdef(indexrelid), 5000)` — обрезка на уровне SQL;
- `pg_am.amname` через `pg_class.relam`;
- флаги `indisunique`/`indisprimary`/`indisvalid`/`indisexclusion`/`indisready` из
  `pg_index`.

Отбор кандидатов — чисто механический top-N по сырым колонкам (`idx_scan` ∪
`idx_tup_read` ∪ `pg_class.relpages`; PG16+ ещё `last_idx_scan` с
`WHERE last_idx_scan IS NOT NULL`), каждая ось с `indexrelid` последним ключом
`ORDER BY` для детерминированного top-N, без порогов и вердиктов. Ось `relpages`
ловит большие неиспользуемые индексы — вывод «большой и ни разу не сканировался»
делает модуль анализа на чтении, а не `WHERE idx_scan = 0` в коллекторе (см.
`1_014` в `postgresql.md`).

## `1_006_001` bgwriter и checkpointer

PG <= 16: читается `pg_stat_bgwriter`.

PG 17+: читаются `pg_stat_checkpointer` и `pg_stat_bgwriter`, затем данные
склеиваются в одну строку. Колонки `buffers_backend` и
`buffers_backend_fsync`, удалённые из PostgreSQL, пишутся как `NULL`.

## `1_020_001` reset metadata

`reset_metadata` собирается минимум один раз на сегмент, лучше дважды: при
открытии сегмента и перед запечатыванием. В секцию пишется последнее
наблюденное значение. Если между двумя чтениями изменился `postmaster_start_time`
или один из `*_reset_at`, коллектор дополнительно генерирует событие
`stats_reset` или `pg_restart`.

Все SQL timestamp-значения переводятся в `unix usec`. Запросы ниже не являются
одним монолитным SQL: коллектор выполняет только те фрагменты, которые
поддерживаются текущей версией PostgreSQL и установленными расширениями.

```sql
-- выражение-шаблон
(EXTRACT(EPOCH FROM ts_value) * 1000000)::bigint
```

`NULL` сохраняется как `NULL`; подставлять `0` запрещено.

### Базовые поля

```sql
SELECT
  (EXTRACT(EPOCH FROM pg_postmaster_start_time()) * 1000000)::bigint
    AS postmaster_start_time;

SELECT
  (EXTRACT(EPOCH FROM MAX(stats_reset)) * 1000000)::bigint
    AS pg_stat_database_reset_max_at
FROM pg_stat_database;

SELECT
  MAX(extversion) FILTER (WHERE extname = 'pg_stat_statements')
    AS ext_pg_stat_statements_version,
  MAX(extversion) FILTER (WHERE extname = 'pg_store_plans')
    AS ext_pg_store_plans_version
FROM pg_extension
WHERE extname IN ('pg_stat_statements', 'pg_store_plans');

SELECT
  current_setting('compute_query_id', true) AS compute_query_id,
  current_setting('track_io_timing', true)::bool AS track_io_timing;
```

`compute_query_id` отсутствует на старых версиях PostgreSQL; в этом случае
`current_setting(..., true)` вернет `NULL`.

### `pg_stat_statements`

`pg_stat_statements_info` доступен только в pgss >= 1.9. Запрос выполняется
только если `to_regclass('pg_stat_statements_info') IS NOT NULL`; иначе поле
`pg_stat_statements_reset_at` пишется как `NULL`.

```sql
SELECT
  (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
    AS pg_stat_statements_reset_at
FROM pg_stat_statements_info;
```

### `pg_store_plans`

У ossc `pg_store_plans` есть `pg_store_plans_info.stats_reset`.
Информационное представление и одноименная функция доступны только в базе, где
выполнен `CREATE EXTENSION`, хотя статистика собирается по серверу в целом.
Коллектор должен выполнять запрос в любой доступной базе, где найдено
расширение.

Если форк не предоставляет `pg_store_plans_info` или у текущего пользователя
нет доступа, `pg_store_plans_reset_at` пишется как `NULL`.

```sql
SELECT
  (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
    AS pg_store_plans_reset_at
FROM pg_store_plans_info;
```

### Глобальные представления статистики PostgreSQL

Эти поля нужны для объяснения reset глобальных C-счётчиков. Запросы выполняются
только на версиях PostgreSQL, где соответствующее представление и колонка
существуют.

```sql
SELECT
  (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
    AS pg_stat_bgwriter_reset_at
FROM pg_stat_bgwriter;

-- PG17+
SELECT
  (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
    AS pg_stat_checkpointer_reset_at
FROM pg_stat_checkpointer;

-- PG14+
SELECT
  (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
    AS pg_stat_wal_reset_at
FROM pg_stat_wal;

SELECT
  (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
    AS pg_stat_archiver_reset_at
FROM pg_stat_archiver;

-- PG16+. В pg_stat_io несколько строк, временная метка reset общая; берём MAX.
SELECT
  (EXTRACT(EPOCH FROM MAX(stats_reset)) * 1000000)::bigint
    AS pg_stat_io_reset_at
FROM pg_stat_io;
```

### Как это использует код чтения

Пример: в `pg_stat_statements.calls` было `100000`, стало `20`. Это
отрицательная дельта C-счётчика. Если между точками увеличился
`pg_stat_statements_reset_at`, код чтения разрывает ряд и начинает считать
скорость с новой базы. Если `pg_stat_statements_reset_at` недоступен, ряд всё равно
разрывается, но reset помечается как неподтвержденный метаданными.

Аналогично, если изменился `postmaster_start_time`, код чтения не считает
скорость через границу рестарта ни для одного PostgreSQL C-счётчика.

## `1_015_001` - `1_017_001` replication

Роль инстанса определяется через `pg_is_in_recovery()`.

Standby lag считается с поправкой на idle primary:

- если receive LSN и replay LSN оба не `NULL` и равны, lag равен 0;
- иначе используется `now() - pg_last_xact_replay_timestamp()`.

Без этой поправки на простаивающем primary lag ложно растёт. Если LSN или время
последнего replay неизвестны, lag остаётся `NULL`.

На primary читаются:

- `pg_stat_replication`;
- `pg_replication_slots`.

`retained_bytes` слота считается как:

```sql
pg_current_wal_lsn() - restart_lsn
```
