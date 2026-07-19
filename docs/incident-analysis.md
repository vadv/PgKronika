# Инцидент-анализ PgKronika

Версия документа: draft-1, 2026-07-19.

Подсистема инцидент-анализа читает уже записанные сегменты, находит в метриках и
логах аномальные отрезки, сшивает совпавшие по времени в инциденты и готовит
диагностические линзы, которые эти инциденты аннотируют. Наружу она выходит одним
endpoint — `GET /v1/incidents` процесса `pg_kronika-web`.

Документ состоит из трёх частей. Часть 1 — для DBA и оператора: модель, чтение
ответа API и граница того, что вывод действительно доказывает. Часть 2 — для
разработчика: поток данных по модулям, контракт линзы и порядок активации. Часть 3 —
справочник каталога: все 59 линз с их полями.

**Каталог линз сейчас dormant: линзы описаны, но детекторы не активны.** Endpoint
кластеризует аномалии и отдаёт каталог как дорожную карту; findings он пока не
считает. Держите эту поправку при чтении обеих частей.

---

## Часть 1. Для DBA и оператора

### Что делает подсистема

Вход — сегменты одного источника (`source_id`) за запрошенный период. Работа идёт
в четыре шага:

1. **Скан аномалий.** По каждому числовому ряду (счётчик после честной дельты или
   gauge) скользящее окно сравнивается с остальным периодом робастным score
   (модифицированный z-score на медиане и MAD). Отрезок, где score вышел за порог, —
   это аномалия (episode). Скан ретроспективный: позиция сравнивается в том числе с
   более поздними точками. Это разбор завершённого периода, не потоковая тревога.
2. **Кластеризация.** Аномалии, отстоящие по времени не дальше порога `epsilon`,
   сшиваются в кластер. Кластер не растёт шире `max_cluster_span`.
3. **Инцидент.** Кластер поднимается до именованного объекта с устойчивым ключом
   (`incident_key`), своими членами-аномалиями и местом под findings.
4. **Аннотация линзами.** Линза читает ряды инцидента и выдаёт finding — гипотезу с
   ролью и уверенностью. Линза не входит в инцидент, она его комментирует. Этот шаг
   сейчас dormant.

### Модель: пять понятий

- **Аномалия (episode)** — атом. Отрезок времени, где метрика робастно отклонилась.
  Кандидат на разбор, не причина.
- **Кластер** — соседние по времени аномалии, сшитые в группу.
- **Инцидент** — кластер с идентичностью. Содержит свои аномалии-члены и findings.
- **Линза (lens)** — оценщик. Читает ряды по кластеру и задаёт один диагностический
  вопрос.
- **Finding** — ответ одной линзы: гипотеза с ролью (`lead`/`amplifier`/
  `downstream`/`coincident`), уверенностью (`low`/`medium`/`high`) и доказательством.

Цепочка: аномалия → кластер → инцидент → линза аннотирует → finding.

### Как читать `GET /v1/incidents`

Запрос: `GET /v1/incidents?source=<id>&from=<us>&to=<us>`. Время — unix-микросекунды.
Необязательные параметры: `window`, `step`, `threshold`, `eps_rel`, `epsilon`,
`max_cluster_span`, `section`.

Ответ — один JSON-объект. Значимые поля:

```json
{
  "source_id": 7,
  "from": 0, "to": 3600000000,
  "complete": false,
  "clustering_complete": true,
  "analysis_status": "incidents_detected",
  "incidents": [
    {
      "interval": { "from": 0, "to": 120000000 },
      "incident_key": "01000000...",
      "members": [
        { "logical_section": "pg_stat_archiver", "column": "archived_count",
          "identity": [], "from": 0, "to": 120000000 }
      ],
      "findings": [],
      "evaluation_complete": false,
      "finding_evaluation_status": "not_available"
    }
  ],
  "coverage_by_section": { "pg_stat_archiver": { "gaps": [] } },
  "data_age_seconds": 42,
  "catalog": { "...": "см. ниже" },
  "data_quality": { "...": "счётчики исключений" },
  "skipped": { "...": "что не влезло в лимиты" }
}
```

`analysis_status` — одно слово о том, что произошло:

