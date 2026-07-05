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
| `pg_settings` | доступ к `pg_settings`; часть `sourcefile`/`sourceline` может быть скрыта | скрытые поля `NULL` |
| представления репликации | права на `pg_stat_replication`, `pg_replication_slots`; часто нужна роль мониторинга | типы репликации отсутствуют или частично `NULL` |
| PostgreSQL logs | файловый доступ к stderr-логу или доставка sidecar | `1_029_001` фиксирует `source_unavailable`/`unsupported_format`/`permission_denied`/`disabled`/`query_failed`; типизированные строки логов отсутствуют |

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

Форки `pg_store_plans` имеют разные наборы колонок, разные идентичности
записи и разные способы получить текст плана, поэтому им назначены разные
семьи типов: `1_004_001` — vadv 2.x, `1_003_001` — ossc upstream 1.10. Форк
определяется каскадом сигнатурных проб: `to_regprocedure('pg_store_plans(boolean)')`
→ vadv, `to_regprocedure('pg_store_plans()')` → ossc; ни одна не совпала —
источник пропускается с записью в лог.

Сбор `1_004_001` относится к классу A с ограничением: статистика
instance-wide, но SQL-объекты расширения существуют только в базе, где
выполнен `CREATE EXTENSION`. Коллектор ищет источник через `pool.main()` и все
per-db подключения, затем кэширует найденную базу. Чтение идёт отдельным
периодом (`KRONIKA_PG_PLANS_INTERVAL_S`, по умолчанию 300 секунд; пустой
результат повторяется через 30 секунд): секция появляется только в сегменте,
снимок которого выполнил чтение. `ts` строк — момент чтения.

- Шаг 1: `pg_store_plans(false)` без текстов, top-N по `total_time`
  (`KRONIKA_PG_MAX_PLANS`, дефолт 500).
- Шаг 2: текст на строку —
  `pg_store_plans_textplan(pg_store_plans_get_plan(userid, dbid, queryid,
  planid))`, усечение `KRONIKA_PG_MAX_PLAN_TEXT` (дефолт 32768 байт), общий
  бюджет чтения `KRONIKA_PG_PLAN_TEXT_BUDGET` (дефолт 8 MiB); строки после
  бюджета пишутся с `plan = NULL`.

Идентичность строки: `(dbid, userid, planid)` — ровно так расширение ключует
entry: в ключ уходит нулевой query id, поэтому запросы с одинаковой
нормализованной формой плана агрегируются в одну запись. Это свойство форка,
не зависящее от GUC. `queryid_stat_statements` — best-effort атрибуция:
расширение перезаписывает её на каждом выполнении, значение называет
ПОСЛЕДНИЙ запрос, исполнивший план; мост к `1_002` валиден только с этой
оговоркой и остаётся `0` без `compute_query_id = on`. `planid` валиден в
пределах одного инстанса, PostgreSQL major и версии расширения.

Границы стоимости чтения: `KRONIKA_PG_MAX_PLANS` проверяется на старте против
ёмкости секции; `pg_store_plans(false)` под `LIMIT` всё равно сканирует и
сортирует весь набор entries на сервере — лимит ограничивает передачу, не
серверную работу. Текстовая фаза ограничена байтовым бюджетом (лимит выборки
не превышает остаток, добивка по границе UTF-8; серверный `left()` режет
символы, а `pg_store_plans_textplan` строит полный текст до усечения) и
дедлайном; любое ограничение вырождается в `plan = NULL`, строки счётчиков не
теряются. Тексты длиннее 4 KiB интернируются в `dict.blobs` штатной политикой
словаря.

Сброс статистики: у vadv-форка нет `pg_store_plans_info`, момент reset в
данных отсутствует — читатель детектирует его по уменьшению counter-колонок,
как для остальных счётчиков. Расширение relocatable: сбор предполагает, что
схема расширения входит в `search_path` подключений коллектора.

Требования к серверу: `shared_preload_libraries = 'pg_stat_statements,
pg_store_plans'`; `track_io_timing = on`, если нужны ненулевые `blk_*_time`;
`pg_store_plans.track_planning = on`, если нужны `*_plan_time`. Без этих GUC
соответствующие счётчики равны 0, что неотличимо от настоящего нуля. В vadv
2.1 функции `pg_store_plans(boolean)` и `pg_store_plans_get_plan(...)`
исполняемы PUBLIC, поэтому тексты чужих планов доступны любому пользователю.
Для отдельной роли коллектора нужно отозвать PUBLIC-доступ и выдать ей
точечный `GRANT EXECUTE`. Сегменты PGM с этой секцией содержат тексты планов
(имена объектов, предикаты) — как и тексты запросов из `1_002`, это
чувствительный артефакт; сбор текстов отключается `KRONIKA_PG_PLAN_TEXT_BUDGET=0`.

