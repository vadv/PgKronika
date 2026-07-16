# Контракт линз `kronika-incident`

Дата: 2026-07-16.

## 1. Потолок атрибуции и форма результата

`kronika-incident` объясняет совместно наблюдавшиеся аномалии за уже завершённый
период. Он не измеряет root cause. PgKronika не семплирует активные сессии и
wait events с частотой ASH, поэтому не располагает аддитивным бюджетом DB time и
не может приписать долю задержки конкретному запросу, ожиданию или ресурсу.

Вывод линзы — проверяемая диагностическая гипотеза. Допустимые роли:

- `lead` — наблюдение могло предшествовать другим членам кластера, а направление
  поддержано документированным механизмом;
- `amplifier` — наблюдение способно усилить инцидент, но не обязано его начать;
- `downstream` — вероятное следствие другого наблюдения;
- `coincident` — совпадение по времени без достаточного основания для направления.

Термин `root cause` в API и UI не используется. Единственное более сильное
свидетельство в текущих данных — сохранённое ребро `blocked_by` из
`pg_blocking_pids()`: оно прямо показывает, кто мешал backend получить
heavyweight lock в момент снимка. Даже такое ребро доказывает блокирование, а не
объясняет, почему держатель не завершил транзакцию.

Результат анализа содержит:

```text
Incident {
  interval, cluster_id,
  members: [AnomalyEpisodeRef],
  findings: [Finding],
  unclassified_members: [AnomalyEpisodeRef],
  context: [ContextFact],
  data_quality: DataQualitySummary
}

Finding {
  lens_id, lens_name, question,
  scope, role, confidence,
  mechanism,
  evidence: [{metric_ref, selector, interval, value, unit, coverage}],
  alternatives, suppressors,
  lowered_by, no_data
}
```

`evidence` всегда несёт исходные значения и значения формулы. Один только
коэффициент, score или текстовый вердикт не является достаточным выводом.

## 2. Данные и границы ответственности

### 2.1. Четыре разных объекта

- **Gauge** — значение в момент снимка: `numbackends`, `n_dead_tup`,
  `io_in_progress`, `MemAvailable`. Gauge не дифференцируется.
- **Cumulative counter** — накопительный счётчик: `calls`, `wal_bytes`, CPU
  ticks, `read_time_ms`. В линзу поступает только типизированный результат
  `kronika-diff`, а не вычитание внутри правила.
- **Derived value** — отношение или скорость, вычисленные только после
  получения честных дельт. Сначала считаются `delta(A)` и `delta(B)` на одном
  наборе интервалов, затем `sum(delta(A)) / sum(delta(B))`.
- **Anomaly episode** — последовательность окон с аномальным score из
  `kronika-anomaly`. Это кандидат для кластеризации, а не метрика и не причина.

PostgreSQL-специфичные identity, версии, reset sources и GUC gates принадлежат
`kronika-registry`. `kronika-diff` и `kronika-anomaly` остаются чистыми
интерпретаторами контрактов. Каталог ниже должен жить в отдельном будущем слое
`kronika-incident`; добавлять правила инцидентов в registry нельзя.

### 2.2. Data-quality contract

Для каждой серии используются статусы `Value`, `Reset`, `Gap`, `FirstPoint`,
`NotCollected` и `Anomaly`. Невалидная точка не превращается в ноль и не
соединяется с точкой после разрыва.

Стандартное поведение линзы, обозначенное далее как `DQ=standard`:

1. `Reset`, `Gap`, `FirstPoint`, timestamp anomaly и `NotCollected` исключают
   соответствующую пару одновременно из числителя и знаменателя.
2. `NULL` означает неизвестное или неприменимое значение. Он не равен нулю.
3. Измеренный ноль допустим только как `Value(0)` при непрерывном покрытии.
4. Для регулярного сигнала нужны не менее трёх валидных пар и покрытие не ниже
   70% ожидаемых снимков на рассматриваемом интервале. Это продуктовая политика,
   а не свойство PostgreSQL. Линзы событий и прямых lock edges явно отменяют это
   правило.
5. Если источник top-N, вывод относится только к сохранённым строкам. Строка
   `coverage` с `unknown_total=true`, timeout или permission снижает confidence;
   отсутствие сущности не доказывает нулевую активность.
6. Неизвестный GUC не разрешает делать вывод из нулевого timing. Положительная
   дельта остаётся свидетельством фактически накопленного времени, но полнота
   измерения неизвестна.

PR #68 или эквивалентный контракт `gated_by -> NotCollected` является
предпосылкой для timing-линз. Этот документ от него не зависит на уровне кода:
ветка PR #68 не включается. До появления gate-aware series timing-колонки с
нулём не участвуют в отрицательных выводах.