| Значение | Смысл |
|---|---|
| `incidents_detected` | кластеры найдены |
| `calm` | данные разобраны, аномалий нет |
| `insufficient_data` | ни одна позиция окна не набрала данных для score |
| `partial` | часть секций или эпизодов не влезла в лимиты; разбор неполный |
| `no_data` | у источника нет units за период |
| `missing_node_identity` / `conflicting_node_identity` | не удалось однозначно определить узел |

Поле `complete` в этой версии всегда `false`: полный анализ включал бы findings,
которых пока нет. За честность разбора кластеризации отвечает `clustering_complete`.

Блок `catalog` описывает состояние линз:

```json
"catalog": {
  "status": "dormant",
  "requirements_status": "incomplete",
  "diagnosis_available": false,
  "scope": "anomaly_clustering_only",
  "applied": [],
  "dormant": [ /* 28 метрических линз */ ],
  "log_dormant": [ /* 31 лог-линза */ ]
}
```

Каждая запись `dormant`/`log_dormant`:

| Поле | Значение |
|---|---|
| `lens_id` | стабильный id линзы (`shared_buffer_misses`) |
| `domain` | `pg` или `os` |
| `title` | русское имя проблемы |
| `detects` | вопрос, который задаёт линза |
| `confidence` | потолок уверенности (`low`/`medium`/`high`) |
| `awaiting` | список capability-токенов, которых линзе не хватает |
| `requirements_status` | `incomplete`, пока не собраны все `awaiting` |

`applied` пуст, `diagnosis_available` — `false`, `finding_evaluation_status` каждого
инцидента — `not_available`. Это не ошибка ответа, а текущее состояние каталога.

### Честная граница атрибуции

Подсистема объясняет совместно наблюдавшиеся аномалии за завершённый период. Она не
измеряет root cause. PgKronika не семплирует активные сессии и wait events с частотой
ASH, поэтому у неё нет аддитивного бюджета DB time и она не приписывает долю задержки
конкретному запросу, ожиданию или ресурсу. По форме результата (именованные находки с
ролями поверх совпавших симптомов) подсистема близка к Oracle ADDM, но доказательная
база слабее: без измеренного DB time атрибуция остаётся эвристикой — правила плюс
временной порядок членов кластера.

Отсюда три правила чтения:

- **Уверенность — потолок, не вероятность.** `high` в каталоге означает предельно
  достижимую уверенность линзы, а не «вероятность 90%». Реальный finding может выйти
  ниже потолка, если доказательство слабее.
- **Роль — гипотеза направления.** `lead` значит «наблюдение могло предшествовать и
  поддержано документированным механизмом», а не «доказанная причина». Без прямого
  structural-доказательства или общей шкалы времени роль опускается до `coincident`.
- **`root cause` в API и UI не появляется.** Единственное более сильное свидетельство
  в текущих данных — сохранённое ребро `blocked_by` из `pg_locks`: оно показывает, кто
  мешал backend получить heavyweight lock в момент снимка. Даже это ребро доказывает
  блокирование, а не объясняет, почему держатель не завершил транзакцию.

### Текущий статус: dormant — дорожная карта, не детектор

Каталог перечисляет вопросы, которые подсистема научится задавать, и данные, которых
для этого не хватает. Поле `awaiting` у каждой линзы — это её недостающие предпосылки.
Пока `awaiting` не пуст, линза не эмитит honest finding.

На практике это значит: `GET /v1/incidents` уже полезен как детектор со-произошедших
аномалий и как читаемая карта будущей диагностики, но `findings` в ответе — всегда
пустой список. Не принимайте пустой `findings` за «инцидент без причин»: причины пока
не считаются вовсе.

### Каталог линз по доменам

Полные таблицы — в части 3. Обзор по темам:

**Метрические линзы PostgreSQL (`domain=pg`).**

- Запросы и планы: `query_workload_shift`, `plan_change`, `stale_statistics`,
  `backend_io_latency`, `shared_buffer_misses`.
- Vacuum и freeze: `vacuum_backlog`, `xid_wraparound_risk`, `hot_update_failure`,
  `xmin_horizon_hold`.
- WAL, чекпоинты, репликация: `wal_amplification`, `requested_checkpoints`,
  `replication_lag`, `slot_wal_retention`, `wal_archiving_failure`,
  `sync_replication_wait`.
- Блокировки и соединения: `lock_wait_graph`, `connection_saturation`,
  `internal_wait_concentration`.