Сбор `1_003_001` (ossc upstream 1.10) отличается по существу: идентичность
записи — `(dbid, userid, queryid, planid)` с настоящим 64-битным core query
id, планы остаются per-query, `queryid` соединяется с `1_002` напрямую и без
оговорок. Без `compute_query_id = on` расширение не записывает entries вовсе.
View отдаёт текст плана сам, поэтому сбор — один запрос с серверным усечением
`left(plan, KRONIKA_PG_MAX_PLAN_TEXT)`; байтовый бюджет применяется к уже
полученным строкам и ограничивает только словарь сегмента, не серверную
работу и не сеть. Тайминги раздельные по классам блоков
(`shared/local/temp_blk_{read,write}_time`); `slow_log_calls` и
`*_plan_time` у upstream отсутствуют. Байтовый контракт тот же: серверный
`left()` режет символы, клиент добивает каждый текст по границе UTF-8 до
`min(лимит, остаток бюджета)`; нулевой бюджет переключает сбор на
numeric-запрос — текст не пересекает сеть. Без `pg_read_all_stats` upstream
маскирует чужие строки: `queryid`/`planid` приходят `NULL` (сборщик
пропускает такие строки и пишет их число в лог), а текст плана заменяется
`<insufficient privilege>` — для полного охвата роли сборщика нужна
`pg_read_all_stats`. У upstream есть `pg_store_plans_info(dealloc,
stats_reset)`; её сбор — follow-up, до него reset детектится по уменьшению
счётчиков, как и у vadv.
Форк определяется каскадом сигнатурных проб: `pg_store_plans(boolean)` →
vadv, `pg_store_plans()` → ossc, ни одна → источник пропускается с записью
в лог.

BDD-образ включает vadv-форк для PostgreSQL 17 и 18, ossc 1.10 — для 15 и 16
(rev обоих закреплены в `flake.nix`): оба пути коллектора получают живой
прогон. Один кластер несёт один форк — файлы расширений одноимённые.

## `1_011_001` / `1_011_002` граф ожиданий lock

Класс A — соединение из `pool.main()`. Отдельного проверочного SQL нет:
коллектор использует свежий снимок `pg_stat_activity`. Если в нём нет строк с
`wait_event_type = 'Lock'`, граф ожиданий не читается и секция в сегмент не
пишется.

Когда свежий activity-снимок содержит lock-waiter, один statement-снимок строит
список waiters, один раз вызывает `pg_blocking_pids(pid)` для каждого waiter,
дедуплицирует `blocked_by`, проверяет guard объёма и только затем запускает
cycle-safe рекурсию. Узлы джойнятся с детерминированным `DISTINCT ON (pid)` по
`pg_locks NOT GRANTED`, чтобы получить тип, режим и цель ожидаемой блокировки.

```text
WITH RECURSIVE
  waiters_raw AS (
    SELECT pid, pg_blocking_pids(pid) AS bp
    FROM pg_stat_activity
    WHERE wait_event_type = 'Lock'
  ),
  waiters AS (
    SELECT pid, dedup(bp) AS bp
    FROM waiters_raw
  ),
  edges AS (SELECT pid AS waiter, b AS blocker FROM waiters, unnest(bp) AS b),
  guard AS (
    SELECT count(waiters) <= :max_rows
       AND count(edges) <= :max_rows
       AND count(nodes) <= :max_rows AS ok
  ),
  primary_tree AS (
    -- real backend roots plus prepared-xact root_pid=0 anchors
  ) CYCLE pid SET is_cycle USING path, -- PG14+; PG10-13: manual path guard
  fallback_tree AS (
    -- rootless/cyclic components not reached from primary_tree
  ) CYCLE pid SET is_cycle USING fallback_path,
  nodes AS (SELECT deterministic_anchor(primary_tree, fallback_tree)),
  waiting_locks AS (
    SELECT DISTINCT ON (pid) *
    FROM pg_locks
    WHERE NOT granted
    ORDER BY pid, locktype, mode, database NULLS FIRST, relation NULLS FIRST,
             page NULLS FIRST, tuple NULLS FIRST, virtualxid NULLS FIRST,
             transactionid NULLS FIRST, classid NULLS FIRST, objid NULLS FIRST,
             objsubid NULLS FIRST, fastpath
  )
SELECT n.pid, n.depth, n.root_pid, /* + backend- и lock-колонки */
       coalesce(w.bp, ARRAY[]::int[]) AS blocked_by
FROM nodes n JOIN pg_stat_activity USING (pid)
LEFT JOIN waiters w ON w.pid = n.pid
LEFT JOIN waiting_locks l ON l.pid = n.pid;
```

