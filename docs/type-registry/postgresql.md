# Класс 1: PostgreSQL

PostgreSQL-источники занимают диапазон `1_001_001` - `1_099_999`.

## Сводная таблица

| `type_id` | Источник | Период | Семантика | Сортировка |
|-----------|----------|----------|-----------|------------|
| `1_001_001` | `pg_stat_activity` (PG 10-12) | базовый шаг | `snapshot_full` | `(ts, pid)` |
| `1_001_002` | `pg_stat_activity` (PG 13) | базовый шаг | `snapshot_full` | `(ts, pid)` |
| `1_001_003` | `pg_stat_activity` (PG 14-18) | базовый шаг | `snapshot_full` | `(ts, pid)` |
| `1_002_001` | `pg_stat_statements` (ext ≤ 1.7, PG 10-12) | 30 с | `snapshot_full` | `(dbid, userid, ts)` |
| `1_002_002` | `pg_stat_statements` (ext 1.8, PG 13) | 30 с | `snapshot_full` | `(dbid, userid, ts)` |
| `1_002_003` | `pg_stat_statements` (ext 1.9, PG 14) | 30 с | `snapshot_full` | `(dbid, userid, ts)` |
| `1_002_004` | `pg_stat_statements` (ext 1.10, PG 15-16) | 30 с | `snapshot_full` | `(dbid, userid, ts)` |
| `1_002_005` | `pg_stat_statements` (ext 1.11, PG 17) | 30 с | `snapshot_full` | `(dbid, userid, ts)` |
| `1_002_006` | `pg_stat_statements` (ext 1.12, PG 18) | 30 с | `snapshot_full` | `(dbid, userid, ts)` |
| `1_003_001` | `pg_store_plans`, форк ossc | 5 мин | `changed` | `(queryid, planid, ts)` |
| `1_004_001` | `pg_store_plans`, форк vadv | 5 мин | `changed` | `(queryid, planid, ts)` |
| `1_005_001` | `pg_stat_database` (PG 10-11) | базовый шаг | `snapshot_full` | `(datid, ts)` |
| `1_005_002` | `pg_stat_database` (PG 12-13) | базовый шаг | `snapshot_full` | `(datid, ts)` |
| `1_005_003` | `pg_stat_database` (PG 14-17) | базовый шаг | `snapshot_full` | `(datid, ts)` |
| `1_005_004` | `pg_stat_database` (PG 18) | базовый шаг | `snapshot_full` | `(datid, ts)` |
| `1_006_001` | `pg_stat_bgwriter` + `pg_stat_checkpointer` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_007_001` | `pg_stat_wal` (PG 14-17) | базовый шаг | `snapshot_full` | `(ts)` |
| `1_007_002` | `pg_stat_wal` (PG 18) | базовый шаг | `snapshot_full` | `(ts)` |
| `1_008_001` | `pg_stat_archiver` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_009_001` | `pg_stat_io` (PG 16-17) | базовый шаг | `snapshot_full` | `(backend_type, object, context, ts)` |
| `1_009_002` | `pg_stat_io` (PG 18) | базовый шаг | `snapshot_full` | `(backend_type, object, context, ts)` |
| `1_010_001` | `pg_prepared_xacts` по базам | базовый шаг | `snapshot_full` | `(datname, ts)` |
| `1_011_001` | `pg_locks`, граф ожиданий (PG 10-13) | по факту | `conditional_full` | `(root_pid, depth, pid)` |
| `1_011_002` | `pg_locks`, граф ожиданий (PG 14-18, +`waitstart`) | по факту | `conditional_full` | `(root_pid, depth, pid)` |
| `1_012_001` | `pg_stat_progress_vacuum` | базовый шаг | `conditional_full` | `(ts, pid)` |
| `1_013_001` | `pg_stat_user_tables` + statio (PG 10-12) | 30 с | `snapshot_full` | `(datid, relid, ts)` |
| `1_013_002` | `pg_stat_user_tables` + statio (PG 13-15) | 30 с | `snapshot_full` | `(datid, relid, ts)` |
| `1_013_003` | `pg_stat_user_tables` + statio (PG 16-17) | 30 с | `snapshot_full` | `(datid, relid, ts)` |
| `1_013_004` | `pg_stat_user_tables` + statio (PG 18) | 30 с | `snapshot_full` | `(datid, relid, ts)` |
| `1_014_001` | `pg_stat_user_indexes` + statio (PG 10-15) | 30 с | `snapshot_full` | `(datid, indexrelid, ts)` |
| `1_014_002` | `pg_stat_user_indexes` + statio (PG 16-18) | 30 с | `snapshot_full` | `(datid, indexrelid, ts)` |
| `1_015_001` | replication: статус инстанса | 30 с | `snapshot_full` | `(ts)` |
| `1_016_001` | replication: реплики primary | 30 с | `snapshot_full` | `(application_name, client_addr, pid, ts)` |
| `1_017_001` | replication: слоты | 30 с | `snapshot_full` | `(slot_name, ts)` |
| `1_018_001` | wraparound | 30 с | `snapshot_full` | `(datname, ts)` |
| `1_019_001` | `pg_settings` | сегмент + 1 ч | `on_change` | `(name)` |
| `1_020_001` | `reset_metadata` | сегмент | `snapshot_full` | `(ts)` |
| `1_021_001` | `instance_metadata` | сегмент | `snapshot_full` | `(ts)` |
| `1_022_001` | log: ошибки, сгруппированные | поток | `event_stream` | `(ts)` |
| `1_023_001` | coverage | 30 с | `snapshot_full` | `(source_type_id, ts)` |
| `1_024_001` | log: checkpoints | поток | `event_stream` | `(ts)` |
| `1_025_001` | log: autovacuum/autoanalyze | поток | `event_stream` | `(ts)` |
| `1_026_001` | log: slow queries | поток | `event_stream` | `(ts)` |
| `1_027_001` | log: lock waits | поток | `event_stream` | `(ts)` |
| `1_028_001` | log: server lifecycle | поток | `event_stream` | `(ts)` |

## `1_001_001` / `1_001_002` / `1_001_003` `pg_stat_activity`

Один из самых горячих типов. Сортировка `time-first` выбрана осознанно: состав
строк между снимками нестабилен, поэтому сортировка по сущности даёт мало
пользы. Внутри одного снимка хорошо сжимаются состояния, типы ожидания, имена
баз и похожие label-колонки.

Снимок берётся целиком, включая фоновые backend'ы (`walwriter`, `checkpointer`,
автовакуум и прочие): у них нет базы, пользователя, состояния и текущего
запроса, поэтому соответствующие колонки `NULL`. `ts` — единое серверное время
снимка (`statement_timestamp()`), одно на все строки. `client_addr` хранится
текстом, пустая строка — локальное соединение. `backend_xid` и `backend_xmin`
хранятся как возраст (`age()`, число транзакций): возраст `backend_xmin` — прямой
сигнал удержания горизонта vacuum и приближения wraparound. Текст запроса идёт
через словарь с усечением на коллекторе по серверному `track_activity_query_size`.

### Версии раскладки

Схема `pg_stat_activity` в диапазоне PG 10–18 менялась дважды, поэтому источник
раскладывается на три версии формата (правило: `type_id` точно характеризует
схему):

| `type_id` | Версии PostgreSQL | Отличие |
|-----------|-------------------|---------|
| `1_001_001` | 10, 11, 12 | базовая раскладка |
| `1_001_002` | 13 | `+ leader_pid` |
| `1_001_003` | 14, 15, 16, 17, 18 | `+ leader_pid`, `+ query_id` |