- Временные файлы: `temp_spill`.

**Метрические линзы OS/cgroup (`domain=os`).**

- CPU: `cpu_saturation`, `cgroup_cpu_throttling`.
- Память: `memory_reclaim`, `cgroup_memory_limit`.
- Диск и I/O: `block_device_latency`, `writeback_pressure`, `io_contender`,
  `filesystem_space`.
- Сеть: `network_errors`.

**Лог-линзы (`domain=pg`, кроме `kernel_oom_victim`).** Держатся отдельным
под-каталогом (`log_dormant`), потому что событийный вход в движок ещё не готов.

- Доступность и ресурсы: `oom_kill`, `backend_crash`, `panic_shutdown`,
  `disk_full_log`, `out_of_memory_log`, `connection_slots_exhausted`,
  `lock_table_exhaustion`, `shared_memory_alloc_failure`.
- Целостность: `data_corruption_log`, `block_io_integrity_suspicion`,
  `wal_integrity_log`.
- Блокировки и производительность запросов: `deadlock`, `lock_wait_logged`,
  `lock_timeout_log`, `statement_timeout_log`, `temp_file_spill_log`,
  `slow_query_logged`, `serialization_failure`, `idle_in_transaction_abort`.
- Обслуживание: `checkpoint_too_frequent`, `aggressive_autovacuum_wraparound`,
  `autovacuum_cancel`.
- Безопасность логов: `auth_password_failures`, `pg_hba_rejections`,
  `permission_denied_burst`, `connection_storm_log`.
- Репликация и архивация: `walsender_disconnect`, `walreceiver_disconnect`,
  `recovery_conflict`, `archive_command_failure`.
- OS: `kernel_oom_victim`.

---

## Часть 2. Для разработчика

### Поток данных по модулям

Событие превращается из строк сегмента в ответ API через шесть модулей. Ядро
инцидента (`incident/*`) не знает про транспорт: подготовка входа и JSON-адаптер
живут снаружи.

```text
сегменты (kronika-reader)
  │
  ▼  anomaly.rs            скан окон, robust score, episodes()
  │                        → EpisodeHit { key, column, episode }
  ▼  incident_input.rs     адаптер: diff счётчиков + gauge-ряды,
  │                        scan_section, ранжирование эпизодов,
  │                        счётчики InputQuality
  │                        → PreparedInput { episodes, series, coverage, ... }
  ▼  incident/cluster.rs   sweep-line группировка по epsilon/max_span
  │                        → ClusterOutcome { clusters, span_splits }
  ▼  incident/engine.rs    analyze(): dispatch линз по секциям кластера,
  │                        FindingSink, сортировка, IncidentKeyV1
  │                        → EngineOutcome { incidents, span_splits, complete, skipped }
  ▼  incident_response.rs  build_response(): движок → JSON
  │
  ▼  handlers/incidents.rs GET /v1/incidents, лимиты, spawn_blocking
```

Карта остальных модулей ядра:

| Модуль | Ответственность |
|---|---|
| `incident/model.rs` | `EpisodeRefV1`, `IncidentKeyV1` (канонический байтовый ключ), `IdentityValue` |
| `incident/series.rs` | `Series`, `SeriesSet` — валидированные числовые ряды одного запроса |
| `incident/dispatch.rs` | `SectionColumn`, `WorkBudget`, `section_index`, `candidate_lenses` |
| `incident/lens.rs` | trait `Lens` |
| `incident/evidence.rs` | `Finding`, `Role`, `Confidence`, `FindingDraft`, `FindingSink` |
| `incident/lenses.rs` | dormant-каталог: `DormantLens`, `MissingCapability`, `Domain` |

Скан (`anomaly.rs`) считает робастный score относительно всего периода, включая более
поздние точки, — non-causal. Позиция получает score, только если у неё не меньше 20
опорных и 3 оконных точек; иначе она попадает в `unevaluated_positions`. Порог по
умолчанию — 3.5 робастных сигмы.

Кластеризация (`cluster.rs`) не зависит от порядка входа: эпизоды сортируются, затем
sweep-line сшивает соседей в пределах `epsilon` и режет кластер по `max_cluster_span`.
Разрез по ширине считается в `span_splits`; естественный разрыв длиннее `epsilon` не
считается.