Первичные семена рекурсии — backend-корни (блокеры, сами не ждущие) и
prepared-xact якоря `root_pid = 0`. PID `0` не получает синтетическую
`pg_stat_activity`-строку, но waiter за prepared-xact сохраняется с
`blocked_by`, содержащим `0`. Если компонент не достижим из backend-корня или
PID `0`, fallback-обход выбирает детерминированный backend-якорь, поэтому чистые
циклы/окна дедлока не пропадают.

Имя таблицы (`lock_relname`) берётся через `pg_class`, когда отношение видно из
базы подключения коллектора или относится к shared-каталогу
(`pg_locks.database = current_database_oid OR 0`). Блокировки в других базах
кластера дают пустое имя при сохранённых raw-полях `lock_database` и
`lock_relation`.

`KRONIKA_PG_MAX_LOCK_ROWS` (по умолчанию `1000`, минимум `1`) — guard на
waiters, edges и backend nodes, а не финальный `LIMIT`. Если guard срабатывает,
`1_011` в этом снимке не пишется, коллектор логирует waiters/edges/nodes и
продолжает запечатывать остальные секции. Ошибка или timeout lock-query ведут к
такой же деградации только для `1_011`.

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
адаптивным `statement_timeout`. Усечённый сбор помечается строкой coverage
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

`reset_metadata` пишется одной строкой в сегмент, когда источник due по
планировщику или тик форсирован сигналом. Чтение выполняется после сбора
статистических секций.

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
  current_setting('compute_query_id', true) AS compute_query_id,
  current_setting('track_io_timing', true)::bool AS track_io_timing,
  current_setting('track_wal_io_timing', true)::bool AS track_wal_io_timing;
```

Версии расширений не читаются из `pg_extension` текущей базы: это каталог
одной базы, а расширение может стоять в другой базе. Коллектор берёт
`extversion` из кэшей источников секций `1_002` и `1_003`/`1_004` — соединений,
где расширения были найдены и читаются. `NULL` в `ext_*_version` означает
«коллектор не собирает это расширение в этом снапшоте», а не «расширение нигде
не установлено».

`compute_query_id` отсутствует на старых версиях PostgreSQL; в этом случае
`current_setting(..., true)` вернет `NULL`.

### `pg_stat_statements`

`pg_stat_statements_info` доступен только в pgss >= 1.9. Запрос выполняется
на соединении источника секции `1_002` и только если
`to_regclass('pg_stat_statements_info') IS NOT NULL`; иначе поле
`pg_stat_statements_reset_at` пишется как `NULL`. Ошибка чтения info-view
(например, отозванный SELECT) деградирует только это поле, не секцию.

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

## `1_021_001` instance metadata

`instance_metadata` пишется одной строкой, когда источник due по планировщику
или тик форсирован сигналом. Серверная половина читается с главного соединения:

```sql
SELECT current_setting('server_version_num')::int4 AS pg_version_num;