Live-BDD прогоняется на доступных в nixpkgs мажорах (15–18 — все в `1_001_003`);
раскладки `1_001_001` и `1_001_002` (PG 10–13, вне матрицы) проверяются
golden-кодеками.

### Раскладка `1_001_003` (PG 14–18)

```text
ts                 ts    T
pid                i32   L
leader_pid         i32?  L   // лидер группы параллельных воркеров; NULL вне параллелизма
datname            str?  L   // NULL у фоновых backend
usename            str?  L   // NULL у фоновых backend
application_name   str   L
client_addr        str   L   // текст; пустая строка = local
backend_type       str   L
state              str?  L   // active | idle | idle in transaction | ...; NULL у фоновых
wait_event_type    str?  L   // NULL, если backend не ждёт
wait_event         str?  L
query              str?  L   // через словарь, усечение на коллекторе; NULL у фоновых
query_id           i64?  L   // pg queryid; NULL при compute_query_id=off или без запроса
backend_xid_age    i64?  G   // age(backend_xid); NULL без присвоенного xid
backend_xmin_age   i64?  G   // age(backend_xmin); удержание горизонта vacuum
backend_start      ts    G
xact_start         ts?   G   // NULL вне транзакции
query_start        ts?   G   // NULL у фоновых
state_change       ts?   G   // NULL у фоновых
```

`1_001_002` — та же раскладка без `query_id`. `1_001_001` — без `query_id` и без
`leader_pid`.

Осознанно отложено: 1-секундный ASH-сэмпл активных строк (для AAS и профиля
коротких ожиданий) — отдельный тип; граф ожиданий (`pg_blocking_pids`) — типы
`1_011_001` / `1_011_002`.

## `1_002_001`..`1_002_006` `pg_stat_statements`

`pg_stat_statements` — расширение, а не встроенное представление. Оно отдаёт по
строке на `(userid, dbid, queryid)`, а с версии расширения 1.9 — ещё и на
`toplevel`. `dbid` указывает базу выполнения, поэтому коллектор читает
instance-wide строки одним запросом из базы, где установлено расширение, без
обхода по базам.

Источник кэшируется: сначала проверяется `pool.main()`, затем покрытые соединения
`per_db()`. На ошибке запроса, закрытом соединении, пропавшем расширении или
изменении `extversion` кэш сбрасывается и запускается ограниченное повторное
обнаружение. Поиск источника видит только `pool.main()` и покрытые per-db
соединения; базы вне лимита пула останутся невидимыми до появления явной
настройки исходной БД.

**Раскладка выбирается по версии расширения, а не по мажору сервера.** Расширение
можно закрепить независимо от версии PostgreSQL, поэтому коллектор читает
`extversion` из `pg_extension` (`WHERE extname = 'pg_stat_statements'`) и по нему
выбирает тип. Если расширение не найдено в доступных источниках — секция не
пишется, а в лог выводится строка о пропуске.

### Версии раскладки

| `type_id` | Версия расширения | Сервер | Что добавилось | Столбцов в структуре |
|-----------|-------------------|--------|----------------|----------------------|
| `1_002_001` | ≤ 1.7 | PG 10-12 | базовая раскладка; старые имена времени `total_time`/`min_time`/`max_time`/`mean_time`/`stddev_time`; без planning/WAL/JIT/`toplevel` | 26 |
| `1_002_002` | 1.8 | PG 13 | переименование в `*_exec_time`; `plans`, `total_plan_time` и остальные planning-столбцы; `wal_records`/`wal_fpi`/`wal_bytes` | 35 |
| `1_002_003` | 1.9 | PG 14 | `toplevel` | 36 |
| `1_002_004` | 1.10 | PG 15-16 | `temp_blk_read_time`/`temp_blk_write_time` и восемь JIT-столбцов | 46 |
| `1_002_005` | 1.11 | PG 17 | переименование `blk_*_time` → `shared_blk_*_time`; `local_blk_*_time`; `jit_deform_count`/`jit_deform_time`; `stats_since`/`minmax_stats_since` | 52 |
| `1_002_006` | 1.12 | PG 18 | `wal_buffers_full`; `parallel_workers_to_launch`/`parallel_workers_launched` | 55 |

Число столбцов в структуре = число столбцов представления этой версии плюс три:
`ts`, `datname` (`LEFT JOIN pg_database`) и `usename` (`LEFT JOIN pg_roles`).
Число столбцов представления по версиям: 23 / 32 / 33 / 43 / 49 / 52.

### Сбор

Top-N объединяет две оси: `total_exec_time` (в раскладке `1_002_001` —
`total_time`) и `calls`. Лимит на ось — `KRONIKA_PG_MAX_STATEMENTS`, по умолчанию
500. SQL не применяет пороги и не присваивает уровни важности: коллектор
фиксирует строки с наибольшими значениями по каждой оси и ограничивает объём
вывода.

Текст запроса берётся прямо в `SELECT` как `LEFT(query, 5000)` (фиксированное
усечение, без отдельного дозапроса) и интернируется в словарь сегмента.

### NULL против нуля

- `queryid` — `None`, когда `compute_query_id` выключен; `query` — `None`, когда у
  вызывающего нет прав читать текст чужой роли. Обе колонки nullable по
  контракту.
- planning-столбцы равны `0` (не `NULL`) при выключенном `track_planning`;
  `*_blk_*_time` равны `0` при выключенном `track_io_timing`; JIT-столбцы равны
  `0` без JIT — так их отдаёт PostgreSQL.
- `stats_since`/`minmax_stats_since` присутствуют (не `NULL`) начиная с раскладки
  `1_002_005`.

Времена — `f64`, поэтому структуры выводят `PartialEq`, но не `Eq`. Столбцы
`min/max/mean/stddev` времени исполнения и планирования — класс `g` (производные,
сбрасываемые); всё остальное числовое — класс `c`.

Раскладка `1_002_006` (сверхмножество):

```text
ts                          ts    T
queryid                     i64?  L   // NULL при выключенном compute_query_id
userid                      u32   L
dbid                        u32   L
toplevel                    bool  L   // с ext 1.9
datname                     str?  L   // через словарь; LEFT JOIN pg_database
usename                     str?  L   // через словарь; LEFT JOIN pg_roles
query                       str?  L   // через словарь; LEFT(query, 5000)
calls                       i64   C
rows                        i64   C
plans                       i64   C
total_exec_time             f64   C   // ms
total_plan_time             f64   C   // ms
min_exec_time               f64   G
max_exec_time               f64   G
mean_exec_time              f64   G
stddev_exec_time            f64   G
min_plan_time               f64   G
max_plan_time               f64   G
mean_plan_time              f64   G
stddev_plan_time            f64   G
shared_blks_hit             i64   C
shared_blks_read            i64   C
shared_blks_dirtied         i64   C
shared_blks_written         i64   C
local_blks_hit              i64   C
local_blks_read             i64   C
local_blks_dirtied          i64   C
local_blks_written          i64   C
temp_blks_read              i64   C
temp_blks_written           i64   C
shared_blk_read_time        f64   C   // с ext 1.11; ранее blk_read_time
shared_blk_write_time       f64   C   // с ext 1.11; ранее blk_write_time
local_blk_read_time         f64   C   // с ext 1.11
local_blk_write_time        f64   C   // с ext 1.11
temp_blk_read_time          f64   C   // с ext 1.10
temp_blk_write_time         f64   C   // с ext 1.10
wal_records                 i64   C
wal_fpi                     i64   C
wal_bytes                   i64   C   // numeric в PG, приводится ::int8
wal_buffers_full            i64   C   // с ext 1.12
jit_functions               i64   C   // с ext 1.10
jit_generation_time         f64   C
jit_inlining_count          i64   C
jit_inlining_time           f64   C
jit_optimization_count      i64   C
jit_optimization_time       f64   C
jit_emission_count          i64   C
jit_emission_time           f64   C
jit_deform_count            i64   C   // с ext 1.11
jit_deform_time             f64   C   // с ext 1.11
parallel_workers_to_launch  i64   C   // с ext 1.12
parallel_workers_launched   i64   C   // с ext 1.12
stats_since                 ts?   G   // с ext 1.11; unix-микросекунды
minmax_stats_since          ts?   G   // с ext 1.11; unix-микросекунды
```