Движок (`engine.rs`) строит `section_index` из входов линз и для каждого кластера
зовёт только те линзы, чьи секции присутствуют в кластере. Работа ограничена
`WorkBudget`, число findings и evidence rows — своими потолками; при исчерпании
инцидент помечается `evaluation_complete=false`, а запрос — `complete=false`. Findings
внутри инцидента сортируются детерминированно (уверенность по убыванию, затем роль,
`lens_id`, scope, evidence), чтобы ключ и порядок не зависели от регистрации линз.

`IncidentKeyV1` — канонический ключ версии 1: `node_self_id`, интервал и отсортированные
члены кодируются в байты с длинными префиксами, поэтому ключ не зависит от порядка
эпизодов и не путает границы текстовых идентичностей.

### Контракт `trait Lens`

Линза — чистый оценщик поверх предзагруженных рядов. Весь вывод и все прочитанные
точки проходят через `sink`, чтобы движок мог считать бюджет.

```rust
pub(crate) trait Lens {
    fn id(&self) -> &'static str;
    fn inputs(&self) -> &'static [SectionColumn];
    fn confidence_cap(&self) -> ConfidenceCap;
    fn evaluate(
        &self,
        cluster: &Cluster,
        series: &SeriesSet,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit>;
}
```

- `id` — стабильный идентификатор, он же попадает в JSON и в `skipped`.
- `inputs` — секции и колонки, которые линза читает. `section_index` строится из этого
  списка; линза называет логическую секцию, а не version-specific `type_id`.
- `confidence_cap` — потолок уверенности линзы.
- `evaluate` — читает ряды членов кластера через `series.get(reference, sink)` и эмитит
  `Finding` через `sink.emit(...)`. Возврат `Err(LimitHit)` означает, что линза уперлась
  в бюджет.

Уверенность и роль решает не сама линза, а `evidence.rs` при сборке finding:

- `confidence = confidence_cap.min(evidence_ceiling(evidence))`. Пустое доказательство
  даёт `low` при любом потолке. Достичь `high` может только `Direct`-доказательство;
  `Ratio`/`Gauge`/`Counter`/`Event` упираются в `medium`.
- Роль `lead`/`downstream` сохраняется, только если доказательство подтверждает
  направление структурно (сейчас это единственный вид — ребро `blocked_by`) или если
  часы источников в одном домене (`ClockRelation::SameDomain`) выдали temporal-permit.
  Иначе роль опускается до `coincident`. Endpoint сейчас работает с
  `ClockRelation::Unknown`, поэтому без lock-ребра направление не удержать.

### Как активировать dormant-линзу

1. Написать `struct` линзы и `impl Lens` для неё.
2. Объявить `inputs()` — секции и колонки, которые линза читает. От этого зависит,
   на каких кластерах движок её вызовет.
3. Реализовать `evaluate()`: прочитать нужные ряды через `sink`, посчитать отклонение,
   собрать доказательство (`Vec<Evidence>`) и эмитить `Finding` через
   `sink.emit(FindingDraft::new(role, scope, evidence, temporal_permit))`.
4. Обеспечить недостающие capability из `awaiting`. Без них честный вывод невозможен:
   например, `lock_wait_graph` требует `sampled_blocked_by_edges` и
   `lock_snapshot_coverage`; без `Direct`-ребра его роль опустится до `coincident`, а
   уверенность — до `medium`, что делает линзу с потолком `high` бессмысленной.
5. Подключить линзу в `analyze()`. Сейчас `handlers/incidents.rs` вызывает

   ```rust
   analyze(prepared.episodes, &prepared.series, &[], &config)
   ```

   с пустым срезом линз. Активная линза добавляется в этот срез. Движок отвергает
   дубликаты `id` типизированной ошибкой.

### Capability-токены

`MissingCapability` (в `lenses.rs`) — предпосылки данных, которых линзам не хватает.
Токен в `awaiting` означает, что без этой возможности линза не даёт honest finding.