SELECT system_identifier FROM pg_control_system();
```

`pg_control_system()` может быть недоступна роли коллектора; в этом случае
`pg_system_identifier` пишется как `NULL`, остальная строка сохраняется.

Хостовая половина читается локально:

- `hostname` — `/proc/sys/kernel/hostname`;
- `kernel_version` — `/proc/sys/kernel/osrelease`;
- `boot_id` — `/proc/sys/kernel/random/boot_id`;
- `btime` — строка `btime` из `/proc/stat`, секунды переводятся в `unix usec`;
- `clock_ticks_per_sec` — `sysconf(_SC_CLK_TCK)`;
- `page_size_bytes` — `sysconf(_SC_PAGESIZE)`.

`node_self_id` берётся из `KRONIKA_NODE_SELF_ID`; без переменной — hostname.

## Сегмент

Окна сборов накапливаются в журнале `active.parts`, который лежит в
`KRONIKA_OUT_DIR` рядом с готовыми сегментами. Каждое окно синхронизируется на
диск при записи, поэтому падение или рестарт коллектора не теряет собранное:
при старте найденные в журнале окна немедленно запечатываются в сегмент под
`ts` самого раннего из них (возраст открытого сегмента через рестарт не
переносится — он отсчитывается заново).

Коллектор запечатывает сегмент, когда сырой размер журнала достигает
`KRONIKA_SEGMENT_MAX_BYTES` (по умолчанию 64 MiB; `0` возвращает режим
«сегмент на тик») или когда возраст открытого сегмента достигает
`KRONIKA_SEGMENT_MAX_AGE_S` (по умолчанию 900 с, аналог `archive_timeout`).
Проверка возраста выполняется на каждом тике до сбора, поэтому сегмент
закрывается не позже своего максимального возраста и на тиках, где источники
не дали строк или упали. Имя файла берётся из `ts` первого окна. SIGUSR2
запечатывает открытый сегмент немедленно и сохраняет контракт
«сигнал → snapshot → файл». После запечатывания `instance_metadata`,
`reset_metadata` и `pg_settings` снова становятся due, чтобы следующий файл был
самодостаточным.

У самого журнала есть жёсткий потолок `KRONIKA_JOURNAL_MAX_BYTES` (по
умолчанию 1 GiB). Окно, не влезающее в потолок, не отбрасывается: коллектор
сначала запечатывает накопленный сегмент, затем дописывает окно в пустой
журнал — первый фрейм после сброса потолком не ограничен. Если
`KRONIKA_SEGMENT_MAX_BYTES` задан выше журнального потолка, ротацию де-факто
ведёт потолок журнала; коллектор предупреждает об этом на старте.

Каждое запечатывание объявляется строкой `sealed <путь> reason=<причина>`;
причины: `forced` (SIGUSR2), `tick` (режим «сегмент на тик»), `size`, `age`,
`journal-full`, `recovered` (запечатано при старте из восстановленного
журнала).

## Планировщик

Коллектор просыпается внутренним таймером (`KRONIKA_INTERVAL_S`, по умолчанию
5 с; `0` выключает таймер — сбор только по сигналам). Это обычный верхний
предел ожидания: положительный интервал источника или триггера, который
наступает раньше, будит коллектор раньше. На каждом пробуждении таймера
планировщик отдаёт множество источников, чей интервал истёк; остальные не
читаются и их секции в этом сегменте отсутствуют. Первое пробуждение таймера
после старта читает всё — первый сегмент самодостаточен. SIGUSR2 —
форсированный тик: читает все
источники независимо от интервалов; это контракт тестов и отладки.

Интервалы источников задаются env-переменными с дефолтами, перечисленными в
`bins/pg_kronika-collector/src/main.rs`. Интервал `0` означает «читать на
каждом пробуждении таймера» и сам по себе не создаёт горячий цикл.
Положительный интервал короче `KRONIKA_INTERVAL_S` приближает следующее
пробуждение. Граф ожиданий локов
интервала не имеет: он снимается, когда свежий снимок `pg_stat_activity`
содержит бэкенд с `wait_event_type = 'Lock'` — отдельный проверочный запрос
не выполняется. Неудачное чтение источника повторяется через его интервал,
а не на следующем тике. Тик, в котором ни один источник не дал строк, не
добавляет окно в журнал; открытый сегмент при этом всё равно закрывается по
`KRONIKA_SEGMENT_MAX_AGE_S`.

### Бюджет прохода

Стоимость размерных источников (`pg_stat_statements`, таблицы, индексы)
растёт с числом объектов и баз, поэтому кроме интервалов их держит бюджет:
`KRONIKA_CYCLE_DB_BUDGET_MS` (по умолчанию 15 000 мс; `0` выключает)
ограничивает суммарное время БД одного цикла. Остаток проверяется перед
каждым размерным источником в порядке живучести — statements, затем таблицы,
затем индексы, — так что при нехватке первыми откладываются самые дорогие.
Отложенный источник не выбрасывается: планировщик возвращает его на
следующий тик, где он читается уже без оглядки на бюджет — постоянно тесный
бюджет вырождается в чтение через тик, а не в голодание. SIGUSR2 бюджет
обходит. Секций отложенного источника в окне нет (как у не-due), факт
отложения пишется в stderr.

Таблицы и индексы дополнительно разведены по фазам: когда оба due на одном
нефорсированном тике, индексы уступают и читаются на следующем — два самых
дорогих прохода не совпадают. Первый тик после старта — исключение
(самодостаточность первого сегмента), интервал `0` — тоже (явная воля
оператора «каждый тик»).

### Триггерные ускорения

Условия триггеров вычисляются из уже собранных строк — дополнительных
запросов нет. Пока условие держится, источник живёт на ускоренном интервале;
как только очередное чтение его не подтверждает, темп возвращается к
базовому. Положительный ускоренный интервал может разбудить коллектор раньше
`KRONIKA_INTERVAL_S`. Ускоренный интервал, равный базовому или больше,
отключает триггер; `0` означает «каждое пробуждение таймера». Переходы темпа
пишутся в stderr.

- **activity**: бэкенд ждёт heavyweight-лок, либо активных клиентских
  бэкендов не меньше `KRONIKA_PG_ASH_ACTIVE_THRESHOLD` (по умолчанию 20) →
  интервал `KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S` (по умолчанию 1 с).
- **replication**: этот standby или любая реплика отстаёт по replay не меньше
  чем на `KRONIKA_PG_REPL_LAG_TRIGGER_S` (по умолчанию 10 с), либо слот
  удерживает не меньше `KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES` WAL (по
  умолчанию 1 GiB) → интервал `KRONIKA_PG_REPLICATION_FAST_INTERVAL_S`
  (по умолчанию 10 с).

## `1_023_001` coverage

Строка пишется на каждый top-N источник, у которого в этом снимке часть строк
не попала в секцию: `total > collected`, `unknown_total = true`, либо часть
источника пропущена по таймауту или другой ошибке. Источник, уложившийся в
лимиты, строки не получает — пустая секция означает «усечений не было».

- `1_013`/`1_014`: `count(*)` выполняется до тяжёлого top-N запроса по каждой
  базе. Если count успешен, `total` включает эту базу даже при последующем
  timeout top-N. Если count не удался, `unknown_total = true`, `reason = 3`, а
  `total` включает только известную нижнюю границу.
- `1_002`: `total` — `count(*)` на соединении источника расширения; при ошибке
  count пишется строка с `unknown_total = true` и `reason = 3`.
- `1_003`/`1_004`: то же; единственная ось отбора даёт `cutoff_value` —
  минимальный `total_time`, попавший в секцию.

## `1_019_001` `pg_settings`

Полная копия `pg_settings` пишется, когда источник due по планировщику или тик
форсирован сигналом. Чтение идёт с главного соединения одним запросом с
`ORDER BY name`. Сортировка по имени в SQL важна: интернер выдаёт id в порядке
вставки, поэтому сортовой ключ секции `(name)` совпадает с алфавитным порядком
имён.

```sql
SELECT
  name, setting, unit, source, sourcefile, sourceline,
  pending_restart, context, vartype, boot_val, reset_val