Ограничение gate глубже состояния collector session. `track_io_timing`,
`track_wal_io_timing` и `pg_stat_statements.track_planning` могут меняться в
отдельной сессии; PostgreSQL прямо предупреждает, что timing мог собираться не
весь период с момента reset. Поэтому `gate=on` не доказывает полное покрытие
агрегата, а `gate=off` требует `NotCollected`. Это правило следует из
[контракта cumulative statistics](https://www.postgresql.org/docs/18/monitoring-stats.html)
и [runtime GUC](https://www.postgresql.org/docs/18/runtime-config-statistics.html).

Для PG18 gate `pg_stat_io.*_time` выбирается по строке: `object='wal'` зависит
от `track_wal_io_timing`, остальные объекты — от `track_io_timing`. `fsyncs` и
`fsync_time` имеют смысл только в поддерживаемых строках; для relation I/O
fsync учитывается в `context='normal'`. `NULL` в неподдерживаемой комбинации
остаётся `NULL`. Версии PG16-17 используют `op_bytes` как gauge, PG18 —
кумулятивные `read_bytes`, `write_bytes`, `extend_bytes`.

### 2.3. Реально доступные семейства

Поддерживаемая матрица — PostgreSQL 15-18. Каталог использует только секции,
описанные в [`postgresql.md`](../../type-registry/postgresql.md) и
[`os.md`](../../type-registry/os.md):

- PostgreSQL: activity, statements, два контракта store plans, database,
  bgwriter/checkpointer, WAL, archiver, PG16+ I/O, prepared transactions, lock
  graph, vacuum progress, user tables/indexes, replication instance/replicas/
  slots, settings, reset/instance metadata, coverage и typed stderr events;
- Linux: process/process status, host CPU/stat/meminfo/loadavg/vmstat/PSI,
  diskstats, netdev/SNMP/netstat, mount capacity/topology, PID-to-cgroup mapping
  и cgroup CPU/memory/I/O/PID.

Отсутствуют, в частности, `pg_stat_slru`, `pg_stat_database_conflicts`,
subscription statistics, per-socket TCP, cgroup PSI, inode capacity, THP/NUMA
vmstat, jbd2, `pg_wal/archive_status` и размер `pg_wal`. Они не используются в
основном каталоге.

## 3. Кластеризация и вычисление

### 3.1. Интервал и контекст

Пользователь выбирает завершённый `[from, to]`. `kronika-anomaly` строит
episodes с некаузальным reference из того же периода; такой результат нельзя
использовать для push alerting.

Episodes сортируются по `(start, end, section, column, identity)`. Sweep-line
объединяет их, если следующий `start <= current_end + epsilon`, где по умолчанию
`epsilon` равен anomaly `step`. Чтобы цепочка коротких пересечений не склеила
весь период, компонент закрывается при достижении `max_cluster_span`. При
достижении лимита причина разбиения возвращается в `data_quality`.

Для направления используются три области:

- `P = [incident.start - max(window, 2 * source_period), incident.start)` —
  допустимое предшествование;
- `I = [incident.start, incident.end]` — совпадение;
- `D = (incident.end, incident.end + window]` — допустимое следствие.

Механизм без различающего наблюдения даёт не выше `coincident`. Например,
WAL generation в `P`, requested checkpoints в `I` и checkpointer I/O в `I/D`
поддерживают направление. Три аномалии в одном окне без такого порядка — нет.

### 3.2. Confidence

`confidence` имеет порядок `high > medium > low`.

- `high` разрешён для прямого lock edge или точного события ограничения
  ресурса, если scope и покрытие известны.
- `medium` требует документированного механизма, корректного порядка `P/I/D` и
  хотя бы одного различающего сигнала.
- `low` означает совместимость с механизмом, но отсутствие различающего сигнала
  или точной атрибуции scope.

Каждая линза задаёт верхнюю границу. Повышение выше cap запрещено. Несколько
одинаково сильных `lead` сохраняются как co-leads; движок не выбирает
единственного победителя.

### 3.3. Детерминизм и пределы

Findings сортируются по `(confidence desc, role_rank, lens_id, scope_key)`, где
`role_rank = lead, amplifier, downstream, coincident`. Равенство не разрешается
временным lead-lag на каденсе 5-30 секунд. `cluster_id` строится из нормализованного
интервала и отсортированных member refs.

Web/reader обязаны ограничить число sections, series, decoded points, episodes,
clusters, findings на cluster и evidence rows на finding. Превышение любого
лимита даёт `skipped{scope, reason, observed, limit}`, а не усечение под видом
полного анализа. Кластеризация имеет `O(E log E)` по числу допущенных episodes.
Каталог выполняет indexed joins по объявленным keys; декартовы произведения
series запрещены. Top-K findings поддерживается bounded heap во время оценки.

### 3.4. Контекст, который не является линзой

В каждый incident без причинной роли добавляются факты:

- рестарт PostgreSQL: изменение `reset_metadata.postmaster_start_time`;
- рестарт узла: изменение `instance_metadata.boot_id`/`btime`;
- lifecycle events `crash`, `shutdown`, `ready`;
- изменение значимых `pg_settings`, включая `pending_restart`;
- log/collector gaps и coverage/top-N;
- смена PostgreSQL major, extension version, hostname, node id или system id.

Деплой приложения сейчас не собирается. Его нельзя выводить из смены query mix;
для точного deploy context нужен отдельный event source.

## 4. Каталог PostgreSQL

Во всех формулах `d(x)` — честная typed delta, `r(x)` — `d(x)/dt`, а
`paired(a,b)` — отношение сумм дельт на одном наборе валидных пар.

### `PG-QRY-001` — изменение query workload

- **Вопрос и роль:** что изменилось у конкретного normalized query: частота,
  работа на вызов или время исполнения? `calls` может быть `lead`, execution
  time — обычно `downstream`.
- **Вход:** `pg_stat_statements` по identity
  `(queryid,userid,dbid,toplevel)`; `calls`, `total_exec_time`, `rows`,
  `shared/local/temp_blks_*`. Нужны extension, известная раскладка и надёжный
  `queryid`; PG15-18, с поправкой на имена timing в ext 1.11+.
- **Расчёт и время:** в `P/I` считать `paired(total_exec_time,calls)`,
  `paired(rows,calls)` и сумму block deltas на `d(calls)>0`. Рост ms/call при
  плоских blocks/call — latency symptom, а не доказательство plan regression;
  рост blocks/call — рост работы. Минимум — `DQ=standard`.
- **Cap и альтернативы:** `medium`; снижают top-N coverage, deallocation,
  `queryid=NULL`, reset и mixed GUC coverage. Альтернативы: смена данных,
  параметров, cache state, locks, CPU или I/O.
- **Показать:** query identity/text при доступе, `d(calls)`, `d(total_exec_time)`,
  ms/call, rows/call, каждый blocks/call, интервалы и coverage.
- **DQ:** standard; неизвестный timing gate запрещает отрицательный вывод из
  нуля. Отсутствующая top-N строка — unknown, не исчезновение query.

### `PG-PLAN-002` — смена сохранённого плана

- **Вопрос и роль:** совпала ли деградация query с появлением или сменой
  `planid`? При порядке `planid` в `P` -> execution work в `I` роль `lead`, иначе
  `coincident`.
- **Вход:** `pg_store_plans` `1_003_001` или `1_004_001`: `planid`, calls,
  execution metrics и plan text; мост к statements по contract конкретного
  форка. Секция опциональна и читается раз в 5 минут.
- **Расчёт и время:** сравнить набор и доли calls по planid до и во время
  incident; требовать новый/вернувшийся planid и изменение work/call или
  exec/call. `mean_*` — gauge, он не дифференцируется.
- **Cap и альтернативы:** `medium` для ossc с core queryid, `low` для best-effort
  bridge vadv. Альтернативы: тот же plan на другом объёме данных, eviction
  extension entries, reset, редкий план между 5-минутными снимками.
- **Показать:** fork/version, query bridge, старые и новые planid, calls share,
  total/mean time, plan sample, first/last call, coverage.
- **DQ:** standard; без extension, bridge или достаточного покрытия линза
  `not_evaluated`. Нулевые planning-поля при неизвестном
  `pg_store_plans.track_planning` не используются.

`pg_stat_statements` не содержит planid; точная смена плана требует отдельного
источника. Контракты расширений различаются, что отражено в
[официальной документации ossc](https://ossc-db.github.io/pg_store_plans/) и
[исходном форке vadv](https://github.com/vadv/pg_store_plans).

### `PG-TEMP-003` — временные файлы и spill

- **Вопрос и роль:** выросла ли работа через temporary blocks/files? Для query с
  ростом temp blocks/call это `lead` или `amplifier`; общая latency — downstream.
- **Вход:** statements `temp_blks_read/written` и ext 1.10+ temp timings;
  database `temp_files/temp_bytes`; PG16+ `pg_stat_io` с
  `object='temp relation'`; `pg_log_temp_files` при включённом логировании.
- **Расчёт и время:** в `P/I` считать temp blocks/call, temp bytes/file и
  temp-time/op только после дельт, со знаменателем >0. Log event является
  точным фактом создания файла, но не полным счётчиком без подходящей настройки.
- **Cap и альтернативы:** `medium`. Альтернативы: hash/sort, materialize,
  maintenance, explicit temp tables; work_mem exhaustion без plan evidence не
  утверждается.
- **Показать:** query, calls, temp block deltas, database files/bytes, log file
  sizes, PG I/O rows и gate state.
- **DQ:** standard; timing `NotCollected` не отменяет положительные block/file
  counters. Нулевой log stream ничего не доказывает при gap/disabled source.

### `PG-ANALYZE-004` — риск устаревшей planner statistics

- **Вопрос и роль:** накопились ли изменения relation после последнего ANALYZE
  рядом с изменением query work/plan? Роль — `lead` с cap `medium`.
- **Вход:** user tables по `(datid,relid)`:
  `n_mod_since_analyze`, `reltuples`, `last_analyze`, `last_autoanalyze`,
  analyze counters; `pg_settings` global analyze threshold/scale factor;
  statements/store plans как различающий сигнал. PG15-18.
- **Расчёт и время:** gauge
  `n_mod_since_analyze/max(abs(reltuples),1)` должен быть повышен в `P` и
  сопровождаться отсутствием свежего analyze и изменением plan/work в `I`.
  Дельта gauge запрещена.
- **Cap и альтернативы:** ниже `medium`, если неизвестны per-table reloptions;
  для partitioned parent autovacuum не выполняет autoanalyze. Альтернативы:
  data skew, correlated columns, параметризация, cache/resource pressure.
- **Показать:** relation identity, n_mod, reltuples, ratio, last analyze times,
  analyze count deltas, global settings и query/plan evidence.
- **DQ:** standard; relation top-N coverage обязателен. Точный autovacuum
  threshold не заявляется: per-table overrides и статистика partition parent
  не собираются. Формулы autovacuum описаны в
  [routine vacuuming](https://www.postgresql.org/docs/18/routine-vacuuming.html).

### `PG-VACUUM-005` — накопление dead tuples и работа vacuum

- **Вопрос и роль:** растёт ли vacuum debt relation и есть ли признаки, что
  cleanup не успевает? Debt — `lead`/`amplifier`, vacuum I/O — `downstream`.
- **Вход:** user tables `n_dead_tup`, `n_live_tup`, `last_*vacuum`, vacuum
  counters, PG18 total vacuum times; vacuum progress; autovacuum log events;
  PG16+ `pg_stat_io context='vacuum'`.
- **Расчёт и время:** gauges `n_dead_tup` и
  `n_dead_tup/max(n_live_tup+n_dead_tup,1)` в `P/I`; counter rates vacuum;
  progress fields остаются gauges одного запуска. Требуется различающий сигнал:
  debt растёт до scan/read/write anomaly либо активный vacuum совпадает с I/O.
- **Cap и альтернативы:** `medium`. `n_dead_tup` — оценка, не физический bloat;
  `context='vacuum'` объединяет VACUUM и ANALYZE и не видит buffer hits.
  Альтернативы: ожидаемый batch cleanup, manual VACUUM, ANALYZE, relation rewrite.
- **Показать:** relation sizes, live/dead estimates, last/counter/timing vacuum,
  progress phase/blocks, log event and I/O selector.
- **DQ:** standard; top-N absence unknown. Снижение `n_dead_tup` является
  изменением gauge, не `Reset`.

### `PG-FREEZE-006` — XID/MXID headroom

- **Вопрос и роль:** приблизилась ли database/relation к forced aggressive
  vacuum или защите от wraparound? Роль — `lead`; cap `medium` по age/headroom
  и `high` только при сохранённом warning/error PostgreSQL о wraparound.
- **Вход:** database `frozen_xid_age/min_mxid_age`; user tables
  `xid_age/mxid_age`; settings `autovacuum_freeze_max_age` и
  `autovacuum_multixact_freeze_max_age`; vacuum progress/activity/logs.
- **Расчёт и время:** gauge headroom `limit-age`, отдельно для XID и MXID;
  минимум три снимка либо один официальный warning/error log. Не смешивать две
  шкалы. Отрицательный headroom не является counter reset.
- **Cap и альтернативы:** снижают per-table storage parameters, top-N и
  устаревший settings snapshot. Старый возраст может означать ожидающий запуска
  vacuum, а не уже возникшую latency.
- **Показать:** database/relation, age, применённый global limit, headroom,
  направление/скорость gauge trend, vacuum activity и blocking holders.
- **DQ:** standard; `NULL` shared database row исключается. Для PG15-18 hard
  protection начинается при остатке около **трёх миллионов**, не одного;
  нормативный источник —
  [PG18 routine vacuuming](https://www.postgresql.org/docs/18/routine-vacuuming.html).

### `PG-HOT-007` — HOT failure и index amplification

- **Вопрос и роль:** какая доля updates не стала HOT и совпала ли она с ростом
  index/WAL work? Роль — `lead` или `amplifier`; cap `medium`.
- **Вход:** user tables `n_tup_upd`, `n_tup_hot_upd`; PG16+
  `n_tup_newpage_upd`; user indexes `idx_blks_read/hit`, size and scan counters;
  statements/WAL counters.
- **Расчёт и время:** на paired intervals с `d(n_tup_upd)>0` показать
  `d(hot)/d(upd)`, `d(newpage)/d(upd)` при наличии, index block miss rate и WAL
  per update. Низкая HOT share должна предшествовать или совпасть с index/WAL
  amplification.
- **Cap и альтернативы:** indexed-column updates, недостаток свободного места на
  heap page, fillfactor, новые/удалённые индексы и workload mix. Низкая HOT share
  сама по себе не измеряет index bloat.
- **Показать:** все дельты и ratios по relation, связанные index identities/
  sizes, WAL bytes/FPI per update, PG version.
- **DQ:** standard; в PG15 ветка `n_tup_newpage_upd` unavailable, но HOT ratio
  остаётся доступен. Механика HOT —
  [официальная глава PostgreSQL](https://www.postgresql.org/docs/18/storage-hot.html).

### `PG-CHKPT-008` — requested-checkpoint pressure

- **Вопрос и роль:** выросли ли requested checkpoints и их write/sync work?
  Requested anomaly — возможный `lead`; I/O — `downstream`.
- **Вход:** объединённая секция bgwriter/checkpointer: `checkpoints_req/timed`,
  write/sync time, buffers; checkpoint log events; PG16+ checkpointer rows
  `pg_stat_io`. Registry скрывает перенос колонок в `pg_stat_checkpointer` PG17.
- **Расчёт и время:** deltas `req`, `timed`, buffers, write/sync ms;
  `d(req)/(d(req)+d(timed))` только при положительном знаменателе. Требовать req
  в `P/I` и checkpointer work в `I/D`.
- **Cap и альтернативы:** `medium`; manual `CHECKPOINT`, restartpoints и
  checkpoint skips не позволяют приравнять requested к `max_wal_size`.
  `num_timed` PG17+ включает scheduled checkpoints, которые могли быть skipped.
- **Показать:** все deltas/ratio, log reason/phase, buffers, write/sync ms,
  checkpointer I/O and OS device evidence.
- **DQ:** standard; PG version/layout показывается. Reset любой исходной семьи
  разрывает общий расчёт.

### `PG-WAL-009` — WAL/FPI amplification

- **Вопрос и роль:** выросли ли WAL bytes/record, FPI share или случаи full WAL
  buffers? Роль — `lead`/`amplifier`; cap `medium` только с различающим
  checkpoint/workload evidence.
- **Вход:** `pg_stat_wal` PG15-18; statements WAL columns по query; PG18
  `pg_stat_io object='wal'`; checkpoint evidence.
- **Расчёт и время:** `paired(wal_bytes,wal_records)`,
  `paired(wal_fpi,wal_records)`, `d(wal_buffers_full)` и WAL per call/update.
  FPI rise после checkpoint поддерживает механизм first-page modification;
  без checkpoint остаётся association.
- **Cap и альтернативы:** bulk writes, full_page_writes, wal_log_hints,
  relation rewrites, logical logging and backup activity. Высокий FPI ratio не
  доказывает слишком частые checkpoints.
- **Показать:** bytes, records, FPI, buffers-full deltas/ratios, top query
  contributors, checkpoint interval and WAL timing gate.
- **DQ:** standard; PG18 WAL timings берутся из row-gated `pg_stat_io`, PG15-17
  — из `pg_stat_wal`. WAL configuration и связь с checkpoint описаны в
  [официальной документации](https://www.postgresql.org/docs/18/wal-configuration.html).

### `PG-CACHE-010` — shared-buffer miss pressure

- **Вопрос и роль:** выросли ли обращения, не найденные в PostgreSQL shared
  buffers? Роль — `lead`/`amplifier`; cap `medium`.
- **Вход:** database `blks_read/blks_hit`; per-relation heap/index/toast
  `*_blks_read/hit`; PG16+ `pg_stat_io` reads/hits/evictions/reuses по context.
- **Расчёт и время:** `d(read)/(d(read)+d(hit))` на положительном знаменателе,
  отдельно по database/relation/fork/context. Сравнить нормальный context с
  `bulkread` и его `reuses`; требовать рост miss work в `P/I`.
- **Cap и альтернативы:** sequential/bulk scan, cold start, larger working set,
  table rewrite, intentional cache turnover. PostgreSQL read здесь не означает
  физический device read и может быть обслужен OS page cache.
- **Показать:** read/hit deltas and ratio, evictions/reuses, contexts,
  relation/query evidence и OS disk bytes.
- **DQ:** standard; relation top-N coverage. PG docs подтверждают, что hit
  относится только к PostgreSQL buffer cache и не включает OS cache:
  [PG16 statistics](https://www.postgresql.org/docs/16/monitoring-stats.html).

### `PG-IO-011` — PostgreSQL I/O time per operation

- **Вопрос и роль:** выросло ли наблюдаемое время на одну I/O operation внутри
  PostgreSQL? Обычно `downstream` OS/device pressure или `amplifier`; cap
  `medium`.
- **Вход:** PG15 database/statements block time; PG16+ `pg_stat_io` by exact
  `(backend_type,object,context)`; operation counters and row-dependent gates.
- **Расчёт и время:** `sum(d(time_ms))/sum(d(ops))` на одних валидных парах и
  `d(ops)>0`; никогда `rate(time)/raw ops`. Сопоставить с device latency/PSI в
  `P/I` и query latency в `I/D`.
- **Cap и альтернативы:** OS page cache, filesystem, cgroup I/O, backend mix,
  timer overhead and partial per-session GUC coverage. `pg_stat_io` не покрывает
  relation I/O, обходящий shared buffers.
- **Показать:** selector row, time/op, обе суммы, gate state/history, NULL rows,
  OS device/mount evidence.
- **DQ:** standard; gate unknown ограничивает вывод положительным накопленным
  временем, gate off -> `NotCollected`. Ноль не означает быстрый I/O.

### `PG-LOCK-012` — heavyweight lock wait graph

- **Вопрос и роль:** какой backend непосредственно блокировал waiter в момент
  снимка? Blocker — `lead`, waiter latency — `downstream`.
- **Вход:** `pg_locks` conditional-full graph `1_011_002` для PG15-18:
  `pid`, `blocked_by`, lock target/mode/type, root/depth, query/session context,
  `waitstart`.
- **Расчёт и время:** восстановить все edges, fan-out и cycles без выбора одного
  parent. Одной полной строки графа достаточно; 70%/три пары не требуются.
  Длительность — `snapshot_ts-waitstart`, если `waitstart` известен.
- **Cap и альтернативы:** cap `high` для edge, `medium` для terminal-root
  interpretation. PID 0 означает prepared transaction. Parallel workers могут
  давать duplicate PIDs, уже дедуплицированные collector. Edge не объясняет,
  почему blocker жив.
- **Показать:** полный путь, все blocked_by, PIDs, database/OIDs/name when
  resolvable, mode/type/target, waitstart/duration, state/query and graph guard.
- **DQ:** partial graph запрещён. При наличии rows вывод прямой; отсутствие
  section не используется как доказательство отсутствия locks, если исторически
  нельзя отличить precheck-empty от collector/guard failure. Семантика
  `pg_blocking_pids()` —
  [официальные system functions](https://www.postgresql.org/docs/18/functions-info.html).

### `PG-HORIZON-013` — удержание xmin и открытые транзакции

- **Вопрос и роль:** есть ли long/idle/prepared transaction, способная удерживать
  vacuum horizon или locks? Роль — `lead` к vacuum debt; cap `medium`.
- **Вход:** activity `state`, `xact_start`, `backend_xid_age`,
  `backend_xmin_age`; database `idle_in_transaction_time`; prepared-xacts
  count/max age/XID age; lock graph.
- **Расчёт и время:** repeated gauge maxima/counts в `P/I`; prepared rows или
  idle-in-transaction должны предшествовать росту dead tuples/freeze headroom
  и, для lock claim, иметь edge. Wall-clock и XID age показываются отдельно.
- **Cap и альтернативы:** legitimate long report/maintenance, replication
  worker, transaction without relevant relation. Большой xmin age не доказывает
  блокировку конкретного vacuum без relation-level horizon evidence.
- **Показать:** PID/database/state, xact start, XID/xmin ages, prepared count/max
  ages, lock edges and downstream relation metrics.
- **DQ:** standard; nullable ages остаются unknown. Prepared section absent
  означает empty только при успешном conditional collection. PostgreSQL
  подтверждает, что prepared transaction продолжает держать locks и мешает
  VACUUM: [PREPARE TRANSACTION](https://www.postgresql.org/docs/18/sql-prepare-transaction.html).

### `PG-CONN-014` — connection и transaction pressure

- **Вопрос и роль:** приблизилось ли число backends к лимиту и вырос ли churn
  соединений при снижении throughput? Роль — `lead`/`amplifier`; cap `medium`.
- **Вход:** database `numbackends`, `sessions`, `sessions_*`, xact counters;
  activity counts by state/backend type; settings `max_connections`, per-DB
  `datconnlimit`; cgroup pids/process memory как отдельное evidence.
- **Расчёт и время:** gauges `sum(numbackends)/max_connections` и per-DB finite
  limit; rates sessions and xacts; state composition. Не использовать
  `work_mem * connections` как измерение памяти.
- **Cap и альтернативы:** pool resize, idle but harmless sessions, maintenance,
  parallel workers, low TPS by design. CPU/memory/lock effect должен иметь свои
  метрики.
- **Показать:** numerator/limit/ratio, states/backend types, session/xact rates,
  abandoned/fatal/killed deltas, process/cgroup corroboration.
- **DQ:** standard; shared database row and unlimited/invalid datconnlimit
  исключаются; permission-limited activity lowers confidence.

### `PG-REPL-015` — physical replication progress

- **Вопрос и роль:** на каком наблюдаемом LSN stage растёт byte gap? Это
  `downstream`/`coincident` до появления независимого resource evidence; cap
  `medium`.
- **Вход:** replication instance current/receive/replay LSN; replicas
  sent/write/flush/replay LSN and reported lag intervals; WAL generation rate.
- **Расчёт и время:** gauge byte gaps `current-sent`, `sent-write`,
  `write-flush`, `flush-replay` только при известных LSN; изменение gap и WAL
  generation сравниваются в `P/I`. Отрицательный gap — data anomaly.
- **Cap и альтернативы:** disconnected/idle standby, delayed apply,
  `recovery_min_apply_delay`, feedback cadence and clock effects. Reported
  `write_lag/flush_lag/replay_lag` — время недавних stage acknowledgements, а
  не декомпозиция причин «network/disk/apply»; caught-up standby может хранить
  последнее значение.
- **Показать:** все LSN/gaps, generation bytes/s, state/sync_state, reported
  lag values and last replay time.
- **DQ:** gauges не дифференцируются как counters; missing LSN stays unknown.
  Нормативная семантика lag —
  [PG16 monitoring](https://www.postgresql.org/docs/16/monitoring-stats.html).

### `PG-SLOT-016` — WAL retention replication slot

- **Вопрос и роль:** удерживает ли slot растущий объём WAL? Роль — `lead` к
  filesystem pressure; cap `medium`.
- **Вход:** slots `active`, `restart_lsn`, `confirmed_flush_lsn`,
  `retained_bytes`, `wal_status`; instance current WAL LSN; setting
  `max_slot_wal_keep_size`.
- **Расчёт и время:** repeated retained-bytes gauge и его slope по реальным
  timestamps; inactive/`extended|unreserved|lost` усиливают finding. Не
  вычитать gauge через `kronika-diff`.
- **Cap и альтернативы:** ожидаемый consumer outage, logical decoding backlog,
  intentional unlimited retention. `active=false` сам по себе не дефект.
- **Показать:** slot/type/plugin, active, both LSN, retained bytes/trend,
  wal_status, current LSN, configured limit and filesystem headroom.
- **DQ:** standard for surrounding counters; slot row disappearance не равно
  zero при coverage gap. Текущий collector не хранит `xmin`, `catalog_xmin` и
  `safe_wal_size`, поэтому tuple/catalog retention и точный loss headroom не
  заявляются. Поля описаны в
  [`pg_replication_slots`](https://www.postgresql.org/docs/18/view-pg-replication-slots.html).

### `PG-ARCH-017` — ошибки WAL archiving

- **Вопрос и роль:** были ли подтверждённые ошибки archive command/library?
  Свежая failure — `lead`/`amplifier`; cap `medium`.
- **Вход:** archiver `failed_count`, `last_failed_*`, `archived_count`,
  `last_archived_*`; settings archive mode/command/library; filesystem lens.
- **Расчёт и время:** положительная `d(failed_count)` и свежий
  `last_failed_time` в `P/I`; сравнить с successful count. Одно timestamp gauge
  может подтвердить событие, но не backlog.
- **Cap и альтернативы:** retry succeeded, timeline switch, disabled archive,
  command semantics. `last_archived_wal` не является границей полного архива:
  файлы не обязаны завершаться строго по имени.
- **Показать:** failure/success deltas, last names/times, settings and FS bytes.
- **DQ:** standard; без роста failure нельзя объявлять зависший архиватор.
  Текущие данные не содержат `.ready`, `pg_wal` size или upload progress.
  Ограничение порядка зафиксировано в
  [cumulative statistics](https://www.postgresql.org/docs/18/monitoring-stats.html).

### `PG-SYNC-018` — sampled synchronous-replication wait

- **Вопрос и роль:** наблюдались ли backends на `wait_event='SyncRep'` при
  настроенной synchronous replication? Роль — `lead` к commit latency либо
  `downstream`; cap `medium`.
- **Вход:** activity snapshots, settings `synchronous_commit` и
  `synchronous_standby_names`, replica state/sync_state/LSN gaps, network and
  standby resource evidence при наличии.
- **Расчёт и время:** per-snapshot count and duration proxy from query/state
  start; минимум три снимка, если нет typed lock/event. SyncRep concentration в
  `I` должна совпасть со снижением commit rate/ростом active time.
- **Cap и альтернативы:** standby disk/apply, network, walsender scheduling,
  configured remote_apply semantics. Без per-socket/standby OS нельзя назвать
  сеть причиной.
- **Показать:** waiting PIDs/query identities, sample count, settings,
  sync_state, LSN gaps, xact rate and external evidence.
- **DQ:** standard; `wait_event=NULL` under insufficient privileges/coverage is
  unknown. Snapshot sampling пропускает короткие waits.

### `PG-WAIT-019` — sampled internal wait concentration

- **Вопрос и роль:** выросла ли доля active backends, увиденных на
  `LWLock`, `BufferPin` или `IO` wait? Роль — `coincident`/`amplifier`; cap
  `low` без прямого отдельного evidence.
- **Вход:** activity state/wait type/event and backend type; lock graph для
  исключения heavyweight locks; PG/OS workload evidence.
- **Расчёт и время:** по каждому снимку считать waiting active / visible active,
  затем median/max в `I` против reference. Категории не смешиваются. Минимум
  три снимка и 70% coverage.
- **Cap и альтернативы:** sampling bias, parallel workers, permission NULLs,
  нормальная короткая синхронизация. `LWLock` не означает fast-path lock
  exhaustion; `BufferPin` не даёт holder edge.
- **Показать:** counts/denominator by event/backend type, sample timestamps,
  top visible PIDs/queries and discriminating resource metrics.
- **DQ:** отсутствие sample не доказывает отсутствие wait. Без ASH attribution
  остаётся эвристической.

## 5. Каталог Linux и cgroup

OS finding связывается с PostgreSQL только при совпадении `node_self_id`,
интервала и совместимого scope. Host-wide signal нельзя приписывать container
без PID-to-cgroup или cgroup evidence.

### `OS-CPU-020` — host CPU pressure и steal

- **Вопрос и роль:** испытывал ли host runnable pressure или hypervisor steal?
  Роль — `lead`/`amplifier`; cap `medium`.
- **Вход:** aggregate `os_cpu cpu_id=-1`, host PSI CPU `some`, loadavg,
  procs_running and topology. Linux, при наличии procfs/PSI.
- **Расчёт и время:**
  `work=d(user-guest)+d(nice-guest_nice)+d(system+irq+softirq)`,
  `total=work+d(idle+iowait+steal)`; показывать `work/total`, `iowait/total`,
  `steal/total`, runnable/core и `d(PSI some_total)/dt`. Guest ticks уже входят
  в user/nice и второй раз не прибавляются. Pressure в `P/I`, PG latency в
  `I/D`.
- **Cap и альтернативы:** other host workloads, CPU hotplug, VM clock/accounting,
  intentional batch. Busy без PSI означает demand, но не доказанную starvation;
  steal не идентифицирует noisy neighbor.
- **Показать:** все tick deltas/shares, HZ, cores, load/runnable, PSI total/avg,
  top process CPU and PG symptoms.
- **DQ:** standard; counter reset при boot/CPU reattach разрывает ряд.
  System-level CPU `full` не определён и не собирается; kernel contract —
  [PSI](https://docs.kernel.org/accounting/psi.html).

### `OS-CGRP-021` — cgroup CPU throttling

- **Вопрос и роль:** была ли cgroup PostgreSQL реально throttled при доступном
  host CPU? Роль — `lead`/`amplifier`; cap `medium`.
- **Вход:** PID mapping `(pid,starttime)->cgroup_path`, PostgreSQL PIDs from
  activity/process, cgroup CPU usage/throttled/nr_throttled/quota/period, host
  CPU/PSI.
- **Расчёт и время:** positive deltas throttled_usec and nr_throttled in `P/I`,
  quota finite, host not saturated; `d(throttled_usec)/dt` is shown as a rate,
  not a percent of wall time.
- **Cap и альтернативы:** intentional quota, ancestor throttling, migration
  between cgroups, other processes in same cgroup. Collector lacks `nr_periods`,
  so fraction of throttled periods is unavailable.
- **Показать:** cgroup path/scope, PG PIDs, quota/period, usage and throttle
  deltas/rates, host busy/steal/PSI.
- **DQ:** standard; mapping coverage must overlap CPU interval. Positive
  throttle is valid evidence; zero under missing controller is not. Kernel
  fields are defined in
  [cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html).

### `OS-MEM-022` — host reclaim, swap и OOM

- **Вопрос и роль:** совпала ли деградация с host memory pressure, direct
  reclaim, swap or OOM? Роль — `lead`/`amplifier`; cap `medium`.
- **Вход:** meminfo available/free/swap/dirty/writeback; vmstat page faults,
  scan/steal, swap and oom counters; memory PSI; process major faults/RSS/swap.
- **Расчёт и время:** gauges headroom and swap use; rates pswpin/out,
  pgscan/steal direct/kswapd, major faults, PSI totals. Direct reclaim/swap in
  `P/I` and PG latency/cache misses in `I/D`.
- **Cap и альтернативы:** filesystem cache reclaim, explicit swap policy,
  short-lived non-PG process, NUMA/THP not observed. Host oom_kill does not name
  victim without process/lifecycle correlation.
- **Показать:** MemAvailable/Total, SwapFree/Total, all counter deltas/rates,
  PSI, top process RSS/swap/major faults, lifecycle evidence.
- **DQ:** standard; optional vmstat NULL is unknown. Gauge fall is not Reset.

### `OS-CGMEM-023` — cgroup memory limit events

- **Вопрос и роль:** достигала ли PostgreSQL cgroup `memory.high/max` или OOM?
  Роль — `lead`/`amplifier`; cap `medium`, `high` только для kill event плюс
  совпавшего исчезновения PG process/lifecycle event.
- **Вход:** PG PID mapping; cgroup memory current/max, anon/file/kernel/slab and
  event counters; cgroup processes and host memory.
- **Расчёт и время:** current/max gauge on finite limit; positive deltas high,
  max, oom, oom_kill in `P/I`; process disappearance by `(pid,starttime)`.
- **Cap и альтернативы:** event inherited from descendant, other tasks in same
  cgroup, container restart, host OOM. Current below max after event не отменяет
  его.
- **Показать:** path/scope/PIDs, current/max/ratio, every event delta, memory
  breakdown, host evidence and lifecycle.
- **DQ:** standard. В cgroup v1 unsupported v2 events записываются текущим
  collector как нули; ноль не подавляет finding и не доказывает отсутствие
  события. Positive values remain evidence.

### `OS-BLOCK-024` — block-device latency и queue pressure

- **Вопрос и роль:** выросли ли completion time и очередь конкретного device?
  Роль — `lead`/`amplifier`; cap `medium`.
- **Вход:** diskstats by `(major,minor)`, mountinfo, host IO PSI; cgroup I/O and
  PG I/O as attribution evidence.
- **Расчёт и время:** `d(read_time_ms)/d(reads)`,
  `d(write_time_ms)/d(writes)`, optional flush time/op; average in-flight proxy
  `d(io_weighted_time_ms)/dt_ms`; `io_in_progress` gauge; bytes from sectors*512.
  Требуются positive operation denominators.
- **Cap и альтернативы:** device mapper layering, partitions vs whole disk,
  page cache, remote/network filesystem, discard/flush mix. `io_time_ms/dt` не
  используется как доказательство saturation: при parallel I/O он не измеряет
  полную capacity.
- **Показать:** device/major/minor/mount, ops/bytes/time deltas, ms/op,
  weighted-time ratio, queue gauge, PSI and PG/cgroup evidence.
- **DQ:** standard; reset при reattach/overflow. Kernel semantics —
  [I/O statistics fields](https://docs.kernel.org/admin-guide/iostats.html).

### `OS-WB-025` — dirty/writeback pressure

- **Вопрос и роль:** совпали ли elevated Dirty/Writeback и device write/flush
  work с PG sync/write latency? Роль — `amplifier`/`coincident`; cap `low`,
  `medium` при устойчивом порядке и scoped writer evidence.
- **Вход:** meminfo Dirty/Writeback gauges; diskstats write/flush; process and
  cgroup write bytes; PG checkpoint/WAL/I/O timings.
- **Расчёт и время:** sustained gauge anomaly in `P`, device write/flush in
  `I`, PG sync/write in `I/D`. Per-process `write_bytes` рассматривается отдельно
  от device bytes: kernel учитывает его при dirtying, а не в момент writeback.
- **Cap и альтернативы:** normal checkpoint, filesystem journal, external
  writer, background flusher, storage latency. Collector не хранит kernel dirty
  thresholds, поэтому нельзя заявлять пересечение `dirty_ratio`.
- **Показать:** Dirty/Writeback bytes, device ops/bytes/time, process/cgroup
  writers, PG checkpoint/sync values and temporal offsets.
- **DQ:** standard; фазовый сдвиг dirtying->writeback допускается в пределах
  `P/I/D`, но не превращается в точную attribution. Kernel threshold semantics —
  [VM sysctl](https://docs.kernel.org/admin-guide/sysctl/vm.html).

### `OS-IOWHO-026` — внешний I/O contender

- **Вопрос и роль:** какая process/cgroup увеличила block I/O рядом с pressure?
  Роль — `lead`/`amplifier`; cap `medium` при cgroup-device mapping, иначе `low`.
- **Вход:** process `(pid,starttime)` read/write/cancelled bytes and cmd/comm;
  PID-cgroup mapping; cgroup I/O by device; diskstats/mount; PG process set.
- **Расчёт и время:** top bounded contributors по positive deltas. Strong path:
  non-PG process -> unique cgroup -> same `(major,minor)` cgroup I/O -> device
  pressure in `P/I`. PID-only bytes без device остаются association.
- **Cap и альтернативы:** process exit/PID reuse, permissions, buffered writes,
  shared cgroup, device mapper. `rchar/wchar` не используются как block bytes.
- **Показать:** process identity/starttime/command, actual read/write/cancelled
  deltas, mapping, cgroup device bytes/ops, disk evidence and PG symptom.
- **DQ:** standard; process/cgroup caps and permission NULL lower confidence;
  no negative inference from missing PID. `/proc/PID/io` semantics —
  [procfs documentation](https://docs.kernel.org/filesystems/proc.html).

### `OS-FS-027` — filesystem byte headroom

- **Вопрос и роль:** был ли mount близок к исчерпанию доступных bytes? Роль —
  `lead`/`amplifier`; cap `high` для наблюдаемого zero/near-zero headroom,
  `medium` для привязки к PostgreSQL.
- **Вход:** mountinfo total/free bytes, mount/source/fstype/major/minor;
  `pg_settings.data_directory` and tablespaces when mapping is resolvable;
  archive/slot/WAL evidence.
- **Расчёт и время:** gauge `free_bytes/total_bytes` and absolute free bytes;
  минимум две materialized segment copies либо один exact ENOSPC-like PG log.
  Threshold является продуктовой policy и показывается в evidence.
- **Cap и альтернативы:** reserved blocks, quotas, thin provisioning, overlay,
  WAL/tablespace symlink outside mapped mount. Низкий free space может быть
  coincident без error/growth evidence.
- **Показать:** mount/source/fstype/scope, total/free/ratio/threshold, resolved PG
  path, slot/archive/WAL rates and errors.
- **DQ:** `NULL` statvfs -> not evaluated; zero is real zero. Inode exhaustion
  не покрывается и не упоминается как результат этой линзы.

### `OS-NET-028` — network errors и retransmission

- **Вопрос и роль:** наблюдался ли рост interface/TCP error counters? Роль —
  `coincident`/`amplifier`; cap `low` для связи с PostgreSQL.
- **Вход:** netdev bytes/packets/errors/drops/carrier; SNMP TCP retrans/reset/
  fail; TcpExt timeout/retrans/listen overflow/drop; scope host or pod_net.
- **Расчёт и время:** error/retrans deltas and ratios to packets/segments on
  paired positive denominators in `P/I`; сопоставить с connection/replication
  symptoms, не присваивая socket.
- **Cap и альтернативы:** unrelated sockets, interface aggregation, local
  listen backlog, packet reordering and host networking. Link speed и per-socket
  TCP_INFO не собираются, поэтому saturation и replication network cause не
  заявляются.
- **Показать:** interface/scope, all relevant deltas/ratios, PG connection or
  replication evidence and coverage.
- **DQ:** standard; host and pod_net scopes не смешиваются. Counter meanings,
  включая ListenDrops/Overflows, заданы в
  [kernel SNMP documentation](https://docs.kernel.org/networking/snmp_counter.html).

## 6. Roadmap: полезные, но не реализуемые сейчас линзы

| Кандидат | Почему исключён из каталога | Точный недостающий input |
|---|---|---|
| ASH/DB-time attribution | Activity snapshots пропускают короткие waits и не дают аддитивного времени | Частый bounded sampling `pid/queryid/wait_event/state`, sample coverage и dropped samples |
| SLRU/subtransaction/MultiXact pressure | `pg_stat_slru` не собирается | Полная PG15-18 секция `pg_stat_slru` с identity, counters, reset time |
| Exact table bloat | `n_dead_tup` — оценка dead tuples, не физический bloat | Bounded `pgstattuple_approx`/page-level estimator с relation coverage и cost budget |
| Exact autovacuum threshold | Нет per-table reloptions; PG18 insert formula требует frozen-page share | `reloptions`, `relpages`, `relallfrozen` и versioned threshold inputs |
| Recovery conflict classes | Есть только aggregate `pg_stat_database.conflicts` | `pg_stat_database_conflicts` по database и классу, reset metadata |
| Logical subscription/apply failure | Subscription stats отсутствуют | `pg_stat_subscription`, `pg_stat_subscription_stats`, worker identity and reset |
| Logical decoding spill | Не собирается `pg_stat_replication_slots` | spill/stream counters и reset per logical slot |
| Slot xmin/catalog retention | Текущая slot schema не содержит horizon/safe headroom | `xmin`, `catalog_xmin`, `safe_wal_size`, invalidation reason, versioned nullable fields |
| Archive backlog and `pg_wal` growth | Archiver counters не показывают `.ready` queue или directory bytes | Bounded `pg_wal/archive_status` counts/oldest age, directory bytes and path-to-mount mapping |
| Base-backup progress | Нет `pg_stat_progress_basebackup` | Versioned progress rows plus walsender/PID bridge |
| Per-socket replication network | Host counters нельзя связать с replication socket | Bounded TCP_INFO per selected walsender socket, endpoint labels and sampling coverage |
| Cgroup PSI and throttle fraction | Сохраняется host PSI; в cgroup CPU нет `nr_periods` | cgroup cpu/memory/io pressure totals plus `nr_periods`, controller/version metadata |
| THP/compaction stalls | Текущий vmstat codec не содержит THP/compaction counters | `compact_stall/fail`, THP alloc/fallback counters and sysfs mode/defrag policy |
| NUMA/zone reclaim | Нет NUMA counters/config | node numastat, `numa_miss/foreign`, zone reclaim counters and `zone_reclaim_mode` |
| Filesystem inode headroom | mountinfo хранит только bytes | `f_files/f_favail` from statvfs with mount identity |
| jbd2/filesystem journal stalls | jbd2 и filesystem-specific stats не собираются | Versioned ext4/jbd2 trace or counters with device mapping; separate contracts for XFS/ZFS |
| Per-device PID attribution | `/proc/PID/io` не содержит device | Bounded eBPF/taskstats block-I/O events or cgroup isolation with explicit loss accounting |
| Deployment context | Нет источника deploy/release events | Typed bounded external event `{ts, service, version, action, source}` |

Roadmap input сначала получает самостоятельный collector/registry contract с
версиями, scope, правами, coverage и memory bounds. Только затем линза может
перейти в основной каталог.

## 7. Карта нормативных источников

| Семантика | Первичный источник |
|---|---|
| PG15-18 cumulative/dynamic stats, permissions, snapshot/reset and timing gates | [PG15](https://www.postgresql.org/docs/15/monitoring-stats.html), [PG16](https://www.postgresql.org/docs/16/monitoring-stats.html), [PG17](https://www.postgresql.org/docs/17/monitoring-stats.html), [PG18](https://www.postgresql.org/docs/18/monitoring-stats.html) |
| `pg_stat_io` introduced in PG16; checkpointer split in PG17; WAL I/O moved in PG18 | [PG16 release](https://www.postgresql.org/docs/16/release-16.html), [PG17 release](https://www.postgresql.org/docs/17/release-17.html), [PG18 release](https://www.postgresql.org/docs/18/release-18.html) |
| `pg_stat_statements`, planning/timing gates, identity, stats_since and deallocation | [PG18 extension docs](https://www.postgresql.org/docs/18/pgstatstatements.html), [PostgreSQL source](https://github.com/postgres/postgres/blob/REL_18_STABLE/contrib/pg_stat_statements/pg_stat_statements.c) |
| Vacuum/analyze/freeze/wraparound | [Routine vacuuming PG18](https://www.postgresql.org/docs/18/routine-vacuuming.html), [VACUUM PG18](https://www.postgresql.org/docs/18/sql-vacuum.html) |
| Replication LSN, lag and slots | [Monitoring PG18](https://www.postgresql.org/docs/18/monitoring-stats.html), [WAL internals](https://www.postgresql.org/docs/18/wal-internals.html), [`pg_replication_slots`](https://www.postgresql.org/docs/18/view-pg-replication-slots.html) |
| Lock edges and prepared transactions | [`pg_blocking_pids`](https://www.postgresql.org/docs/18/functions-info.html), [`PREPARE TRANSACTION`](https://www.postgresql.org/docs/18/sql-prepare-transaction.html) |
| Linux PSI | [Kernel PSI](https://docs.kernel.org/accounting/psi.html) |
| cgroup CPU/memory/I/O semantics | [Kernel cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html) |
| `/proc/diskstats` counters | [Kernel I/O statistics](https://docs.kernel.org/admin-guide/iostats.html) |
| `/proc/PID/io`, CPU and procfs fields | [Kernel procfs](https://docs.kernel.org/filesystems/proc.html) |
| Dirty/writeback thresholds | [Kernel VM sysctl](https://docs.kernel.org/admin-guide/sysctl/vm.html) |

Схемы, selectors и ограничения сбора для PgKronika задаются текущими registry
и source code. Внешние monitoring recipes этот контракт не определяют.