## `1_003_001` и `1_004_001` `pg_store_plans`

Есть два несовместимых расширения с одним именем:

- `1_003_001` — `ossc-db/pg_store_plans`;
- `1_004_001` — vadv-форк.

Схемы и способ получения текста плана различаются, поэтому используются разные
`type_id`. Форк определяется при старте по сигнатурам функций.

Собирается top-N по `total_time DESC`, базовый лимит — 500 строк. Для усеченных
сборов пишется coverage `1_023_001`.

Нормализованная раскладка:

```text
ts                              ts    T
queryid                         i64   L
planid                          i64   L
dbid                            u32   L
userid                          u32   L
datname                         str   L
usename                         str   L
plan                            str   L   // текст плана через dict.blobs
calls                           i64   C
total_time                      f64   C
min_time                        f64   G
max_time                        f64   G
mean_time                       f64   G
stddev_time                     f64   G
rows                            i64   C
shared_blks_hit                 i64   C
shared_blks_read                i64   C
shared_blks_dirtied             i64   C
shared_blks_written             i64   C
local_blks_read                 i64   C
local_blks_written              i64   C
temp_blks_read                  i64   C
temp_blks_written               i64   C
blk_read_time                   f64   C   // vadv: shared + local + temp
blk_write_time                  f64   C
first_call                      ts    G
last_call                       ts    G
is_baseline                     bool  L
```

## `1_005_001` / `1_005_002` / `1_005_003` / `1_005_004` `pg_stat_database`

Секция хранит снимок `pg_stat_database` целиком: одна строка на базу. С PG12
представление добавляет агрегатную строку `datid = 0` (shared-объекты кластера)
с `datname = NULL`. `ts` — единое серверное время снимка
(`statement_timestamp()`). `numbackends` — мгновенное число коннектов (gauge).
Раскладка оставляет `numbackends` nullable: документация PostgreSQL описывает
`NULL` для shared-строки, а исходный SQL представления PG12+ возвращает `0`.
`stats_reset` — время последнего сброса статистики этой БД (`NULL`, если не
сбрасывалась).
`blk_read_time` / `blk_write_time` равны нулю без `track_io_timing`.

Дополнительные поля берутся из `pg_database` через `LEFT JOIN` по `oid = datid`:
возрасты wraparound `frozen_xid_age` / `min_mxid_age`, лимит коннектов
`datconnlimit` и флаги `datallowconn` / `datistemplate`. Для строки
shared-объектов (`datid = 0`) соединение не находит строку `pg_database`,
поэтому эти колонки `NULL`. Поля читаются из общих каталогов и не требуют
подключения к целевой базе.

`datconnlimit = -1` означает отсутствие лимита. В PG18 `-2` означает invalid
database. Насыщение лимита считается только при `datallowconn = true`,
`datistemplate = false`, `datconnlimit > 0` и `numbackends IS NOT NULL`:
`numbackends / datconnlimit`. Для остальных строк конечного лимита нет.

`frozen_xid_age` и `min_mxid_age` — разные шкалы. XID-график считает
`max(frozen_xid_age)` по реальным базам; MXID-график считает
`max(min_mxid_age)` по реальным базам. `NULL` shared-строки игнорируется.
Headroom считается отдельно: `autovacuum_freeze_max_age - frozen_xid_age` и
`autovacuum_multixact_freeze_max_age - min_mxid_age`.

### Версии раскладки

В PG 10–18 колонки только добавлялись; удалений и переименований не было:

| `type_id` | Версии PostgreSQL | Отличие |
|-----------|-------------------|---------|
| `1_005_001` | 10, 11 | базовая раскладка |
| `1_005_002` | 12, 13 | `+ checksum_failures`, `+ checksum_last_failure` |
| `1_005_003` | 14, 15, 16, 17 | `+` session-статистика (7 колонок) |
| `1_005_004` | 18 | `+ parallel_workers_to_launch`, `+ parallel_workers_launched` |

Пять `pg_database`-полей (`frozen_xid_age`, `min_mxid_age`, `datconnlimit`,
`datallowconn`, `datistemplate`)
присутствуют во всех раскладках и не зависят от мажорной версии.

BDD покрывает раскладки `1_005_003` и `1_005_004`. `1_005_001` / `1_005_002`
покрыты golden-кодеками.

### Раскладка `1_005_004` (PG 18)

```text
ts                          ts    T
datid                       u32   L
datname                     str?  L   // NULL у строки shared-объектов (datid=0)
numbackends                 i32?  G   // NULL или 0 у строки shared-объектов
xact_commit                 i64   C
xact_rollback               i64   C
blks_read                   i64   C
blks_hit                    i64   C
tup_returned                i64   C
tup_fetched                 i64   C
tup_inserted                i64   C
tup_updated                 i64   C
tup_deleted                 i64   C
conflicts                   i64   C
temp_files                  i64   C
temp_bytes                  i64   C
deadlocks                   i64   C
blk_read_time               f64   C   // 0 без track_io_timing
blk_write_time              f64   C
stats_reset                 ts?   G   // время сброса статистики БД; NULL без сброса
frozen_xid_age              i64?  G   // age(datfrozenxid); NULL у shared-строки
min_mxid_age                i64?  G   // mxid_age(datminmxid); NULL у shared-строки
datconnlimit                i32?  G   // -1 без лимита; PG18 -2 invalid database; NULL у shared
datallowconn                bool? L   // принимает ли БД подключения; NULL у shared
datistemplate               bool? L   // шаблонная ли БД; NULL у shared
checksum_failures           i64   C   // PG12+
checksum_last_failure       ts?   G   // PG12+; NULL, если ошибок не было
session_time                f64   C   // PG14+
active_time                 f64   C
idle_in_transaction_time    f64   C
sessions                    i64   C
sessions_abandoned          i64   C
sessions_fatal              i64   C
sessions_killed             i64   C
parallel_workers_to_launch  i64   C   // PG18+
parallel_workers_launched   i64   C
```

Версионные отличия остаются в хвосте раскладки: `1_005_003` — без двух
parallel-колонок, `1_005_002` — без parallel и session-статистики,
`1_005_001` — только базовые счётчики и `pg_database`-поля.

## `1_006_001` `pg_stat_bgwriter` + `pg_stat_checkpointer`

Синглтон. На PG17+ часть колонок переехала в `pg_stat_checkpointer`; коллектор
склеивает данные обратно в одну строку.

```text
ts                    ts   T
checkpoints_timed     i64  C
checkpoints_req       i64  C
checkpoint_write_time f64  C
checkpoint_sync_time  f64  C
buffers_checkpoint    i64  C
buffers_clean         i64  C
maxwritten_clean      i64  C
buffers_backend       i64? C   // NULL на PG17+
buffers_backend_fsync i64? C   // NULL на PG17+
buffers_alloc         i64  C
```

## `1_007_001` / `1_007_002` `pg_stat_wal`

Синглтон, доступен с PG14. Раскладка `1_007_001` (PG 14-17):