| Токен | Назначение |
|---|---|
| `typed_counter_deltas` | Честные дельты накопительных счётчиков из `kronika-analytics::diff`; правило не вычитает значения само. |
| `typed_gauge_samples` | Валидированные снимки gauge-колонок (не дифференцируются). |
| `paired_interval_inputs` | `delta(A)` и `delta(B)` на одном наборе интервалов до деления — честное отношение. |
| `source_period_provenance` | Границы периода для периодических источников; для событийного потока неприменимо. |
| `request_input_coverage` | Покрытие и разрывы входа в пределах окна запроса. |
| `cross_section_entity_join` | Индексированный join между секциями по объявленным ключам: PID+backend start, dbid, relid, путь cgroup, WAL endpoint. |
| `track_planning_gate` | Queryable gate для колонок планирования (`track_planning`), которого пока нет. |
| `store_plans_bridge` | Мост к хранилищу планов и `planid`. |
| `sampled_blocked_by_edges` | Рёбра `blocked_by` из снимков `pg_locks` — единственное прямое structural-доказательство. |
| `lock_snapshot_coverage` | Покрытие снимков блокировок на интервале. |
| `sampled_activity_rows` | Семплированные строки `pg_stat_activity`. |
| `pid_cgroup_mapping` | Сопоставление PID → cgroup. |
| `incident_log_event_input` | Ограниченная типизированная событийная модель на входе движка, отдельно от периодического anomaly-пути. |
| `log_detail_continuation` | Качество ассоциации продолжений `DETAIL`/`CONTEXT` с событием. |
| `log_source_coverage` | Непрерывность tail и источника лога. |
| `effective_log_config_coverage` | Покрытие эффективных GUC логирования на окне. |
| `sensitive_log_redaction` | Редакция чувствительных полей (SQL, IP, user/db, archive command) до сериализации. |
| `source_clock_provenance` | Clock domain, timezone/skew и качество timestamp события. |
| `kernel_oom_victim_evidence` | Запись ядра о victim PID при OOM-kill. |
| `structured_log_identity` | Severity, SQLSTATE, PID/session, provenance формата и association quality как типизированные поля. |

### Почему лог-линзы держатся отдельным под-каталогом

Метрические и лог-линзы лежат в разных константах: `DORMANT_CATALOG` (28) и
`LOG_DORMANT_CATALOG` (31). Разделение — по типу входа, а не по владельцу: оба
под-каталога остаются в приватном web-модуле и держат свой compile-time bound
(`MAX_DORMANT_LENSES = 28`, `MAX_LOG_DORMANT_LENSES = 40`).

Причина честная: событийный вход в incident-движок ещё не готов. Адаптер
(`incident_input.rs`) сейчас приводит колонки секций к cumulative/gauge `f64`-рядам и
anomaly-эпизодам. Severity, SQLSTATE, текст, entity и ассоциация в `SeriesSet` не
попадают — событие теряет всё, кроме числового следа. Периодический anomaly-путь не
подходит событийному потоку: ему нужны отдельные bounded event rows, event-aware
dispatch и coverage lookup. Пока их нет, лог-линза может быть каталогизирована, но не
активирована; её `awaiting` всегда содержит `incident_log_event_input`.

`domain` лог-линз тоже честный: PostgreSQL-emitted ENOSPC остаётся `pg` с OS-корреляцией,
а `kernel_oom_victim` — `os`, потому что kernel OOM victim record приходит от ядра.
Один общий `pg` скрыл бы источник доказательства.

---

## Часть 3. Справочник каталога

Источник истины — `bins/pg_kronika-web/src/incident/lenses.rs`. Порядок строк
совпадает с порядком в коде. Все линзы dormant; статус в отдельную колонку не выведен.

### Метрические линзы (28)