FROM pg_settings
ORDER BY name;
```

`setting` хранится как отдаёт сервер — числом в единицах `unit`
(`work_mem = '7539kB'` даёт `setting = 7539`, `unit = kB`). Раскладка стабильна
на PG10-18: `pending_restart` существует с PostgreSQL 9.5.

Происхождение пишется без нормализации: `source` — категория источника из
PostgreSQL (`default`, `configuration file`, `client` и т. п.), `sourcefile` —
путь к файлу конфигурации, если сервер его раскрывает, `sourceline` — номер
строки в этом файле, если сервер его раскрывает. Для нефайловых источников или
скрытых путей `sourcefile` и `sourceline` остаются `NULL`.

Число GUC ожидается небольшим и ограниченным кодом PostgreSQL плюс загруженными
расширениями. Коллектор всё равно проверяет `pg_settings` на
`MAX_SECTION_ROWS` до интернирования строк и падает с ошибкой этой секции, если
снимок не помещается. Строки не усекаются и не отбрасываются.

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

Обе детальные секции (`1_016_001`, `1_017_001`) читаются с главного соединения
на любой роли: на standby `pg_stat_replication` не возвращает walsender-строк,
а у слотов `retained_bytes` остаётся `NULL` — `pg_current_wal_lsn()` там не
определена, и запрос оборачивает её в `CASE WHEN NOT pg_is_in_recovery()`.
LSN-значения насыщаются до `i64::MAX` тем же способом, что в `1_015_001`.

`retained_bytes` слота считается как:

```sql
pg_current_wal_lsn() - restart_lsn
```