```text
ts               ts   T
wal_records      i64  C
wal_fpi          i64  C
wal_bytes        i64  C   // numeric в PG; cast к i64, overflow = ошибка сбора
wal_buffers_full i64  C
wal_write        i64  C
wal_sync         i64  C
wal_write_time   f64  C   // 0 без track_wal_io_timing
wal_sync_time    f64  C   // 0 без track_wal_io_timing
stats_reset      ts?  G
```

`1_007_002` (PG 18) оставляет `wal_records`, `wal_fpi`, `wal_bytes`,
`wal_buffers_full`, `stats_reset`: write/sync-поля больше не приходят из
`pg_stat_wal`; их продолжает покрывать `pg_stat_io` по строкам `object = wal`.

## `1_008_001` `pg_stat_archiver`

```text
ts                 ts   T
archived_count     i64  C
last_archived_wal  str? L
last_archived_time ts?  G
failed_count       i64  C
last_failed_wal    str? L
last_failed_time   ts?  G
stats_reset        ts?  G
```

Синглтон для PG 10–18. Имена WAL-файлов идут через словарь.
`last_archived_wal` не является границей архивной сохранности: PostgreSQL
предупреждает, что более старые WAL могут ещё не быть заархивированы.

## `1_009_001` / `1_009_002` `pg_stat_io`

PG16+ (на PG 10–15 представления нет — источник пропускается). Сущность — тройка
`(backend_type, object, context)`, обычно 30–50 строк за сбор. Счётчики и
тайминги — `NULL`, а не `0`, для комбинаций, которые backend не порождает (`NULL`
≠ нулевая активность). При выключенном `track_io_timing` тайминги приходят как `0`, а не `NULL` (в PG18
для строк `object = wal` — при `track_wal_io_timing`). `stats_reset` хранится в
строке.

Пропускная способность (байты) считается по-разному на двух раскладках:

- PG16–17 (`1_009_001`): `rate(reads) * op_bytes`, аналогично для `writes` и
  `extends`. `op_bytes` — фиксированный размер блока (обычно 8192), это **gauge,
  а не счётчик**: брать от него `rate` бессмысленно (он почти константа).
- PG18 (`1_009_002`): напрямую `rate(read_bytes)`, `rate(write_bytes)`,
  `rate(extend_bytes)` — `op_bytes` убран, потому что размер I/O-операции стал
  переменным.

Байтовые счётчики PG18 (`*_bytes`) имеют тип `numeric` и приводятся к `i64`:
`i64` вмещает ~8 ЭиБ, чего реальный кластер за свой uptime не достигает. На
теоретическом переполнении сбор упадёт с ошибкой (сегмент потеряется, но
коллектор не запишет искажённое значение) — clamp или nullable не вводим.

### Версии раскладки

Схема `pg_stat_io` менялась в PG18 неаддитивно (добавлены байтовые счётчики,
удалён `op_bytes`), поэтому две версии формата:

| `type_id` | Версии PostgreSQL | Отличие |
|-----------|-------------------|---------|
| `1_009_001` | 16, 17 | `op_bytes`, без байтовых счётчиков |
| `1_009_002` | 18 | `+ read_bytes`, `+ write_bytes`, `+ extend_bytes`; `- op_bytes`; новые `object = wal`, `context = init` |

Live-BDD: PG 16–17 → `1_009_001`, PG 18 → `1_009_002` (обе раскладки в матрице
nixpkgs); на PG 15 секции `pg_stat_io` нет.

### Раскладка `1_009_001` (PG 16–17)

```text
ts              ts    T
backend_type    str   L
object          str   L   // relation | temp relation
context         str   L   // normal | vacuum | bulkread | bulkwrite
reads           i64?  C
read_time       f64?  C
writes          i64?  C
write_time      f64?  C
writebacks      i64?  C
writeback_time  f64?  C
extends         i64?  C
extend_time     f64?  C
op_bytes        i64?  G   // размер блока (8192), не счётчик: байты = (reads+writes+extends)*op_bytes
hits            i64?  C
evictions       i64?  C
reuses          i64?  C
fsyncs          i64?  C
fsync_time      f64?  C
stats_reset     ts?   G
```

### Раскладка `1_009_002` (PG 18)

Без `op_bytes`, с байтовыми счётчиками рядом со счётчиками операций; `object`
получает значение `wal`, `context` — `init`.

```text
ts              ts    T
backend_type    str   L
object          str   L   // relation | temp relation | wal
context         str   L   // normal | vacuum | bulkread | bulkwrite | init
reads           i64?  C
read_bytes      i64?  C
read_time       f64?  C
writes          i64?  C
write_bytes     i64?  C
write_time      f64?  C
writebacks      i64?  C
writeback_time  f64?  C
extends         i64?  C
extend_bytes    i64?  C
extend_time     f64?  C
hits            i64?  C
evictions       i64?  C
reuses          i64?  C
fsyncs          i64?  C
fsync_time      f64?  C
stats_reset     ts?   G
```

## `1_010_001` `pg_prepared_xacts` по базам

```text
ts              ts   T
datname         str  L   // база, где висят prepared-транзакции
prepared_count  i64  G   // число prepared-транзакций в базе
max_age_us      i64  G   // wall-clock возраст старейшей prepared-транзакции, микросекунды
max_xid_age_tx  i64  G   // максимальный age(transaction), транзакции
```

Одна строка на базу с prepared-транзакциями (двухфазный коммит), `GROUP BY
database`. Если prepared-транзакций нет, секция отсутствует; это означает ноль
prepared-транзакций, а не ошибку сбора. По умолчанию
`max_prepared_transactions = 0`, и 2PC выключен. Забытый 2PC удерживает горизонт
xmin и блокирует vacuum в своей базе, поэтому `datname` обязателен.

`prepared_count` — размер группы. `max_age_us` — wall-clock возраст старейшей
prepared-транзакции в микросекундах, рассчитанный от `clock_timestamp()` и
зажатый снизу нулём. `max_xid_age_tx` — максимальный `age(transaction)` в
транзакциях; это XID-сигнал удержания горизонта. `pg_prepared_xacts` при чтении
кратко блокирует и копирует состояние transaction manager; reset-семантики у
источника нет.

Детализация по транзакциям (`gid`, `owner`, `transaction`) может стать отдельным
типом, если понадобится.

## `1_011_001` / `1_011_002` `pg_locks`, граф ожиданий

`conditional_full`. Секция пишется только при наличии ожидающих блокировок.
Отсутствие секции означает пустой граф ожиданий в этом окне сбора — не
неизвестное состояние.

Секция node-центрична: каждый backend, участвующий в блокирующем компоненте,
даёт одну строку. Направленные рёбра графа хранятся в колонке `blocked_by`
(`list<i32>` — дедуплицированный результат `pg_blocking_pids(pid)`). У корней
`blocked_by` пуст. `depth` — расстояние от корня (кратчайший путь); это
вспомогательный скаляр, авторитетна именно `blocked_by`. `root_pid` указывает,
к какому корню привязан данный узел блокирующего компонента.

PID `0` в `blocked_by` — опаковый маркер двухфазной транзакции: `pg_locks`
видит блокирующий хранитель строки, но без `pg_stat_activity`-строки.

### Версии раскладки

Раскладка разделена на две версии из-за появления `waitstart` в `pg_locks` в
PG 14. Единственное отличие — наличие или отсутствие этой колонки в хвосте;
всё остальное идентично.

| `type_id` | Версии PostgreSQL | Отличие | Столбцов |
|-----------|-------------------|---------|---------|
| `1_011_001` | 10, 11, 12, 13 | базовая раскладка | 31 |
| `1_011_002` | 14, 15, 16, 17, 18 | `+ waitstart` | 32 |