| lens_id | domain | title | Что детектит | confidence |
|---|---|---|---|---|
| `query_workload_shift` | pg | Сдвиг профиля запроса | У нормализованного запроса изменились частота, работа на вызов или время исполнения. | medium |
| `plan_change` | pg | Смена плана запроса | Деградация запроса совпала с появлением или сменой `planid`. | medium |
| `temp_spill` | pg | Спил во временные файлы | Рост работы через временные блоки и файлы. | medium |
| `stale_statistics` | pg | Устаревшая статистика планировщика | `n_mod_since_analyze` высок, свежего analyze нет, план/работа поехали. | medium |
| `vacuum_backlog` | pg | Отставание vacuum | Растёт долг мёртвых кортежей, cleanup не успевает. | medium |
| `xid_wraparound_risk` | pg | Приближение wraparound XID/MXID | Headroom по возрасту XID/MXID тает, близко к форсированному aggressive vacuum. | medium |
| `hot_update_failure` | pg | Срыв HOT-обновлений | Доля non-HOT updates растёт вместе с работой по индексам и WAL. | medium |
| `requested_checkpoints` | pg | Внеплановые контрольные точки | Растёт доля requested checkpoints и их write/sync-работа. | medium |
| `wal_amplification` | pg | Раздувание WAL и FPI | Растут WAL bytes на запись, доля FPI, `wal_buffers_full`. | medium |
| `shared_buffer_misses` | pg | Промахи shared buffers | Растёт доля промахов shared buffers по базе/отношению/контексту. | medium |
| `backend_io_latency` | pg | Задержка I/O внутри PostgreSQL | Растёт время на операцию или блок (`pg_stat_io`, PG16+). | medium |
| `lock_wait_graph` | pg | Граф ожидания блокировок | Кто блокировал ожидающего в момент снимка (`blocked_by` из `pg_locks`). | high |
| `xmin_horizon_hold` | pg | Удержание горизонта xmin | Долгая или idle-in-transaction транзакция держит vacuum-горизонт. | medium |
| `connection_saturation` | pg | Насыщение по соединениям | Backends подходят к `max_connections`, churn растёт при падении throughput. | medium |
| `replication_lag` | pg | Отставание физической репликации | На каком LSN-этапе растёт байтовый разрыв (sent/write/flush/replay). | medium |
| `slot_wal_retention` | pg | Удержание WAL слотом репликации | Слот держит растущий WAL, `retained_bytes` со склоном вверх. | medium |
| `wal_archiving_failure` | pg | Ошибки архивации WAL | Подтверждённые ошибки archive command/library (`failed_count`). | medium |
| `sync_replication_wait` | pg | Ожидание синхронной репликации | Backends висят на `wait_event='SyncRep'` при настроенной синхронной репликации. | medium |
| `internal_wait_concentration` | pg | Концентрация внутренних ожиданий | Растёт доля active backends на `LWLock`/`BufferPin`/`IO` wait. | low |
| `cpu_saturation` | os | Насыщение CPU хоста | Runnable pressure, iowait, steal. | medium |
| `cgroup_cpu_throttling` | os | Троттлинг CPU в cgroup | Реальный throttling cgroup при доступном CPU хоста. | medium |
| `memory_reclaim` | os | Нехватка памяти хоста | Memory pressure, direct reclaim, swap, OOM. | medium |
| `cgroup_memory_limit` | os | Лимит памяти cgroup | Достижение `memory.high`/`max`/OOM в cgroup. | medium |
| `block_device_latency` | os | Задержка блочного устройства | Растут время завершения и очередь устройства. | medium |
| `writeback_pressure` | os | Давление dirty/writeback | Повышенные Dirty/Writeback совпали с write/sync-задержкой PostgreSQL. | low |
| `io_contender` | os | Внешний потребитель I/O | Какой процесс или cgroup нарастил block I/O рядом с давлением. | medium |
| `filesystem_space` | os | Исчерпание места ФС | Точка монтирования близка к исчерпанию байтов. | high |
| `network_errors` | os | Сетевые ошибки и ретрансмиты | Растут счётчики ошибок интерфейса и TCP-ретрансмиссий. | low |

### Лог-линзы (31)

Восемь линз из батча 1 — core (`CORE_LOG_LENS_IDS`): их одна запись самодостаточна как
finding, поэтому активировать их стоит первыми. Они помечены **core** в колонке
приоритета.