Live-BDD охватывает PG 14-18 (раскладка `1_011_002`, все в матрице nixpkgs);
раскладка `1_011_001` (PG 10-13, вне матрицы) покрыта golden-кодеком.

### Что попадает в секцию (scope IN)

- Тяжёлые блокировки (`relation`, `extend`, `object`).
- Блокировки строк через `transactionid` — стандартный паттерн row-lock.
- Рекомендательные блокировки (`advisory`).

### Что не попадает в секцию (scope OUT)

- LWLock и BufferPin — события внутреннего синхронизатора PostgreSQL; они
  фиксируются в типе `1_001` через `wait_event_type = 'LWLock'` /
  `'BufferPin'`, но не дают строк `pg_locks NOT GRANTED`.
- SIReadLock — предикатная блокировка SSI; она никогда не блокирует другой
  backend напрямую.

### Дедлоки

Коллектор работает с циклами в графе безопасно: рекурсивный CTE защищён от
бесконечного обхода. Дедлоки видны в этой секции через взаимный `blocked_by`,
однако их разрешение PostgreSQL фиксирует в логе (тип `1_027_001`). Коллектор
записывает состояние до или после разрешения — в зависимости от момента снимка.

### Ограничение `lock_relname`

`lock_relname` (имя таблицы для relation/page/tuple-блокировок) разрешается
только через системный каталог базы, к которой подключён коллектор. Блокировка
в другой базе того же кластера даст `lock_relname = NULL`; `lock_relation` (oid)
при этом остаётся.

### Примечание для кода чтения

Построение дерева/графа с нодами, окрашивание путей по типу ожидания,
определение тупиков и вынесение вердиктов (`«критическая цепочка»`,
`«дедлок»`) — ответственность read-side модуля анализа, а не коллектора.
Коллектор записывает топологию графа как есть.

### Раскладка `1_011_002` (PG 14-18)

```text
ts                    ts        T
pid                   i32       L   // backend PID
blocked_by            list<i32> L   // pg_blocking_pids(pid), дедуплицировано; пуст у корней
depth                 i32       G   // расстояние от корня; 0 у корней
root_pid              i32       L   // PID корня этого компонента
datid                 u32       L   // OID базы
datname               str       L   // имя базы
usename               str?      L   // NULL у некоторых фоновых backend
application_name      str       L
client_addr           str       L   // текст; пустая строка = local
backend_type          str       L
state                 str?      L   // active | idle | ...; NULL у фоновых
wait_event_type       str?      L   // NULL у корней-не-ждущих
wait_event            str?      L
query                 str       L   // через словарь, усечение на коллекторе
backend_xid_age       i64?      G   // age(backend_xid); NULL без xid
backend_xmin_age      i64?      G   // age(backend_xmin); удержание горизонта vacuum
backend_start         ts?       G   // время старта backend
xact_start            ts?       G   // NULL вне транзакции
query_start           ts?       G   // NULL у некоторых фоновых
state_change          ts?       G   // NULL у некоторых фоновых
lock_locktype         str?      L   // тип ожидаемой блокировки; NULL у корней
lock_mode             str?      L   // режим ожидаемой блокировки; NULL у корней
lock_granted          bool?     L   // всегда false для ожидаемой строки; NULL у корней
lock_relation         u32?      L   // OID отношения (relation/page/tuple/extend); NULL иначе
lock_relname          str?      L   // имя отношения; только для базы коллектора
lock_page             i32?      G   // номер страницы (page/tuple lock); NULL иначе
lock_tuple            i16?      G   // смещение кортежа (tuple lock); NULL иначе
lock_transactionid    i64?      L   // ожидаемый XID (row-lock паттерн); NULL иначе
lock_fastpath         bool?     L   // fast-path блокировка; NULL у корней
lock_target           str?      L   // человекочитаемая цель, best effort
waitstart             ts?       G   // начало ожидания блокировки (PG14+); NULL у корней
```

`1_011_001` — та же раскладка без `waitstart` (31 колонка).

## `1_012_001` `pg_stat_progress_vacuum`

`conditional_full`. Секция пишется только когда `pg_stat_progress_vacuum`
содержит строки. Отсутствие секции означает ноль активных `VACUUM` в момент
снимка, а не ошибку сбора.

```text
ts                    ts   T
pid                   i32  L
datid                 u32  L
datname               str  L
relid                 u32  L
is_autovacuum         bool L
phase                 str  L
heap_blks_total       i64  G   // блоки heap
heap_blks_scanned     i64  G   // блоки heap
heap_blks_vacuumed    i64  G   // блоки heap
index_vacuum_count    i64  G
max_dead_tuples       i64? G   // PG10-16, tuples
num_dead_tuples       i64? G   // PG10-16, tuples
max_dead_tuple_bytes  i64? G   // PG17+
dead_tuple_bytes      i64? G   // PG17+
num_dead_item_ids     i64? G   // PG17+
indexes_total         i64? G   // PG17+
indexes_processed     i64? G   // PG17+
delay_time            f64? G   // PG18+
```

Одна строка на backend, выполняющий `VACUUM`, включая autovacuum. `VACUUM FULL`
сюда не попадает. `is_autovacuum` вычисляется по `pg_stat_activity.backend_type =
'autovacuum worker'`; ручной `VACUUM` хранится с `is_autovacuum = false`.

PG17 заменил `max_dead_tuples` / `num_dead_tuples` на
`max_dead_tuple_bytes`, `dead_tuple_bytes` и `num_dead_item_ids`; единицы
измерения разные, поэтому поля не объединяются. PG17 также добавил прогресс по
индексам, PG18 — `delay_time`.

`datid` нужен вместе с `datname`: `relid` локален в базе, а имя базы может быть
переименовано. Связь с `pg_stat_activity` идёт по `pid` внутри того же снимка.
`heap_blks_scanned` / `heap_blks_vacuumed` монотонны внутри одного vacuum, но
класс `G`: между запусками сбрасываются.

## `1_013_001`..`1_013_004` `pg_stat_user_tables` + `pg_statio_user_tables`

Собирается отдельно по каждой базе через пул соединений (один коннект на базу,
обновление пула раз в 10 минут, env `KRONIKA_PG_POOL_REFRESH_SECS`). Коллектор
всегда перечисляет базы и ходит по каждой — режима одной базы через `PGDATABASE`
нет: `dbname` из DSN задаёт лишь стартовый коннект, а per-db соединения всё равно
открываются по всем базам. Тяжёлый запрос идёт под адаптивным `statement_timeout`
(старт 15 с, ×2 до `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` = 60 с; SQLSTATE 57014 —
расширить и повторить базу, иначе пропустить).

Четыре версии по росту каталога: `n_ins_since_vacuum` появился в PG13 (V2),
`n_tup_newpage_upd` и `last_seq_scan`/`last_idx_scan` — в PG16 (V3), четыре
кумулятивных тайминга вакуума/анализа (`total_vacuum_time`,
`total_autovacuum_time`, `total_analyze_time`, `total_autoanalyze_time`,
`double precision`, миллисекунды) — в PG18 (V4).

Отбор кандидатов — чисто механический: объединение top-N таблиц по сырым
колонкам (N по умолчанию 500, env `KRONIKA_PG_MAX_TABLES`):

- активность: `seq_scan + idx_scan + n_tup_ins/upd/del` (PG16+ — `GREATEST(last_seq_scan, last_idx_scan)`);
- запись: `n_tup_ins + n_tup_upd + n_tup_del` DESC — на PG16+ ось активности это давность чтения, и таблица только под запись иначе теряется;
- размер: `pg_class.relpages`;
- dead tuples: `n_dead_tup`;
- xid-возраст: `age(relfrozenxid)` DESC — старые таблицы могут попасть в выборку без активности пользовательских запросов;
- multixact-возраст: `mxid_age(relminmxid)` DESC.

Коллектор пишет крайние строки по каждой оси и ограничивает свой объём; оценку
«опасно или нет» делает модуль анализа на чтении — порогов и вердиктов в
коллекторе нет.

`pg_statio_user_tables` сливается в строку через `LEFT JOIN` по `relid`;
`xid_age`/`mxid_age`/`reltuples` берутся из `pg_class` тем же запросом. `datid`
нужен вместе с `datname` как стабильный числовой ключ (базу могут переименовать)
и для join к `pg_stat_database`. `NULL` означает «нет индексов» (`idx_*`), «нет
TOAST» (`toast_*`) или «события не было» (`last_*`) — не ноль.
`heap_blks_read`/`heap_blks_hit` — счётчики shared-буферов PostgreSQL (попадание
в кэш буферов против промаха), а не I/O блочного устройства или ОС: `read` здесь
значит «не нашлось в shared buffers», а не «прочитано с диска».

Семантика `snapshot_full`: каждый цикл отдаёт все отобранные строки. Ряд
разрежённый — отсутствие таблицы в снимке означает, что она не попала в top-N, а
не ноль; сбросы статистики по всей базе видны через `pg_stat_database.stats_reset`
(`1_005`) по join на `datid`. `changed`-семантика потребует отдельного
delta-контракта, поэтому этот тип её не использует.

Раскладка V4 (надмножество; V3 без четырёх `total_*_time`, V2 ещё и без
`n_tup_newpage_upd`/`last_seq_scan`/`last_idx_scan`, V1 ещё и без
`n_ins_since_vacuum`):

```text
ts                              ts    T
datid                           u32   L
datname                         str   L
relid                           u32   L
schemaname                      str   L
relname                         str   L
tablespace                      str   L
seq_scan                        i64   C
seq_tup_read                    i64   C
idx_scan                        i64?  C
idx_tup_fetch                   i64?  C
n_tup_ins                       i64   C
n_tup_upd                       i64   C
n_tup_del                       i64   C
n_tup_hot_upd                   i64   C
n_tup_newpage_upd               i64   C
n_live_tup                      i64   G
n_dead_tup                      i64   G   // dead tuples (мёртвые кортежи)
n_mod_since_analyze             i64   G
n_ins_since_vacuum              i64   G
vacuum_count                    i64   C
autovacuum_count                i64   C
analyze_count                   i64   C
autoanalyze_count               i64   C
last_vacuum                     ts?   G
last_autovacuum                 ts?   G
last_analyze                    ts?   G
last_autoanalyze                ts?   G
last_seq_scan                   ts?   G
last_idx_scan                   ts?   G
total_vacuum_time               f64   C   // PG18+, мс
total_autovacuum_time           f64   C   // PG18+, мс
total_analyze_time              f64   C   // PG18+, мс
total_autoanalyze_time          f64   C   // PG18+, мс
main_fork_bytes                 i64   G   // pg_relation_size, только main fork
toast_bytes                     i64?  G
toast_n_live_tup                i64?  G
toast_n_dead_tup                i64?  G
toast_last_autovacuum           ts?   G
xid_age                         i64   G
mxid_age                        i64   G
reltuples                       i64   G
heap_blks_read                  i64   C   // shared-буферы: промах, не диск/ОС
heap_blks_hit                   i64   C   // shared-буферы: попадание
idx_blks_read                   i64?  C
idx_blks_hit                    i64?  C
toast_blks_read                 i64?  C
toast_blks_hit                  i64?  C
tidx_blks_read                  i64?  C
tidx_blks_hit                   i64?  C
```

## `1_014_001`..`1_014_002` `pg_stat_user_indexes` + `pg_statio_user_indexes`

Секция соседствует с `1_013` (`pg_stat_user_tables`): та же схема сбора по каждой базе
через пул соединений (один коннект на базу, обновление пула раз в 10 минут, env
`KRONIKA_PG_POOL_REFRESH_SECS`). Коллектор всегда ходит по всем базам — режима
одной базы через `PGDATABASE` нет. Запрос идёт под тем же адаптивным
`statement_timeout` (старт 15 с, ×2 до `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` = 60 с;
SQLSTATE 57014 — расширить и повторить базу, иначе пропустить).

Две версии по росту каталога: `last_idx_scan` появился в PG16 (V2). PG17 и PG18
в `pg_stat_all_indexes` не добавили ничего — в отличие от таблиц, у индексов нет
колонок тайминга.

Отбор кандидатов — чисто механический: объединение top-N индексов по сырым
колонкам (N по умолчанию 500, свой env `KRONIKA_PG_MAX_INDEXES` — отдельно от
`KRONIKA_PG_MAX_TABLES`). Каждая ось добавляет `indexrelid` последним ключом
`ORDER BY`, чтобы top-N был детерминированным при равных значениях:

- `idx_scan` DESC, `indexrelid`;
- `idx_tup_read` DESC, `indexrelid`;
- размер: `pg_class.relpages` DESC, `indexrelid` (JOIN по `indexrelid`) — ловит
  большие индексы, в том числе неиспользуемые;
- PG16+: `last_idx_scan` DESC, `indexrelid` c `WHERE last_idx_scan IS NOT NULL` —
  ось давности не забивается ни разу не сканированными индексами (те всё равно
  попадают через ось `relpages`/размера).

Коллектор не фильтрует `WHERE idx_scan = 0`: это был бы вердикт. Он пишет
`relpages` (через размер) и `idx_scan`, а «неиспользуемый большой индекс»
выводит модуль анализа на чтении.

Стартовая валидация: `DEFAULT_MAX_DATABASES * оси * N` для tables и indexes
должно укладываться в лимит строк одной секции (`MAX_SECTION_ROWS` = 65536), иначе
коллектор падает на старте с явной ошибкой — иначе переполнение всплыло бы только
при запечатывании и потеряло бы весь сегмент.

`pg_statio_user_indexes` сливается в строку через `LEFT JOIN` по `indexrelid`;
`idx_blks_read`/`idx_blks_hit` берутся под `COALESCE(..., 0)` — гонка между
каталогом и статистикой не даст `NULL`, который иначе уронил бы декодирование в
`i64`. `main_fork_bytes` = `pg_relation_size(indexrelid)` (только main fork, без
TOAST — у индекса его нет); `amname` берётся из `pg_am` через `pg_class.relam`;
флаги `indisunique`/`indisprimary`/`indisvalid`/`indisexclusion`/`indisready` — из
`pg_index` (`indisexclusion` даёт отличить индекс exclusion-ограничения от
обычного неиспользуемого; `indisready` — готовность к вставкам); определение —
`left(pg_get_indexdef(indexrelid), 5000)`: текст обрезается на уровне SQL до
материализации (partial-индекс с большим выражением иначе не ограничен). `datid`
нужен вместе с `datname` как стабильный числовой ключ (базу могут переименовать) и
для join к `pg_stat_database`. `last_idx_scan` = `NULL` означает «ни разу не
сканировали» — не ноль. `idx_blks_read`/`idx_blks_hit` — счётчики shared-буферов
PostgreSQL (попадание в кэш буферов против промаха), а не I/O блочного устройства
или ОС.

Семантика `snapshot_full` (как у `1_013`): каждый цикл отдаёт все отобранные
строки. Ряд разрежённый — отсутствие индекса в снимке означает, что он не попал
в top-N, а не ноль; сбросы статистики по всей базе видны через
`pg_stat_database.stats_reset` (`1_005`) по join на `datid`. `changed`-семантика
потребует отдельного delta-контракта, поэтому этот тип её не использует.