| lens_id | domain | title | Что детектит | confidence | Приоритет |
|---|---|---|---|---|---|
| `oom_kill` | pg | SIGKILL бэкенда | Был ли backend завершён сигналом 9? Жертва kernel-OOM — отдельный сигнал, signal 9 её не доказывает. | high | core |
| `backend_crash` | pg | Аварийное завершение backend | Упал ли backend по сигналу (SIGSEGV/SIGABRT) с каскадом восстановления? | high | core |
| `panic_shutdown` | pg | PANIC / аварийная остановка | Была ли запись severity PANIC и отдельный crash/restart? Не помечает повреждение данных автоматически. | high | core |
| `disk_full_log` | pg | Нет места на диске (по логу) | Отказала ли запись из-за ENOSPC? | high | core |
| `out_of_memory_log` | pg | Ошибка аллокации PostgreSQL (по логу) | Отказала ли аллокация PostgreSQL (SQLSTATE 53200)? Это ошибка аллокатора, не исчерпание физической RAM. | high | core |
| `connection_slots_exhausted` | pg | Исчерпание слотов соединений | Отклонялись ли подключения по лимиту? | high | core |
| `deadlock` | pg | Взаимоблокировка | Обнаружил ли PostgreSQL цикл блокировок с жертвой? Факт события, не доказанная причина инцидента. | high | core |
| `data_corruption_log` | pg | Повреждение данных (по логу) | Дала ли сбой завершённая проверка checksum/страницы? Не generic ошибка чтения или I/O. | high | core |
| `lock_wait_logged` | pg | Длительное ожидание блокировки | Кто и как долго ждал блокировку до её выдачи? | medium | |
| `lock_timeout_log` | pg | Отмена по lock_timeout | Отменялись ли запросы по `lock_timeout`? Факт отмены, не доказанная причина инцидента. | medium | |
| `statement_timeout_log` | pg | Отмена по statement_timeout | Упирались ли запросы в `statement_timeout`? Факт отмены; таймаут не доказывает медленный сервер. | medium | |
| `temp_file_spill_log` | pg | Пролив во временные файлы | Сливались ли сортировки/хеши в temp-файлы, какого размера? | medium | |
| `slow_query_logged` | pg | Медленный запрос (по логу) | Превышен ли настроенный порог длительности конкретным запросом? Сам по себе не аномалия. | medium | |
| `serialization_failure` | pg | Сбой сериализации транзакций | Всплеск ли откатов по конфликту сериализации? | medium | |
| `idle_in_transaction_abort` | pg | Обрыв по idle-in-transaction | Убивались ли зависшие в транзакции сессии? | medium | |
| `checkpoint_too_frequent` | pg | Слишком частые контрольные точки | Форсирует ли WAL-давление внеплановые чекпоинты? | medium | |
| `aggressive_autovacuum_wraparound` | pg | Агрессивный autovacuum против wraparound | Запускался ли аварийный anti-wraparound freeze? | medium | |
| `autovacuum_cancel` | pg | Отмена autovacuum под блокировкой | Отменяется ли autovacuum конфликтующими локами (DDL)? | medium | |
| `auth_password_failures` | pg | Всплеск неверных паролей | Всплеск ли отказов аутентификации по паролю? | medium | |
| `pg_hba_rejections` | pg | Отказы по pg_hba | Стучится ли неизвестный хост/БД/пользователь мимо pg_hba? | medium | |
| `permission_denied_burst` | pg | Всплеск отказов доступа (RBAC) | Всплеск ли `permission denied` (обычно кривой деплой грантов)? | low | |
| `connection_storm_log` | pg | Шторм подключений | Резкий churn коннектов без упора в лимит? | medium | |
| `archive_command_failure` | pg | Сбой archive_command (по логу) | Почему падает архивация WAL (exit-код, stderr)? | medium | |
| `walsender_disconnect` | pg | Обрыв walsender (primary) | Оборвал ли primary поток репликации по timeout walsender? | medium | |
| `walreceiver_disconnect` | pg | Обрыв walreceiver (standby) | Оборвался ли приём на standby (walreceiver timeout / could not receive data)? | medium | |
| `recovery_conflict` | pg | Конфликт восстановления на реплике | Отменяются ли запросы на реплике конфликтом с replay? | medium | |
| `wal_integrity_log` | pg | Проблемы целостности WAL | Сбой валидации WAL из archive/stream? Локальный конец WAL (invalid record length в pg_wal) легитимен, не finding. | high | |
| `kernel_oom_victim` | os | Жертва OOM-killer ядра | Убил ли OOM-killer ядра конкретный процесс (victim PID)? Signal 9 у backend этого не доказывает. | medium | |
| `lock_table_exhaustion` | pg | Исчерпание таблицы блокировок | Отказала ли операция из-за нехватки shared memory под таблицу блокировок ("out of shared memory")? | high | |
| `shared_memory_alloc_failure` | pg | Сбой аллокации разделяемой памяти | Не удалось выделить или изменить сегмент разделяемой памяти (DSM)? | high | |
| `block_io_integrity_suspicion` | pg | Подозрение на ошибку block I/O | Generic ошибка чтения блока (short/torn read, zero page), не подтверждённая проверкой checksum? | low | |