Раскладка V2 (надмножество; V1 без `last_idx_scan`):

```text
ts             ts    T
datid          u32   L
datname        str   L
indexrelid     u32   L
relid          u32   L
schemaname     str   L
relname        str   L
indexrelname   str   L
tablespace     str   L
idx_scan       i64   C
idx_tup_read   i64   C
idx_tup_fetch  i64   C
main_fork_bytes i64  G   // pg_relation_size(indexrelid)
last_idx_scan  ts?   G   // PG16+, NULL = ни разу
indisunique    bool  L
indisprimary   bool  L
indisvalid     bool  L
indisexclusion bool  L   // индекс exclusion-ограничения
indisready     bool  L   // готов к вставкам
amname         str   L
indexdef       str   L   // left(pg_get_indexdef, 5000)
idx_blks_read  i64   C   // shared-буферы: промах, не диск/ОС
idx_blks_hit   i64   C   // shared-буферы: попадание
```

## `1_015_001` - `1_017_001` replication

Старая вложенная структура разложена на три плоских типа. Это упрощает код
чтения и снижает зависимость от поддержки Parquet nested-схем.

### `1_015_001` статус инстанса

Тип описывает роль инстанса, его синхронные настройки, upstream WAL receiver и
позицию применения на standby. Это не источник для удержания WAL: слоты,
retained bytes и детализация по репликам остаются в `1_016_001` и `1_017_001`.
Текстовые поля обрезаются в SQL до 4096 байт до интернирования.

```text
ts                         ts    T
is_in_recovery             bool  G
timeline_id                i32   G
synchronous_standby_names  str   L
synchronous_commit         str   L
wal_receiver_status        str?  L   // pg_stat_wal_receiver.status
sender_host                str?  L   // upstream host standby
sender_port                i32?  G
slot_name                  str?  L
streaming_replicas         i32   G   // pg_stat_replication rows with state='streaming'
replay_lag_s               i64?  G
standby_receive_lsn        i64?  G   // signed byte offset from 0/0
standby_replay_lsn         i64?  G
standby_last_replay_at     ts?   G
current_wal_lsn            i64?  G   // pg_current_wal_lsn(), WAL write location
latest_end_lsn             i64?  G
latest_end_time            ts?   G
received_tli               i32?  G
```

Для standby `replay_lag_s = 0`, только если receive LSN и replay LSN оба известны
и равны. Если LSN или timestamp применения неизвестны, значение остаётся `NULL`.
`sender_host` — из `pg_stat_wal_receiver.sender_host` на PG11+; на PG10 из
байтово ограниченного `conninfo` извлекается `host`, при отсутствии — `hostaddr`.
Сырой `conninfo` не хранится. LSN-смещения хранятся как `i64`; если PostgreSQL
вернул значение выше `i64::MAX`, оно насыщается до `i64::MAX`.

### `1_016_001` реплики primary

```text
ts                ts    T
pid               i32   L
usename           str   L
application_name  str   L
client_addr       str?  L
state             str   L
sync_state        str   L
sent_lsn          i64   G
write_lsn         i64   G
flush_lsn         i64   G
replay_lsn        i64   G
write_lag_us      i64?  G
flush_lag_us      i64?  G
replay_lag_us     i64?  G
```

### `1_017_001` replication slots

```text
ts                  ts    T
slot_name           str   L
plugin              str?  L
slot_type           str   L
active              bool  G
restart_lsn         i64   G
confirmed_flush_lsn i64?  G
retained_bytes      i64?  G
wal_status          str   G   // reserved | extended | lost, PG13+
```

## `1_018_001` wraparound

```text
ts       ts   T
datname  str  L
age      i64  G   // age(datfrozenxid)
```

## `1_019_001` `pg_settings`

`on_change`, политика материализации `every_segment_last_known`. Около 350 строк
и около 11 КБ на снимок. Источник пишется при старте, при изменении и один раз
в каждый сегмент как актуальная копия. Это сохраняет самодостаточность сегмента:
коду чтения не нужно искать настройки в предыдущих сегментах.

```text
ts              ts    T
name            str   L
setting         str   L
unit            str?  L
source          str   L
sourcefile      str?  L
sourceline      i32?  L
pending_restart bool  L
context         str   L
vartype         str   L
boot_val        str   L
reset_val       str   L
```

## `1_020_001` `reset_metadata`

Служебная секция, обязательная в каждом сегменте. Это не метрика для графика,
а контекст для механизма сравнения: код чтения использует её, чтобы отличать
настоящий reset счётчиков от потери данных, переполнения или ошибки кода
записи.

Зачем это нужно:

- разрывать вычисление скоростей на границах рестарта PostgreSQL;
- объяснять отрицательные дельты C-счётчиков после `pg_stat_reset()`,
  `pg_stat_statements_reset()` и аналогичных reset-функций;
- понимать, доступны ли `pg_stat_statements` и `pg_store_plans`, и какой
  версии их схемы;
- корректно интерпретировать `query_id` и IO-time колонки, зависящие от GUC.

Секция содержит одну строку на сегмент. `ts` — время чтения метаданных. Все поля
типа `ts` хранятся в `i64 unix usec`. Если источник отдаёт
`timestamp with time zone`, коллектор должен умножить `EXTRACT(EPOCH)` на
`1_000_000`.

```text
ts                             ts    T
postmaster_start_time          ts    G
pg_stat_database_reset_max_at  ts    G
pg_stat_statements_reset_at    ts?   G
pg_store_plans_reset_at        ts?   G
pg_stat_bgwriter_reset_at      ts?   G
pg_stat_checkpointer_reset_at  ts?   G
pg_stat_wal_reset_at           ts?   G
pg_stat_archiver_reset_at      ts?   G
pg_stat_io_reset_at            ts?   G
ext_pg_stat_statements_version str?  L
ext_pg_store_plans_version     str?  L
compute_query_id               str?  L
track_io_timing                bool? L
```

Семантика полей:

| Поле | Значение |
|------|----------|
| `postmaster_start_time` | время старта postmaster; изменение означает рестарт PostgreSQL |
| `pg_stat_database_reset_max_at` | максимум `stats_reset` из `pg_stat_database`; грубый маркер reset на уровне базы |
| `pg_stat_statements_reset_at` | reset `pg_stat_statements`; `NULL`, если расширение или `pg_stat_statements_info` недоступны |
| `pg_store_plans_reset_at` | reset `pg_store_plans`; `NULL`, если расширение, информационное представление/функция или форк этого не поддерживает |
| `pg_stat_bgwriter_reset_at` | reset bgwriter-статистики; `NULL`, если представление или поле недоступны |
| `pg_stat_checkpointer_reset_at` | reset checkpointer-статистики; `NULL` до PG17 |
| `pg_stat_wal_reset_at` | reset WAL-статистики; `NULL` до появления `pg_stat_wal` |
| `pg_stat_archiver_reset_at` | reset archiver-статистики; `NULL`, если сервер не вернул время сброса |
| `pg_stat_io_reset_at` | reset `pg_stat_io`; `NULL` до PG16 |
| `ext_pg_stat_statements_version` | версия расширения или `NULL`, если расширение не установлено в доступных БД |
| `ext_pg_store_plans_version` | версия расширения или `NULL` |
| `compute_query_id` | значение GUC; при `off`/`NULL` `query_id` нельзя считать надежным ключом |
| `track_io_timing` | если `false`, `blk_*_time` остаются нулевыми и не означают «быстрый IO»; `NULL`, если GUC недоступен |

Правила для кода чтения:

- Если `postmaster_start_time` изменился, все PostgreSQL C-счётчики считаются
  начавшимися заново.
- Если любой `*_reset_at` увеличился между соседними точками, скорость для
  связанных C-счётчиков не считается через эту границу.
- Если C-счётчик дал отрицательную дельту, а подходящий `*_reset_at` не
  изменился или недоступен, код чтения всё равно должен разорвать ряд и пометить
  reset как неподтвержденный.
- `NULL` в `*_reset_at` означает «источник недоступен», а не unix epoch.
- `pg_stat_database_reset_max_at` — грубый маркер. Он подтверждает, что был
  reset статистики на уровне базы, но не говорит, какая именно база сброшена.
  Если понадобится точная атрибуция, будущая версия должна добавить
  `stats_reset` по каждой базе в `1_005_001` или отдельный служебный тип.

Соответствие reset-полей типам:

| Типы | Reset-поле |
|------|------------|
| `1_002_001` | `pg_stat_statements_reset_at` |
| `1_003_001`, `1_004_001` | `pg_store_plans_reset_at` |
| `1_005_001` | `pg_stat_database_reset_max_at` |
| `1_006_001` | `pg_stat_bgwriter_reset_at`, `pg_stat_checkpointer_reset_at` |
| `1_007_001`, `1_007_002` | `pg_stat_wal_reset_at` |
| `1_008_001` | `pg_stat_archiver_reset_at` |
| `1_009_001`, `1_009_002` | `pg_stat_io_reset_at` |
| все PostgreSQL C-счётчики | `postmaster_start_time` |

## `1_021_001` `instance_metadata`

Служебная секция, обязательная в каждом сегменте с PostgreSQL или OS-снимками.

```text
ts                    ts    T
hostname              str   L
node_self_id          str   L
pg_version_num        i32   L
kernel_version        str   L
pg_system_identifier  i64   L
clock_ticks_per_sec   i64   L   // sysconf(_SC_CLK_TCK), нужно для ticks
page_size_bytes       i64   L
boot_id               str   L   // /proc/sys/kernel/random/boot_id
btime                 ts    L   // /proc/stat btime
```

`pg_system_identifier` переживает рестарты и меняется при `initdb`.
`clock_ticks_per_sec`, `page_size_bytes`, `boot_id` и `btime` делают OS-секции
самодостаточными: код чтения не должен знать эти значения из внешней
конфигурации.

## Логовые типы `1_022_001`, `1_024_001` - `1_028_001`

Конвейер чтения логов общий для всех логовых типов:

- читатель логов отслеживает ротацию по inode и усечение файла;
- лимиты чтения: 10 000 строк за проход, 64 КБ на строку;
- поддерживаются `stderr` и `csvlog`;
- `stderr` парсится поиском по ключевым словам и не требует фиксированного
  `log_line_prefix`;
- поддерживаются английская и русская локали PostgreSQL;
- строки продолжения, начинающиеся с пробела или таба, присоединяются к
  предыдущему событию.

Читатель логов хранит устойчивую позицию чтения отдельно от PGM-сегмента:

```text
path
dev
inode
offset
last_ts
parser_kind
partial_event_state
```

Позиция чтения обновляется после успешной записи мини-сегмента. При `copytruncate`,
ротации через переименование или смене inode читатель логов должен либо
продолжить с сохранённого offset, либо сгенерировать `collector_gap`, если часть лога могла
быть потеряна. При перегрузке нельзя бесконечно копить строки в памяти: после
превышения лимита строки отбрасываются, а коллектор пишет диагностическое
событие и счётчик отброшенных строк.

Каждый тип `1_024_001` - `1_028_001` порождает тонкое событие в таймлайне
класса 2. Интерфейс получает метку из хвоста сегмента, а детали читает из
типизированной секции.

### `1_022_001` log: ошибки

Ошибки группируются по нормализованному шаблону (`pattern`): кавычки, числа и
скобки заменяются на `...`, длина шаблона ограничена 256 символами. `STATEMENT`
после ошибки даёт SQL.

Окно агрегации ошибок — интервал сброса мини-сегмента. Внутри окна строки с
одинаковым `(severity, sqlstate, pattern)` схлопываются в одну с суммарным
`count`; `message` и `statement` берутся из первого экземпляра.

```text
ts          ts    T
severity    u8    L   // WARNING | ERROR | FATAL | PANIC
sqlstate    str   L
pattern     str   L
message     str   L   // первый сырой экземпляр в окне агрегации
statement   str?  L
count       u32   G   // повторов шаблона за окно агрегации
```

### `1_024_001` log: checkpoints

Из строк `checkpoint starting` и `checkpoint complete`, включая русскую
локаль. Для `checkpoint complete` строка продолжения не требуется: все данные
в одной строке.

```text
ts               ts   T
kind             u8   L   // 0=starting 1=complete 2=too_frequent
reason           str? L
buffers_written  i64? G
write_time_ms    f64? G
sync_time_ms     f64? G
total_time_ms    f64? G
distance_kb      i64? G
estimate_kb      i64? G
wal_added        i64? G
wal_removed      i64? G
wal_recycled     i64? G
sync_files       i64? G
longest_sync_s   f64? G
average_sync_s   f64? G
interval_s       i64? G
```

Дублирование с `1_006_001` намеренное: счётчики дают агрегат, лог даёт
детализацию по отдельному checkpoint.

### `1_025_001` log: autovacuum / autoanalyze

Из строк `automatic vacuum/analyze of table ...` и строк продолжения с
buffer usage, rates, system usage, tuples, pages и WAL usage.

```text
ts                 ts    T
is_analyze         bool  L
table_name         str   L
elapsed_s          f64   G
tuples_removed     i64   G
pages_removed      i64   G
buffer_hits        i64   G
buffer_misses      i64   G
buffer_dirtied     i64   G
avg_read_rate_mbs  f64   G
avg_write_rate_mbs f64   G
cpu_user_s         f64   G
cpu_system_s       f64   G
wal_records        i64   G
wal_fpi            i64   G
wal_bytes          i64   G
```

Дублирование с `1_012_001` намеренное: progress показывает живой ход, лог —
стоимость завершенного прохода.

### `1_026_001` log: slow queries

Из `duration: X ms statement: SQL`.

```text
ts           ts   T
duration_ms  f64  G
sql          str  L   // усечение 64 КБ при чтении лога
```

### `1_027_001` log: lock waits

Из `process N still waiting for ShareLock ... after Y ms`.

```text
ts            ts   T
waiting_pid   i32  L
lock_type     str  L
wait_ms       f64  G
```

### `1_028_001` log: server lifecycle

Crash, shutdown, ready. Для crash из `DETAIL` извлекается SQL упавшего процесса,
если PostgreSQL его записал.

```text
ts          ts   T
kind        u8   L   // crash | shutdown | ready | ...
pid         i32  L
signal      i32  L
detail_sql  str? L
```

## `1_023_001` coverage

Без coverage top-N источники выглядят как полные данные. Пишется по одной
строке на каждый усеченный источник.

```text
ts              ts    T
source_type_id  u32   L
total           u32   G   // строк в источнике
collected       u32   G   // строк записано
max_n           u32   L   // лимит коллектора
order_by        str   L   // метрика/выражение отбора
cutoff_value    f64?  G   // NULL, если cutoff неизвестен
reason          u8    L   // 0=top_n, 1=timeout, 2=permission, 3=other
```

Coverage не делает top-N источник полным. Он только сообщает коду чтения, какую
часть источника видел коллектор и почему остальное отсутствует.
