# PgKronika — overview-index и timeline API

Версия: 0.3  
Дата: 2026-07-22  
Статус: архитектурная спецификация parity v1

## 1. Цель и статус решений

PgKronika должна быстро строить событийный обзор и health-line для произвольного временного диапазона. Повторный запрос, запрос после перезапуска процесса и многодневный диапазон не должны заново декодировать тела PGM-сегментов, если для них существует валидный fact index. Свежие данные из `active.parts` должны появляться с задержкой не больше нескольких refresh-циклов.

Parity v1 включает reader-owned persistent on-disk fact index. In-memory cache без дискового слоя может быть промежуточным этапом разработки, но не считается parity-релизом.

PGM остаётся единственным источником истины. Writer, формат PGM и протокол seal не меняются. Индекс принадлежит reader-слою, хранит formula-neutral facts и удаляется без потери исходных данных.

reftool используется как проверенный reference implementation для четырёх наблюдаемых свойств: быстрых повторных запросов, тёплого чтения после restart, live-обновления и многодневного timeline. PgKronika не копирует его writer-computed health/charts, optimistic missing semantics, интерполяцию gaps, ошибки cadence/reset/chunk boundary и нестабильную identity событий.

Числовые health-кривые и продуктовые пороги требуют отдельной калибровки. Эта спецификация фиксирует их входы, алгебру, coverage, версионирование и ограничения, но не выдаёт непроверенные пороги за доказанную модель здоровья.

Нормативные слова «обязан», «нельзя» и «допускается» задают контракт v1. Псевдокод описывает wire и storage semantics, а не Rust ABI.

## 2. Определения

| Термин | Значение |
| --- | --- |
| PGM | Иммутабельный sealed-сегмент PgKronika, источник истины для закрытого диапазона. |
| Active part | Завершённый CRC-валидный frame в `active.parts`. Незавершённый tail не считается частью. |
| Segment descriptor | Content-bound identity PGM, вычисленная из его catalog/tail/length и source scope. |
| Segment facts | Formula-neutral индекс одного sealed PGM: retained observations, timestamped metric facts, reset/gap/coverage и provenance. |
| Live builder | Единственный mutable lossless builder, сворачивающий завершённые active parts ровно один раз. |
| Live view | Иммутабельный снимок live facts с journal generation и folded watermark. |
| Index view | Атомарный снимок ordered sealed descriptors и одной точной live generation. Все части одного запроса читаются из одного view. |
| EventObservation | Сохранённое PGM-наблюдение в форме источника: individual row, grouped row или gap. |
| EventFact | Policy-neutral нормализованный факт, выведенный из одного или нескольких observations/samples с явной provenance. |
| NotablePolicy | Версионированная чистая проекция, выбирающая и ранжирующая EventFacts для `/events` и preview. |
| IncidentDiagnosis | Отдельный корреляционный вывод о возможной причине с evidence и confidence. Не является observation или fact. |
| Retained exactness | Точность относительно строк и counts, которые фактически дошли до PGM. Она не означает полноту исходного PostgreSQL log. |
| Projection | Health, notable, digest, downsample или HTTP-представление, вычисленное из facts текущей policy version. |
| Source scope | Стабильная в пределах deployment identity набора PGM, отделяющая одинаковые `source_id` и имена файлов в разных store roots. |

## 3. Продуктовый scope и parity contract

### 3.1 Входит в parity v1

- `GET /v1/timeline/overview` для компактной событийной и health-сводки диапазона.
- `GET /v1/timeline/health` для health-line с честным per-point coverage.
- `GET /v1/timeline/events` для стабильной пагинации notable observations/facts.
- Reader-owned per-segment fact files в отдельном cache directory.
- Инкрементальный lossless live builder для завершённых active parts.
- Raw PGM fallback и ленивый rebuild отсутствующего, несовместимого или повреждённого cache.
- Bounded memory caches для fact blocks, projections и exact responses.
- Memory-only fallback при невозможности писать cache directory.
- Bounded, cancellation-safe cold builds и per-key single-flight.
- Quota, GC, metrics, corruption tests и restart benchmarks.

### 3.2 Наблюдаемый parity contract

1. Повторный identical sealed query в одном процессе не читает тела PGM и не повторяет projection work при exact response hit.
2. После process restart валидные fact files обслуживают sealed interior без PGM body reads и section decode.
3. Новый range, step или фильтр поверх уже существующих facts не требует PGM decode, если сохранённых dimensions достаточно.
4. Многодневный запрос читает только пересекающиеся компактные blocks с bounded parallelism.
5. Завершённый active frame становится виден не позже установленного live freshness gate; pending/torn tail отмечается отдельно.
6. Seal, разбиение на parts/segments и порядок merge не меняют retained facts, coverage или результат одной policy version.
7. Ни один пропуск required data не превращается в `score=1.0`.
8. Numeric health не обязан совпадать с reftool: сравниваются signal coverage, отсутствие false-green и производительность compact read path.

### 3.3 Границы v1

- Новые collectors и новые PGM sections не требуются.
- Текущий stderr source остаётся bounded и grouped; overview не обещает восстановить отброшенные строки.
- Историческая причина incident не выводится из одного token/signal/category.
- Cache не становится архивом и не продлевает retention PGM.

## 4. Инварианты и честность данных

### 4.1 Источник истины и disposable cache

1. PGM и valid completed active parts — единственные источники фактов.
2. Fact file versioned и disposable. Missing, incompatible, corrupt, oversized или wrong-source file игнорируется и перестраивается из PGM.
3. In-place migration fact files запрещена. Новая несовместимая версия получает новый key/namespace.
4. Cache read/write/GC failure не превращает корректный вычисленный ответ в ошибку.
5. Source PGM failure не маскируется как cache miss. Он становится source gap или typed source error.
6. Удаление всех derived files влияет только на latency первого чтения.

### 4.2 Три уровня exactness

Ответ обязан различать:

| Уровень | Что гарантируется |
| --- | --- |
| `fact_exact` | Index path семантически равен forced raw decode для тех же PGM rows и той же extractor version. |
| `retained_exact` | Каждая сохранённая observation/group учитывается ровно один раз, включая `occurrence_count`. |
| `source_complete` | Все физические source events были собраны. Для текущего log source это обычно нельзя доказать. |

`pg_log_errors` хранит группы `(normalized pattern, severity, sqlstate)` с timestamp первой occurrence и `count`; за collection cycle остаётся не больше 32 групп. Lifecycle observations также capped. Tailer, parser и dictionary имеют собственные bounds и gap signals. Поэтому `/events` точен над retained observations/groups, но не над физическими строками stderr.

Grouped row остаётся одной observation с `occurrence_count`. Её нельзя синтетически разворачивать в N событий: timestamps, PID и порядок остальных occurrences неизвестны.

### 4.3 Никаких ложных нулей и зелёных gaps

- Missing, unsupported, not-collected и measured zero — разные состояния.
- Пустой health bucket имеет `score=None`, а не `1.0`.
- Отсутствующий factor не создаёт нулевой penalty.
- Log/collector caps переводят event completeness в `partial`, даже если temporal sampling непрерывный.
- UI и API не интерполируют health через gaps.
- Counter reset/decrease/gap рвёт interval; он не превращается в zero rate.

### 4.4 Canonical state не бывает намеренно lossy

Canonical sealed facts и canonical live facts имеют одинаковую семантику. Response caps, top-N и preview limits применяются только после построения полного retained fact set.

Если hard memory/build bound всё же не позволяет удержать canonical live state:

1. builder переходит в `Incomplete`;
2. response публикует причину и loss coverage;
3. promotion live state в sealed fact file запрещён;
4. sealed segment строится заново из PGM;
5. request может восстановить retained exactness прямым bounded fold active parts;
6. без такого fold `/events` честно возвращает partial live interval.

### 4.5 Checked arithmetic

Counts и lengths складываются checked-операциями. Silent saturation запрещена. Overflow делает block/index uncacheable или response incomplete с machine-readable reason; он не выдаётся за точное значение.

## 5. Сквозной поток данных

```text
active.parts completed frames
        │ RefreshDelta(journal generation, PartId, watermark, damage)
        ▼
mutable lossless LiveBuilder
        │ publish immutable snapshot
        ▼
LiveView ─────────────────────────────────────────────┐
                                                     │
sealed PGM -> SegmentDescriptor -> memory/disk lookup │
        │                         │                    │
        │                         └-> cold PGM build   │
        │                              + best-effort persist
        ▼                                              ▼
ordered sealed SegmentFacts ───────────────────> immutable IndexView
                                                      │
                           capture one generation per request
                                                      ▼
               selective facts + left/right boundary halo
                                                      │
                  current health/notable/response projections
                                                      ▼
                       overview / health / events response
```

### 5.1 Refresh delta

Reader обязан публиковать semantic delta, а не только изменившуюся длину файла:

```text
RefreshDelta {
  previous_view_generation: u64,
  new_view_generation: u64,
  sealed_added: Vec<SegmentDescriptor>,
  sealed_removed: Vec<SegmentDescriptor>,
  journal: JournalDelta {
    generation_id: JournalGenerationId,
    previous_valid_len: u64,
    new_valid_len: u64,
    completed_parts: Vec<PartDescriptor>,
    transition: Append | Reset | Replaced | Uncertain,
    tail_pending: Option<ByteRange>,
    damages: Vec<SourceDamage>,
  },
}
```

`generation_id` меняется при inode/device transition, truncation, replacement, metadata discontinuity или любом переходе, который нельзя доказать как append. Equal-length rewrite нельзя считать unchanged. `Uncertain` всегда переводит live state в rebuild.

`PartId` — idempotency key внутри journal generation. Он связывает frame position, exact part catalog/content descriptor и длину. Повторная доставка одного `PartId` не меняет builder.

### 5.2 Query flow

Один request:

1. Захватывает один `Arc<IndexView>`.
2. Валидирует `[from_us,to_us)`, limits и выбранные sources.
3. Строит ordered plan sealed descriptors, одной live generation и boundary halo.
4. Проверяет exact response cache.
5. Загружает нужные fact blocks из memory/disk; miss запускает bounded single-flight build.
6. Обрезает observations/samples по точному диапазону и применяет reducer semantics §6.
7. Применяет текущие health/notable policies.
8. Формирует coverage/loss и response metadata из того же view.
9. Кеширует projection/response только под полным `FactSetId`.

Запрос никогда не смешивает новый sealed set со старым live view.

### 5.3 Multi-segment merge

- Segment selection использует source snapshot как authoritative range catalog; cache directory не сканируется на каждый request.
- Facts merge-ятся в deterministic range/source/provenance order.
- Additive counts складываются checked.
- Coverage merge-ится как union half-open intervals, а не как сумма ratios.
- Event sets union-ятся по stable ID; logical crash dedup является отдельной projection policy.
- Gauge/counter samples сначала объединяются в ordered series, затем редуцируются. Готовые penalties и scores не merge-ятся.

## 6. Время, диапазоны, buckets и reductions

### 6.1 Единый time contract

- Все новые HTTP ranges и health buckets — half-open `[from_us,to_us)`.
- `from_us < to_us`; timestamps — signed Unix microseconds UTC.
- Event с sort timestamp `t` принадлежит ровно одному bucket, где `start <= t < end`.
- PGM catalog `max_ts` остаётся inclusive metadata. Planner преобразует его, но wire contract не становится inclusive.
- `effective_range` совпадает с requested range. Последний health bucket может быть короче `effective_step_us`; range не округляется молча.

### 6.2 Step semantics

```text
effective_step_us = max(
  requested_step_us,
  ceil((to_us - from_us) / MAX_HEALTH_POINTS)
)
```

Если `step_us` не задан, policy выбирает его из диапазона и возвращает фактическое значение. Step не объявляется более точным, чем source cadence: sparse points получают partial/unknown coverage, а не invented samples.

Факты на disk хранят natural timestamps, а не final buckets. Поэтому новый step не перестраивает fact files.

### 6.3 Boundary halo

Для диапазона planner может читать:

- последний sample каждой нужной series перед `from_us`;
- первый sample после `to_us`;
- reset/gap markers между halo и диапазоном.

Halo нужен для counter pair, time-weighted gauge и state transition. Он не включается в event/count response и входит в `FactSetId`, потому что влияет на reduction.

### 6.4 Event и grouped-count slicing

- Individual observation включается по своему exact/fallback `sort_ts_us`.
- Grouped log row включается целиком по сохранённому first/fallback timestamp.
- `occurrence_count` grouped row не распределяется по времени: PGM не содержит timestamps остальных occurrences.
- Если source знает только interval, observation пересекает range как interval fact и не превращается в точечное событие.

### 6.5 Counter semantics

Каждая adjacent pair одной series образует candidate interval:

```text
CounterInterval {
  previous: (ts_us, value, reset_epoch),
  current:  (ts_us, value, reset_epoch),
  delta: u64,
  duration_us: u64,
  quality: Valid | Reset | Gap | NonMonotonicTime | Missing,
}
```

Pair валиден только когда `current.ts > previous.ts`, reset family/epoch совпадает, нет coverage gap и counter не уменьшился. Decrease или reset создаёт boundary, а не delta zero.

В v1 valid pair атрибутируется timestamp более позднего sample. Для bucket используются pairs, у которых `current.ts_us` лежит в bucket. Это даёт детерминированные arbitrary half-open ranges без пропорционального размазывания неизвестных increments.

- Count/rate: `sum(delta) / sum(duration_us)` по valid pairs, не среднее per-pair rates.
- Ratio counters: numerator и denominator суммируются отдельно, division выполняется после merge.
- Pair может использовать predecessor из halo, но принадлежит только bucket текущего sample.
- Отсутствие valid pairs даёт `None`, не zero.

### 6.6 Gauges и time weighting

- Для instantaneous gauge `max`/`min` берутся только по реально сохранённым samples в bucket.
- Для state gauge с объявленной hold-моделью boundary sample создаёт valid interval до следующего sample или `max_gap_us`; extrema могут учитывать это поддержанное состояние внутри bucket.
- Sample mean равен `sum(values)/count`, а не mean of means.
- Time-weighted mean допускается только для factor с явно заданной zero-order-hold моделью.
- Hold действует между соседними valid samples одной coverage epoch и не дольше factor-specific `max_gap_us`.
- Interval пересекается с bucket математически; gap не заполняется и carry-forward через него запрещён.
- Raw samples, timestamps, boundary samples и coverage сохраняются в facts, поэтому policy может выбирать max или time-weighted mean без PGM rebuild.

### 6.7 Health evaluation и worst downsample

Health сначала вычисляется на co-temporal evaluation cells, образованных границами valid factor intervals, event floors и request buckets. Один cell использует только одновременно поддержанные observations.

При downsample:

1. выбирается реально вычисленная fine point/cell с минимальным overall numeric score;
2. factor/domain penalties берутся из той же точки;
3. component-wise maxima из разных моментов не объединяются;
4. любой trusted floor marker переносится в bucket независимо от numeric score;
5. bucket без required coverage остаётся `Unknown`.

Такой downsample сохраняет худшее доказанное состояние и не создаёт «phantom worst» из несинхронных пиков.

## 7. Модель событий, taxonomy, notable и diagnosis

### 7.1 Четыре разных сущности

```text
PGM row/sample
    -> EventObservation       // что именно сохранил source
    -> EventFact              // нормализованный policy-neutral факт
    -> NotablePolicy result   // показывать ли пользователю и как ранжировать
    -> IncidentDiagnosis      // возможная причина, evidence, confidence
```

Ни один слой не имеет права молча подменять следующий:

- observation с `signal=9` означает SIGKILL observation, а не OOM;
- severity `PANIC` означает PANIC, а не доказанную physical corruption;
- heuristic `DataCorruption` category не доказывает повреждение данных;
- SQLSTATE-like token из stderr остаётся parsed evidence, а не гарантированным structured SQLSTATE;
- immediate shutdown — административный lifecycle fact, а не автоматическая catastrophic cause;
- отсутствие observation ничего не доказывает при partial/unknown coverage.

### 7.2 EventObservation

```text
EventObservation {
  observation_id: [u8; 32],
  identity_quality: SourceExact | ContentDerived | Approximate,

  source_scope_id: [u8; 32],
  source_type_id: u32,
  provenance: ObservationProvenance,

  shape: Individual | GroupedCount | Gap | CounterInterval | StateTransition,
  time: ObservationTime,
  occurrence_count: u64,

  payload: ObservationPayload,
  evidence_quality: Structured | Parsed | Heuristic | DerivedExact,
  quality_flags: bitset,
  loss: Option<LossSummary>,
}

ObservationTime {
  sort_ts_us: i64,
  occurred_at_us: Option<i64>,
  observed_interval: Option<[i64, i64)>,
  quality: Exact | FirstInGroup | CollectionFallback | IntervalOnly,
}

ObservationProvenance {
  section_body_id: [u8; 32],
  section_instance_ordinal: u32,
  row_ordinal: u32,
  dictionary_context_id: [u8; 32],
  source_locator: Option<SourceLocator>,
}
```

`occurrence_count=1` для individual observations/transitions. Grouped error row сохраняет исходный `count`; `count=0` невалиден.

`dictionary_context_id` — digest канонического набора `(StrId, resolved bytes)` для всех dictionary references, которые влияют на observation. Он нужен потому, что одинаковые section bytes с одинаковыми `StrId` могут иметь другую семантику при другом dictionary context.

### 7.3 Stable identity live → sealed

Writer при seal копирует section bodies verbatim и сохраняет catalog order. V1 использует эту provenance:

```text
segment_lineage_id = SHA-256(
  "pgk-overview-lineage-v1" ||
  source_scope_id ||
  first_catalog_entry_type ||
  first_catalog_entry_content_descriptor
)

observation_id = SHA-256(
  "pgk-overview-observation-v1" ||
  segment_lineage_id ||
  source_type_id ||
  section_body_id ||
  section_instance_ordinal ||
  row_ordinal ||
  dictionary_context_id
)
```

`first_catalog_entry_content_descriptor` строится из offset-independent catalog fields (`type/schema/flags/body_len/rows/body_crc32c`), поэтому для lineage не требуется читать нерелевантное body. `section_body_id` хеширует exact relevant section body вместе с `type_id` и длиной. `section_instance_ordinal` различает повторения одинакового body внутри lineage. Ordinal считается в catalog order и остаётся тем же после seal.

Гарантия ограничена текущим source contract:

- ID стабилен при normal active→sealed handoff и повторном derived rebuild;
- policy/formula version в ID не входит;
- repack/resegmentation может изменить lineage;
- byte-identical source bodies в независимых lineages могут быть неразличимы без writer-provided occurrence UUID;
- когда source не содержит file offset/session identity, API возвращает `identity_quality=ContentDerived`, а не обещает source-level identity.

Logical dedup, например compatibility error row и lifecycle row одного crash, выполняет NotablePolicy по доказанной relation. Он не меняет canonical observation IDs и не складывает независимые counts.

### 7.4 EventFact

```text
EventFact {
  fact_id: [u8; 32],
  kind: EventKind,
  shape: FactShape,
  interval: [i64, i64),
  count: u64,
  entity: Option<EntityRef>,
  payload: EventPayload,
  supporting_observation_ids: Vec<[u8; 32]>,
  evidence_quality: EvidenceQuality,
  coverage: CoverageRef,
}
```

EventFact остаётся policy-neutral: он может утверждать `pg.lifecycle.child_signal_termination` или `os.cgroup.oom_kill_delta`, но не `postgres_was_killed_by_oom` без отдельного diagnosis.

### 7.5 Поддерживаемая taxonomy v1

Stable machine codes на wire:

**Retained PostgreSQL log observations**

- `pg.log.error_group_observed`
- `pg.lifecycle.child_signal_termination`
- `pg.lifecycle.shutdown_requested`
- `pg.lifecycle.ready_observed`
- `pg.checkpoint.started`
- `pg.checkpoint.completed`
- `pg.checkpoint.too_frequent_reported`
- `pg.maintenance.autovacuum_reported`
- `pg.maintenance.autoanalyze_reported`
- `pg.query.slow_group_reported`
- `pg.lock.wait_reported`
- `pg.lock.acquired_after_wait_reported`
- `pg.temp_file.reported`
- `collector.pg_log_gap`

**PostgreSQL counter/state facts**

- `pg.database.deadlock_delta`
- `pg.database.recovery_conflict_delta`
- `pg.database.checksum_failure_delta`
- `pg.database.sessions_abandoned_delta`
- `pg.database.sessions_fatal_delta`
- `pg.database.sessions_killed_delta`
- `pg.statistics.reset_observed`
- `pg.postmaster.start_changed`
- `pg.recovery.role_changed`
- `pg.timeline.changed`
- `pg.replication.sender_state_changed`
- `pg.replication.sender_disappeared`
- `pg.replication.slot_state_changed`
- `pg.replication.slot_lost`

Disappearance/state transition выводится только между complete compatible snapshots одной stable entity identity. Через gap факт не создаётся.

**OS/cgroup/coverage facts**

- `os.cgroup.memory_high_delta`
- `os.cgroup.memory_max_delta`
- `os.cgroup.oom_delta`
- `os.cgroup.oom_kill_delta`
- `os.host.oom_kill_delta`
- `os.filesystem.capacity_observation`
- `os.filesystem.capacity_zero_transition`
- `collector.snapshot_gap`
- `collector.source_read_failure`
- `collector.visibility_restricted`

PSI, CPU, memory ratio, cgroup pids, disk throughput, blocked count и wraparound headroom являются metric facts. `pressure_episode`, `low_space` и `wraparound_danger` появляются только после versioned threshold/window policy.

### 7.6 Payload error group

```text
ErrorGroupPayload {
  severity: Error | Fatal | Panic | Warning | Log,
  category: Lock | Constraint | Serialization | Timeout | Connection |
            Auth | Syntax | Resource | DataCorruption | System | Other,
  sqlstate: Option<[u8; 5]>,
  normalized_pattern: Option<TextRef>,
  database: Option<TextRef>,
  user: Option<TextRef>,
  dropped_fields: bitset,
}
```

В facts хранится joint dimension `(severity, category, sqlstate)`, а не только три marginal maps. Иначе нельзя ответить, сколько именно Resource FATAL было в диапазоне. Severity/category/lifecycle — small closed arrays; SQLSTATE и signals кодируются sorted unique bounded vectors, не `HashMap` iteration order.

Normalized pattern сохраняется в bounded string block, поскольку pattern-based policy должна меняться без PGM reread. Sample/detail/hint/context/statement не копируются в v1 canonical index: они не нужны overview и увеличивают privacy/size. Policy, которой потребуются эти поля, меняет extractor semantics и перестраивает facts из PGM.

### 7.7 NotablePolicy

```text
NotablePolicy {
  policy_version: u32,
  rules: ordered stable rules,
  required_evidence_quality: per-rule minimum,
  correlation_and_dedup: stable rule set,
  ranking: stable total order,
  response_cap: projection-only limit,
}
```

Policy может использовать severity, category, SQLSTATE, event kind, rate/window, entity, occurrence count и evidence quality. Она обязана:

- не менять `observation_id`/`fact_id`;
- не записывать notable class обратно в canonical facts;
- применять cap только к response page/preview;
- возвращать `omitted_count` и `next_cursor`, если элементы остались;
- сохранять upstream loss отдельно от response omission;
- различать `PANIC`, `integrity_evidence`, `out_of_memory_observation`, `sigkill_observation`, `storage_capacity`, `authentication`, `contention`, `connection_capacity`, `replication`, `maintenance` и `system` без причинного overclaim.

Current rule catalog v1 обязан покрывать machine codes:

- `postgres_child_sigkill_observed` и `postgres_child_signal_termination`;
- `postgres_panic_observed`;
- `postgres_disk_full_observed`;
- `postgres_out_of_memory_observed`;
- `postgres_connection_slots_exhausted_observed`;
- `postgres_deadlock_observed`;
- `postgres_integrity_error_observed`;
- `postgres_lock_not_available_observed`;
- `postgres_query_cancelled_observed`;
- `postgres_serialization_failure_observed`;
- `postgres_authentication_failure_observed`;
- `postgres_authorization_failure_observed`;
- `postgres_permission_denied_observed`.

Эти codes называют observations. `sigkill`, `out_of_memory` и `integrity_error` остаются разными evidence classes и не объединяются в cause без `IncidentDiagnosis`.

Начальные thresholds из прежних эвристик не являются correctness contract. Auth storm, query-cancel storm, application errors и connection exhaustion требуют rate/window calibration и coverage, а не безусловного catastrophic verdict.

### 7.8 IncidentDiagnosis

```text
IncidentDiagnosis {
  diagnosis_id: [u8; 32],
  diagnosis_kind: stable code,
  interval: [i64, i64),
  supporting_fact_ids: Vec<[u8; 32]>,
  contradicting_or_missing_evidence: Vec<EvidenceRef>,
  confidence: Low | Medium | High,
  diagnosis_policy_version: u32,
}
```

Overview-index не хранит diagnosis как canonical fact. Допустимы формулировки вроде «SIGKILL совпал с cgroup `oom_kill` delta»; недопустимы «SIGKILL доказал OOM» или «PANIC доказал corruption» без дополнительных facts.

### 7.9 Roadmap evidence inputs

| Input | Что он позволит доказать |
| --- | --- |
| Structured csvlog/jsonlog с `log_time`, SQLSTATE, PID/session и source offset | Source-exact occurrence identity и structured fields. |
| Kernel journal/audit OOM victim с PID/start time/cgroup | Конкретную OOM victim relation. |
| systemd/Patroni/Kubernetes lifecycle | Кто запросил restart/termination. |
| Filesystem errno, inode/quota/RO и ext4/XFS events | ENOSPC против quota/RO/corruption и affected mount. |
| SMART/NVMe health | Device media/controller evidence. |
| Declared replication topology и network/link state | Потерю required replica и обоснованную network diagnosis. |

До появления этих полей taxonomy расширяется observation kinds, но не выдуманными causes.

## 8. Модель health

### 8.1 Разделение continuous score, floor и state

```text
HealthPoint {
  interval: [i64, i64),
  continuous_score: Option<f64>,
  overall_score: Option<f64>,
  overall_state: Unknown | Normal | Degraded | Critical,

  health_policy_version: u32,
  factor_set_id: [u8; 16],
  factor_penalties: Vec<FactorPenalty>,
  domain_penalties: Vec<DomainPenalty>,
  floor_evidence: Vec<FloorEvidence>,
  coverage: Vec<FactorCoverage>,
}
```

`continuous_score` описывает continuous resource/operational pressure. `floor_evidence` — отдельные trusted catastrophic observations. `overall_state` объединяет их для UI, не стирая unknown.

Если required domain не покрыт, оба numeric score равны `None`. Trusted floor при этом всё равно задаёт `overall_state=Critical` и остаётся в `floor_evidence`; неизвестный continuous score не превращается в выдуманный zero. При полном required coverage trusted floor делает `overall_score=0.0`.

Полный decision table:

```text
if any required domain is unknown:
  continuous_score = None
  overall_score = None
  overall_state = Critical if trusted floor exists else Unknown
else:
  continuous_score = product(1 - known domain penalties)
  overall_score = 0.0 if trusted floor exists else continuous_score
  overall_state = Critical if trusted floor exists
                  else state_thresholds(overall_score)
```

### 8.2 Factors и domains

Каждый factor имеет stable `FactorId`, unit, applicability rule, reduction, validity/gap rule и monotonic penalty curve `[0,1]`.

Начальные domains:

| Domain | Текущие inputs |
| --- | --- |
| `database_error_pressure` | joint severity/category/SQLSTATE counts и DB session failure deltas |
| `connection_capacity` | current connections/limit, retained 53300-like observations |
| `contention` | blocked sessions, lock waits, deadlock deltas |
| `cpu_pressure` | host/cgroup CPU, PSI CPU, runnable pressure |
| `memory_pressure` | PSI memory, cgroup usage/limits/events, host/cgroup OOM facts |
| `storage_pressure` | disk I/O, proven PG mount capacity, temp/disk-full observations |
| `maintenance` | checkpoint requested/timed deltas, too-frequent logs, XID/MXID headroom |
| `replication` | lag/state/slot lost при declared applicability |

Плановый checkpoint и активный autovacuum сами по себе не являются негативными factors. Wraparound оценивается по XID и MXID axes отдельно. Freeze top-N input сохраняет `source_total`/population completeness.

### 8.3 Formula

Для co-temporal cell:

```text
factor_penalty[f] = curve_f(reduced_fact_f) in [0, 1]

domain_penalty[d] = max(
  factor_penalty[f] for f in domain d
  after dedup by supporting fact identity
)

continuous_score = product(
  1 - domain_penalty[d]
  for known applicable domains
)
```

Within-domain `max` снижает double counting коррелированных continuous signals: cgroup memory pressure+PSI и blocked gauge+lock-wait pressure не умножаются как независимые доказательства одной цепочки. Correlated floor observations дедуплицируются отдельно по supporting fact IDs; они не участвуют в product как обычные penalties.

Между domains используется произведение дополнений. Оно является ordinal operational index, а не вероятностью. Для фиксированного factor set и penalties в `[0,1]` score bounded и монотонен; исчезновение factor не входит в monotonicity property.

### 8.4 Required-domain semantics

`HealthPolicy` содержит:

```text
RequiredFactorProfile {
  profile_id,
  required_domains: Vec<DomainId>,
  required_factors_by_domain: Map<DomainId, Vec<FactorId>>,
  optional_factors: Vec<FactorId>,
  minimum_covered_ratio_by_factor,
}
```

Domain считается known только когда все применимые factors, помеченные required для этого profile, имеют достаточное coverage в evaluation cell и не пересечены invalidating loss/gap. Optional missing factor не блокирует score, но не создаёт zero penalty.

`factor_set_id` — hash health policy version, profile, registry contract, ordered applicable factors и exact ordered set factors/domains, фактически участвовавших в этой point. Если optional factor пропал, ID меняется. Scores сравнимы только при одинаковых `health_policy_version` и `factor_set_id`.

### 8.5 Coverage

```text
FactorCoverage {
  factor_id: FactorId,
  applicability: Applicable | NotApplicable | Unsupported,
  state: Complete | Partial | Gap | Unknown | NotCollected,
  interval: [i64, i64),
  expected_period_us: Option<u64>,
  present_samples: u64,
  covered_duration_us: u64,
  source_population: Option<{ collected: u64, total: u64 }>,
  loss_reasons: bitset,
  lost_count_lower_bound: Option<u64>,
  exactness: RetainedExact | LowerBound | Unknown,
}
```

`covered_ratio = covered_duration_us / bucket_duration_us` может присутствовать как display projection, но не заменяет эту структуру и не решает eligibility score.

Log coverage не объявляется `Complete`, пока source contract не может это доказать. Отсутствие `pg_log_gap` само по себе не является доказательством полноты stderr.

### 8.6 Floors

Trusted floor evidence включает только факты с достаточной evidence quality, например:

- lifecycle crash observation — availability floor;
- structured PANIC — availability floor, но не corruption verdict;
- SQLSTATE XX001/XX002 или checksum failure delta — integrity evidence;
- cgroup/host `oom_kill` delta — OOM-kill evidence;
- structured 53100 observation — disk-full evidence, affected filesystem только при proven mapping.

Не являются автоматическим trusted floor:

- один signal 9;
- generic Resource/System/DataCorruption category;
- SQLSTATE 53200 как доказательство kernel OOM;
- immediate shutdown без interval недоступности/maintenance context;
- отсутствие replication sender без complete previous/current topology.

### 8.7 Explainability

V1 не публикует искусственные additive `contributions`. API отдаёт:

- normalized factor penalties;
- domain penalty и список driving factor IDs;
- raw/reduced value и unit;
- coverage каждого factor;
- floor evidence с fact IDs.

Это однозначно объясняет score. Если позже понадобится allocation total drop между factors, он получает отдельную versioned математическую спецификацию.

## 9. Логическое содержимое per-segment fact index

### 9.1 Canonical required blocks

Каждый fact file содержит следующие block kinds. Block может быть пустым, но required baseline block не может отсутствовать.

| Block | Содержимое |
| --- | --- |
| `SOURCE_MANIFEST` | Catalog-entry inventory, PGM layout/schema, supported/unsupported sections, body/content provenance, source/range metadata. |
| `EVENT_OBSERVATIONS` | Retained source-shaped observations, sorted by `(sort_ts_us, observation_id)`. |
| `EVENT_FACTS` | Policy-neutral normalized facts и links к observations. |
| `LOSS_COVERAGE` | Section presence, coverage intervals, `pg_log_gap`, caps/drop counters, population completeness, tail/source quality. |
| `GAUGE_SAMPLES` | Timestamped values, factor/series/entity identity, units, quality и coverage epoch. |
| `COUNTER_SAMPLES` | Timestamped cumulative values, series/entity, counter family и reset epoch. |
| `RESET_MARKERS` | Per-family reset/postmaster/source epoch boundaries. |
| `ENTITY_STATES` | Complete bounded entity snapshots, population totals и state needed for proven transitions. |
| `STRING_TABLE` | Bounded canonical UTF-8/bytes для normalized patterns и других explicitly retained text refs. |

Blocks partitioned by kind, logical factor/source ID and time range. Один kind может иметь несколько blocks, чтобы query декодировал только пересечение и соседний halo.

### 9.2 Optional accelerator blocks

Unknown optional block разрешено пропустить только если он redundant и не влияет на canonical semantics. V1 допускает:

- full-segment event totals;
- boundary lookup index;
- per-kind timestamp offset index;
- precomputed sorted keys для binary search.

Готовые `HealthPoint`, notable set и final chart buckets не являются canonical facts. Если они когда-либо кешируются на disk, это отдельный projection file kind с policy versions и тем же raw fallback.

### 9.3 Minimal sufficient facts

| Операция | Что хранится | Что нельзя восстановить после premature aggregation |
| --- | --- | --- |
| Event counts | Timestamped observation, joint dimensions, `occurrence_count`, loss | Физические timestamps grouped occurrences |
| Gauge max/min | Natural samples и timestamps | Extrema arbitrary subrange из одного segment-wide max |
| Sample mean | Samples или exact sum+count на разрезаемом block | Mean из mean без count |
| Time-weighted gauge | Samples, validity/gap rule, boundary halo | Exact cut из coarse integral без boundary state |
| Counter delta/rate | Ordered samples, reset family/epoch, gaps | Pair через reset/gap; arbitrary cut из segment endpoints |
| Counter ratio | Separate numerator/denominator samples/deltas | Ratio of averaged ratios |
| Entity disappearance | Complete before/after sets и stable identity | Transition через incomplete snapshot/gap |
| Health | Co-temporal factor facts и coverage | Merge готовых penalties/scores; component maxima from different times |
| Percentile | Raw samples или versioned sketch | Exact percentile из min/max/sum/count |

V1 выбирает compact timestamped samples. Coarse canonical base buckets не используются как единственный источник. Optional summaries ускоряют interior merge, но raw timestamped block остаётся доступным для exact edge slicing.

### 9.4 Canonical encoding rules

- Integers — little-endian fixed width или явно описанный bounded varint внутри block schema.
- Floats — IEEE-754 binary64; NaN и infinity запрещены, `-0.0` canonicalized to `0.0`.
- Small closed domains — fixed arrays.
- SQLSTATE — exact five bytes, sorted unique vector.
- Signals — sorted unique `(i32,u64)` vector.
- Variable collections — length prefix, hard count and byte bounds before allocation.
- Maps encode as sorted unique key/value vectors; Rust `HashMap` order никогда не попадает на disk.
- Timestamps and IDs inside blocks follow canonical total order.
- Decoder consumes the whole decoded block; trailing bytes invalidate block.
- Text bytes bounded, validated by declared text kind, and never implicitly localized.

## 10. Физический per-segment формат

### 10.1 Placement и file key

Fact files физически отделены от store directory:

```text
<cache_root>/overview/v1/<source_scope_hex>/<prefix>/<fact_key_hex>.ovf
```

`prefix` — первые байты `fact_key`, только для ограничения числа directory entries. PGM filename и `first_ts` могут использоваться в diagnostic metadata, но не участвуют как correctness identity.

```text
FactKey = SHA-256(
  "pgk-overview-fact-key-v1" ||
  source_scope_id ||
  source_descriptor ||
  file_kind ||
  fact_schema_version ||
  extractor_semantics_version ||
  registry_contract_version
)
```

Health/notable/response versions в `FactKey` не входят.

### 10.2 Source scope и PGM descriptor

```text
source_scope_id = SHA-256(
  "pgk-overview-source-scope-v1" ||
  normalized_store_namespace ||
  pgm_source_id
)
```

`normalized_store_namespace` берётся из explicit reader configuration. Если настройка отсутствует, fallback — canonical absolute store root; API/metrics помечают identity как `deployment_scoped`. Такой fallback стабилен в данном deployment, но не обещает прежние IDs после переноса store.

Descriptor kind v1:

```text
source_descriptor = SHA-256(
  "pgk-pgm-catalog-descriptor-v1" ||
  source_file_len_le ||
  exact_tail_index_bytes ||
  exact_raw_catalog_block_bytes
)
```

Raw catalog содержит source/range/format и per-section type, offset, length, rows и CRC32C. Descriptor тем самым связан с PGM contents в пределах PGM catalog integrity model и обнаруживает обычную replacement/corruption без чтения bodies.

Threat model v1: PGM/cache принадлежат тому же доверенному OS user, PGM после publication иммутабелен, CRC32C защищает от случайного damage, а не от hostile writer. SHA-256 над catalog с CRC не превращает CRC в криптографическую аутентификацию body.

Restart-warm gate требует zero PGM body reads. Поэтому silent body bit flip при неизменном catalog не может одновременно обнаруживаться на каждом cache hit. Его обнаруживает отдельный bounded source scrub или последующий raw read; после scrub failure segment помечается source-corrupt и derived cache больше не маскирует gap.

### 10.3 Fixed header v1

Все поля сериализуются field-by-field, little-endian. Rust `repr(C)` и native struct layout запрещены.

Header v1 — ровно 160 bytes:

| Offset | Поле | Тип | Контракт |
| ---: | --- | --- | --- |
| 0 | `magic` | `[u8;8]` | `b"PGKOVF\0\0"` |
| 8 | `container_version` | `u16` | `1` |
| 10 | `header_len` | `u16` | `160` |
| 12 | `file_kind` | `u16` | `1 = SegmentFacts` |
| 14 | `header_flags` | `u16` | v1: `0` |
| 16 | `fact_schema_version` | `u32` | Logical fact shape |
| 20 | `extractor_semantics_version` | `u32` | PGM→facts/reducer semantics |
| 24 | `registry_contract_version` | `u32` | Supported type/layout contract |
| 28 | `source_format_version` | `u32` | PGM container version |
| 32 | `pgm_source_id` | `u64` | Provenance, не самостоятельный key |
| 40 | `source_min_ts_us` | `i64` | Inclusive PGM metadata |
| 48 | `source_max_ts_us` | `i64` | Inclusive PGM metadata |
| 56 | `source_file_len` | `u64` | Exact PGM length |
| 64 | `source_scope_id` | `[u8;32]` | Dataset/deployment scope |
| 96 | `source_descriptor` | `[u8;32]` | Content-bound PGM descriptor |
| 128 | `directory_offset` | `u64` | v1: `160` |
| 136 | `directory_count` | `u32` | `1..=MAX_DIRECTORY_ENTRIES` |
| 140 | `directory_entry_len` | `u16` | v1: `64` |
| 142 | `descriptor_kind` | `u16` | v1: catalog descriptor `1` |
| 144 | `file_len` | `u64` | Exact fact-file length |
| 152 | `directory_crc32c` | `u32` | CRC exact directory bytes |
| 156 | `header_crc32c` | `u32` | CRC header с этим полем zeroed |

Unknown magic/version/kind/flags/descriptor kind делает file incompatible. `source_min_ts_us <= source_max_ts_us` обязательно.

### 10.4 Block directory entry v1

Каждая запись — 64 bytes:

| Offset | Поле | Тип |
| ---: | --- | --- |
| 0 | `block_kind` | `u32` |
| 4 | `block_schema_version` | `u16` |
| 6 | `block_flags` | `u16` |
| 8 | `logical_id` | `u32` |
| 12 | `reserved` | `u32` |
| 16 | `offset` | `u64` |
| 24 | `stored_len` | `u64` |
| 32 | `decoded_len` | `u64` |
| 40 | `item_count` | `u32` |
| 44 | `block_crc32c` | `u32` |
| 48 | `min_ts_us` | `i64` |
| 56 | `max_ts_us` | `i64` |

`logical_id` — stable factor/source ID либо `0` для segment-wide blocks. `reserved` обязан быть zero.

Block flags v1:

- bit 0 `REQUIRED_FOR_FACT_SCHEMA`;
- bit 1 `CANONICALLY_SORTED`;
- bits 8..11 codec: `0=None`, `1=Zstd`;
- остальные bits обязаны быть zero.

CRC считается по stored bytes. При compression decoder сначала проверяет stored CRC и `decoded_len` bound, затем decompress с exact output-length limit.

Для codec `None` поля `stored_len` и `decoded_len` обязаны совпадать.

### 10.5 Required/optional extension semantics

- Unknown required `block_kind` или required block schema делает file incompatible и запускает rebuild.
- Unknown optional block безопасно пропускается.
- Canonical block нельзя объявить optional только ради forward compatibility.
- Optional blocks могут быть только redundant accelerators, перечисленные в §9.2.
- Unknown block flag/codec всегда делает file incompatible.
- Missing required baseline block делает file corrupt/incomplete; zero-item required block допустим.

### 10.6 Admission и bounds

V1 safety bounds — correctness/DoS limits, а не benchmark claims:

| Limit | Значение v1 |
| --- | ---: |
| Fact file length | 512 MiB |
| Directory entries | 4096 |
| Directory bytes | 256 KiB |
| One stored block | 64 MiB |
| One decoded block | 128 MiB |
| Items in one block | 1,048,576 |
| Event observations in one segment | 1,048,576 |
| Samples across one logical series block | 1,048,576 |
| SQLSTATE keys in one aggregate | 65,536 |
| Signal keys in one aggregate | 1,024 |
| Coverage spans in one segment | 262,144 |
| One retained normalized pattern | 64 KiB |
| String table decoded bytes | 64 MiB |

Превышение bound не обрезает canonical facts. Segment становится `Uncacheable(limit)`, ответ строится streaming/raw под request work limits и публикует соответствующую acceleration metric. Если одновременно сработал canonical live bound, live state становится `Incomplete` по §4.4.

Admission order:

1. Resolve deterministic target только внутри trusted cache root; не следовать symlink из cache namespace.
2. Открыть regular file и проверить stat/file length bound.
3. Прочитать 160-byte header; проверить magic, versions, kind, flags и header CRC.
4. Checked-арифметикой проверить directory offset/count/entry length и exact directory range.
5. Прочитать bounded directory и проверить directory CRC.
6. Сравнить expected source scope/descriptor/source ID/range/format/file length и `FactKey`.
7. Проверить canonical order, known flags, zero reserved fields, timestamp bounds, non-overlapping block extents и exact final `file_len`.
8. Выбрать только нужные blocks; до allocation проверить stored/decoded length и item count.
9. Проверить block CRC, decompress bound и logical decoder invariants.
10. Проверить sorted/unique keys, enum ranges, finite floats, count overflow, references и полное потребление block.

Bad selected block отвергает весь segment fact file. Partial use хороших blocks из corrupt file запрещено: PGM rebuild остаётся однозначным fallback.

### 10.7 Durable publication

1. Создать process-unique temp в том же cache directory через `create_new`, с правами не шире `0600`; cache namespace не шире `0700`, если operator явно не задал другой безопасный режим.
2. Записать header placeholder, directory и blocks; flush.
3. Записать финальные CRC/lengths, вызвать `sync_all(file)`.
4. Повторно валидировать собственный file тем же admission path.
5. Выполнить atomic same-filesystem rename с no-clobber semantics. Если platform не даёт no-clobber rename, per-key owner сериализует rename; существующий target не перезаписывается до validation. Content-addressed race winner допустим, loser принимает winner только после полной validation.
6. Вызвать `sync_all(parent_directory)`.
7. Удалить собственный temp best-effort.

Cache persistence failure после успешного build не отменяет computed response.

## 11. Версии и identity

### 11.1 Независимые version axes

| Версия | Что меняет | Что инвалидирует |
| --- | --- | --- |
| `container_version` | Header/directory framing | Decoder compatibility; при отсутствии — fact file |
| `fact_schema_version` | Logical canonical facts/fields | Fact file |
| `extractor_semantics_version` | PGM mapping, normalization, reducer/reset semantics | Fact file |
| `registry_contract_version` | Supported PGM types/layouts и required inputs | Fact file |
| `health_policy_version` | Factor set, curves, domains, floors, required profile | Health projection/response only |
| `notable_policy_version` | Selection, dedup, ranking, caps | Event projection/response only |
| `diagnosis_policy_version` | Correlation/cause model | Incident diagnosis only |
| `response_schema_version` | JSON/wire shape | Serialized response cache |
| `cursor_version` | Cursor encoding/validation | Cursor only |

Health/notable change не перестраивает facts, когда сохранённых dimensions достаточно. Новая pattern-based или source-field policy, для которой facts недостаточны, повышает extractor semantics и делает controlled PGM rebuild.

### 11.2 FactSetId и projection cache identity

```text
FactSetId = SHA-256(
  ordered sealed FactKeys ||
  boundary-halo FactKeys ||
  live journal_generation ||
  live folded_watermark ||
  live fact_digest ||
  source/loss generation
)
```

TTL не является identity. Любое изменение active generation/watermark, sealed descriptor, halo, loss state или relevant policy естественно меняет cache key.

### 11.3 Cache file compatibility

- Container decoder может поддерживать несколько старых compatible versions.
- Fact/extractor/registry mismatch не мигрируется in place: old file игнорируется, новый строится рядом.
- Старые namespaces удаляет GC после grace period.
- Unknown required input layout запрещает считать absence measured zero. Segment rebuild завершается `UnsupportedLayout`/coverage unknown, если текущий extractor его не понимает.

## 12. Машины состояний

### 12.1 Sealed segment

```text
Absent
  ├─ memory hit ------------------------------------> ReadyMemory
  ├─ disk candidate -> HeaderAdmitted -> ReadyDisk -> ReadyMemory
  │                       └─ reject ----------------> SoftRejected -> Build
  └─ cold miss -------------------------------------> Build

Build --global admission + per-key single-flight--> Building
  ├─ PGM success -----------------------------------> ReadyMemory
  │                                                    └─ persist best-effort
  │                                                         ├─ success -> ReadyDisk
  │                                                         └─ failure -> PersistBackoff
  ├─ source failure ---------------------------------> SourceFailed
  └─ fact safety limit ------------------------------> Uncacheable
```

`Missing`, `Incompatible`, `Corrupt`, `WrongSource` и cache I/O error являются soft cache errors. `SourceMissing`, `SourceIo`, `SourceCorrupt` и `UnsupportedLayout` влияют на result coverage и не переименовываются в cache miss.

### 12.2 Live builder

| Состояние | Инвариант | Разрешённый response |
| --- | --- | --- |
| `Empty` | Journal доказанно пуст | Sealed-only |
| `Warming` | Restart/full rescan ещё не folded до watermark | Admitted direct fold или explicit warming/tail gap |
| `Current` | Все completed parts до watermark folded ровно один раз | Published `LiveView` + bounded pending-tail read |
| `NeedsRebuild` | Append continuity/identity не доказана | Старый live view не публикуется как current |
| `Incomplete` | Hard cap, unsupported/corrupt completed input или overflow | Explicit loss; promotion запрещён |

```text
LiveState::Current {
  journal_generation,
  folded_through_offset,
  folded_part_ids,
  facts_digest,
  data_through_us,
}
```

Mutable builder имеет одного writer. `ArcSwap` публикует `Arc<LiveView>`, но builder не копирует весь growing vector на каждый part: records хранятся chunked/persistent blocks, публикация переиспользует неизменившиеся chunks. Частота publish ограничивается refresh cycle, а не каждой decoded row.

CPU/blocking I/O, PGM/Parquet decode, hashing и fsync выполняются в bounded blocking workers. Async refresh task только планирует, ждёт result и атомарно публикует view.

### 12.3 Seal handoff

Time-range match не является identity. Handoff:

1. Refresh одновременно видит новый sealed `SegmentDescriptor` и journal transition.
2. Reader строит ordered provenance нового PGM: section body IDs, instance ordinals, row/dictionary context и constituent part facts.
3. Live candidate допускается только из `Current` lossless builder.
4. Candidate provenance должна точно совпасть с новым sealed PGM по всем использованным inputs; timestamp equality недостаточна.
5. При match live facts могут быть promoted как готовый `SegmentFacts` candidate и опубликованы по обычному durable protocol.
6. При mismatch/uncertainty/incomplete candidate отбрасывается, sealed facts строятся из PGM.
7. Новый sealed set и новый/empty live generation публикуются одним `IndexView`.
8. Query-level dedup использует provenance IDs; range partition служит только planner optimization.

Response caps никогда не влияют на promotion.

### 12.4 Restart

1. Reader строит authoritative sealed catalog из PGM headers/catalogs.
2. Fact paths вычисляются из descriptors; cache directory не является source catalog.
3. Headers/directories валидируются лениво или bounded startup scan, bodies fact blocks — on demand.
4. Valid fact file даёт restart-warm path без PGM body read.
5. Active journal получает новую доказанную generation и входит в `Warming`.
6. Completed frames fold-ятся один раз. До `Current` responses показывают warming/tail state или используют admitted direct fold.
7. RAM fact/projection/response caches начинают пустыми.

### 12.5 Corruption и schema change

- Torn active tail не продвигает valid watermark и публикуется как `tail_pending`; это не corruption completed frame.
- Corrupt/incompatible fact file закрывается, учитывается metric и rebuild-ится из PGM.
- Wrong-source file никогда не допускается по совпадению filename.
- Corrupt source PGM создаёт source gap/error; старый derived file не становится автономным source of truth после обнаруженного source damage.
- Formula/notable-only change очищает projection/response keys, но не меняет fact files/mtimes.
- Fact/extractor/registry change создаёт новый cache key; old file остаётся orphan до GC.

### 12.6 Retention и GC

- Mark set строится только из последнего успешного полного store scan.
- Directory-level uncertainty запрещает sweep.
- PGM, исчезнувший из authoritative view, немедленно перестаёт участвовать в новых responses.
- Derived orphan не продлевает source retention.
- Физическое удаление facts откладывается минимум на две successful view generations и configured grace period.
- GC учитывает fact, projection, response-persistence (если появится), temp и orphan bytes/files.
- Stale temps и старые schema namespaces очищаются только по owned naming pattern.
- Content-addressed blob races безопасны; GC и multi-process writer требуют single-owner lock/lease. V1 может требовать один cache owner process.
- GC никогда не удаляет PGM или `active.parts`.

## 13. Иерархия cache и memory-only fallback

### 13.1 Слои

```text
L0 source: immutable PGM + completed active parts
L1 disk:   per-segment canonical fact files
L2 memory: byte-bounded decoded fact blocks + projections
L3 memory: exact serialized response cache
```

L1 переживает restart. L2/L3 очищаются при restart. Ни один слой не меняет correctness semantics нижнего слоя.

### 13.2 Memory fact/projection cache

- Key decoded fact block: `(FactKey, directory entry identity)`.
- Value: immutable `Arc<DecodedBlock>`.
- Eviction: byte-bounded LRU/segmented clock; entry count не используется как основной budget.
- In-flight/pinned bytes учитываются отдельно и входят в global work budget.
- Entry больше per-entry admission limit читается streaming и не ломает response.
- Projection key включает `FactSetId`, exact range, effective step, filters, factor set и policy versions.

Budget должен вмещать dense one-hour working set с boundary halo, если он ниже configured ceiling:

```text
dense_hour_bytes = max over every contiguous 1h plan (
  sum(decoded_len of canonical intersecting blocks) +
  left/right halo blocks +
  measured decoded object overhead
)

effective_fact_budget = min(
  configured_ceiling,
  max(configured_floor, dense_hour_bytes)
)
```

На restart `decoded_len` берётся из admitted directories; object overhead уточняется по runtime metrics. Если `dense_hour_bytes > configured_ceiling`, system публикует `working_set_exceeds_ceiling` и сохраняет correctness через streaming/redecode. Спецификация не выдумывает универсальный byte budget: deployment обязан проверить его на своей dense-hour fixture.

Активный request pin-ит свой рабочий набор до завершения, но не может превысить global in-flight budget.

### 13.3 Exact response cache

```text
ResponseKey {
  endpoint,
  response_schema_version,
  source_scope_ids,
  fact_set_id,
  requested_range,
  effective_range,
  requested_step_us?,
  effective_step_us?,
  normalized_filters,
  health_policy_version?,
  notable_policy_version?,
  factor_set_id?,
  page/view identity?,
}
```

Value — immutable serialized body плюс content type/status metadata. Cache byte-bounded. Live response key всегда включает journal generation и folded watermark; короткий TTL не заменяет эту identity.

### 13.4 Persistent cache modes

Disk read и disk write capabilities ведутся независимо:

```text
PersistentCacheMode =
  ReadWrite |
  ReadOnlyBackoff { reason, next_retry_at, failures } |
  UnavailableBackoff { reason, next_retry_at, failures }

PersistFailure =
  ReadOnlyFilesystem | PermissionDenied | NoSpace | Quota |
  TransientIo | InvalidWinner
```

При `EROFS`, `EACCES`, `ENOSPC`, quota или transient I/O:

1. computed `SegmentFacts` остаётся в memory и обслуживает текущий response;
2. новые builds работают memory-only;
3. уже валидные disk facts продолжают читаться, если read path доступен;
4. причина учитывается отдельно от source PGM errors;
5. `ENOSPC`/quota один раз запускает bounded GC, затем write retry;
6. повторные writes подавляются backoff, чтобы каждый request не повторял одну ошибку;
7. background probe возвращает `ReadWrite` после успешной durable temp publication.

Backoff v1: initial 1 s, multiplier 2, cap 5 min, jitter ±20%; для permission/read-only причин первая повторная проверка начинается с capped interval. Успех сбрасывает backoff. Эти значения operational, а не health/benchmark thresholds.

Cache persistence state виден в metrics/admin diagnostics. Он не попадает в source coverage и не делает корректный timeline partial.

### 13.5 Quota

- Отдельные byte budgets: disk facts, memory facts/projections, exact responses, pinned cursor views и in-flight builds.
- Disk quota считает committed files, stale namespaces, orphans и temp files.
- Временное превышение ограничено одним bounded in-flight file на writer slot.
- Eviction никогда не удаляет in-use `Arc`; file unlink безопасен только после исключения из lookup и с учётом platform semantics.
- При невозможности освободить quota system остаётся memory-only, а не обрезает facts.

## 14. Конкурентность, single-flight и admission

### 14.1 Per-key single-flight

Single-flight key равен `FactKey`. Build принадлежит registry-owned task, а не request leader:

```text
get_or_build(key):
  lock registry briefly
  if Ready -> clone Arc result
  if Building -> subscribe waiter
  if Absent -> insert Building and spawn owned task
  unlock before await
  await shared terminal result
```

Инварианты:

- registry lock не удерживается через `await`;
- cancellation waiter не отменяет общий build и не оставляет slot навсегда `Building`;
- если waiters исчезли до старта, queued work можно отменить;
- уже начавшийся bounded build допускается завершить и кешировать;
- success/error будит всех waiters;
- terminal slot удаляется или заменяется weak ready entry;
- transient cache/source failure не кешируется навечно;
- panic/abort owned task превращается в typed terminal error и очищает slot.

### 14.2 Global cold-work bounds

Per-key single-flight не защищает от одного запроса на сотни разных cold segments. Нужны одновременно:

- weighted global budget по estimated PGM bytes, decoded bytes и CPU work units;
- hard max concurrently building keys;
- bounded blocking worker pool;
- per-request parallelism;
- max in-flight FD/read/write bytes;
- max concurrent cache publications;
- fair queue между requests/sources;
- admission timeout и `Retry-After` для overload;
- max range/segments/points/page до materialization.

Cache hits и response hits не занимают cold-build permits и не проходят через существующий global heavy-analysis semaphore. Короткая LRU metadata mutation допускается; payload возвращается как `Arc` без копирования.

### 14.3 HTTP safety limits v1

| Limit | Default | Absolute v1 cap |
| --- | ---: | ---: |
| Query range | 7 days | 31 days |
| Selected sealed segments | 1024 | 4096 |
| Health points | 2000 | 10,000 |
| Event page size | 100 | 1000 |
| Concurrent segment loads per request | 4 | 16 |
| Pinned cursor views | 128 | 1024 |
| Cursor view TTL | 5 min | 30 min |

Deployment может снижать defaults. Повышение до absolute cap требует memory/FD benchmark. Превышение request shape возвращает machine error до cold work; оно не превращается в partial semantic response.

## 15. Машинный HTTP-контракт

### 15.1 Общая metadata

```text
TimelineMeta {
  response_schema_version: u32,
  view_generation: u64,
  fact_set_id: Base64Url,

  requested_range: { from_us: i64, to_us: i64 },
  effective_range: { from_us: i64, to_us: i64 },
  effective_step_us: Option<u64>,

  data_through_us: Option<i64>,
  tail_pending: Option<{ from_us: i64, to_us: Option<i64> }>,
  source_status: CompleteForContract | Partial | Warming | Gap | Unknown,
  loss: Vec<LossSummary>,
}
```

`CompleteForContract` означает полноту выбранного retained/source contract, а не физического PostgreSQL log, если collector не может её доказать.

Responses machine-neutral: stable codes/enums, числа и IDs. Localized human text не является API contract. Unknown future code отображается клиентом как raw code с generic label.

### 15.2 Overview

```http
GET /v1/timeline/overview?from_us=...&to_us=...&step_us=...
```

```text
OverviewResponse {
  meta: TimelineMeta,
  event_digest: {
    retained_occurrence_count: u64,
    retained_observation_count: u64,
    by_severity: [u64; 5],
    by_category: [u64; 11],
    by_sqlstate: Vec<{ code: [u8;5], count: u64 }>,
    sqlstate_other_count: u64,
    joint_top: Vec<JointErrorCount>,
    lifecycle: LifecycleCounts,
    exactness: RetainedExact | LowerBound | Unknown,
  },
  notable_preview: {
    observations: Vec<EventObservationView>,
    omitted_count: u64,
    events_query_hash: Base64Url,
  },
  health_summary: {
    worst_point: Option<HealthPoint>,
    latest_point: Option<HealthPoint>,
  },
  coverage: Vec<FactorCoverage>,
}
```

`by_sqlstate` top-N и `joint_top` — response projection. `other_count` считается из exact retained aggregate, а canonical index не обрезается. `notable_preview` использует ту же `NotablePolicy` и IDs, что `/events`.

`retained_occurrence_count` нельзя складывать с lifecycle count как «общее число событий»: error groups и compatibility lifecycle representation могут пересекаться. Поля остаются раздельными.

### 15.3 Health

```http
GET /v1/timeline/health?from_us=...&to_us=...&step_us=...&profile=...
```

```text
HealthResponse {
  meta: TimelineMeta,
  health_policy_version: u32,
  factor_set_ids: Vec<Base64Url>,
  points: Vec<HealthPoint>,
  coverage: Vec<FactorCoverage>,
}
```

- Points sorted by interval start.
- Intervals do not overlap and exactly partition effective range, кроме explicit omitted tail outside `data_through_us`.
- No data bucket возвращается с `overall_state=Unknown`, numeric scores `None` и coverage reasons.
- No health interpolation на backend или frontend.
- Worst downsample follows §6.7; floor facts не сглаживаются.

### 15.4 Events

```http
GET /v1/timeline/events?from_us=...&to_us=...&limit=...&cursor=...
                            &min_severity=...&kind=...&entity=...
```

```text
EventsResponse {
  meta: TimelineMeta,
  notable_policy_version: u32,
  events: Vec<EventObservationView>,
  next_cursor: Option<String>,
  omitted_by_response_filter: u64,
  retained_exactness: RetainedExact | LowerBound | Unknown,
  coverage: Vec<FactorCoverage>,
}

EventObservationView {
  event_id: Base64Url,
  identity_quality: SourceExact | ContentDerived | Approximate,
  sort_ts_us: i64,
  occurred_at_us: Option<i64>,
  observed_interval: Option<{ from_us: i64, to_us: i64 }>,
  time_quality: TimeQuality,
  occurrence_count: u64,
  event_kind: stable code,
  notable_class: stable code,
  evidence_quality: EvidenceQuality,
  entity: Option<EntityRef>,
  payload: typed union,
  source_loss: Option<LossSummary>,
}
```

Canonical order: `(sort_ts_us ASC, event_id ASC)`. Byte-identical rows не теряются, пока source provenance может их различить. Grouped row остаётся одной page item.

### 15.5 Cursor

Wire cursor opaque и authenticated server secret; decoded v1 shape:

```text
EventsCursor {
  cursor_version: u16,
  view_generation: u64,
  source_set_id: [u8;32],
  query_hash: [u8;32],
  last_ts_us: i64,
  last_event_id: [u8;32],
  issued_at_us: i64,
}
```

`source_set_id` хеширует ordered selected source scopes. `query_hash` включает range, normalized filters, order, notable policy и response schema. Первая страница pin-ит immutable query/index view. Следующая страница обязана использовать ту же generation и тот же query hash.

`min_severity` применяется только к observations с severity. Typed lifecycle/state facts без severity остаются eligible по `kind` и NotablePolicy; caller, которому нужны только log error groups, задаёт соответствующий `kind` filter.

Pinned view registry bounded по count, bytes и TTL. Она удерживает нужные `Arc` fact/event views, поэтому GC/refresh не меняют уже начатую pagination. Если view не помещается в budget, первая страница возвращает `cursor_view_limit` до обещания stable pagination.

Cursor errors:

- `invalid_cursor` — decode/MAC/version;
- `cursor_query_mismatch` — изменён range/filter/policy;
- `cursor_expired` — TTL/count eviction;
- `view_gone` — source/view больше нельзя удержать;
- HTTP status для expired/gone — `410 Gone`; invalid/mismatch — `400 Bad Request`.

После process restart in-memory pinned views исчезают, поэтому старые cursors честно expire. Stateless continuation на новом live generation запрещено.

### 15.6 Source errors, cache errors и overload

- Cache corruption/write failure не включается в source loss response; это acceleration diagnostics.
- Unreadable/corrupt source segment становится explicit segment/source gap. Если другие данные позволяют корректный partial response, endpoint возвращает `200` с `source_status=Partial/Gap`.
- Если authoritative store view вообще недоступен, возвращается `503 source_unavailable`.
- Request shape выше hard limit — `400 query_limit_exceeded` или `413` для oversized body, без partial work.
- Cold admission timeout — `503 cold_build_overloaded` с `Retry-After`; cache-hit path не должен попадать в этот ответ.

## 16. Границы модулей и крейтов

### 16.1 `kronika-analytics::overview`

Чистое dependency-light ядро:

- `EventObservationInput`, `EventFact`, factor/sample/coverage types;
- deterministic fold/merge/reduce;
- health/notable policies и versioned pure functions;
- checked count algebra;
- property/metamorphic tests.

Модуль не знает `Catalog`, `Part`, `Row`, `StrId`, filesystem, Parquet или HTTP.

### 16.2 `kronika-reader::overview`

Reader-owned persistent index:

- selective PGM section/body extraction;
- targeted dictionary resolver;
- source scope, segment/part/body descriptors и provenance;
- semantic `RefreshDelta`;
- disk header/directory/block codec;
- cache admission, durable publication и typed read/persist errors;
- raw oracle path и fact builder;
- boundary block lookup.

Targeted resolver обязан находить referenced IDs и в `dict.strings`, и в `dict.blobs`; нельзя предполагать, что будущий normalized pattern всегда короче текущего string/blob threshold.

Formula/notable/HTTP semantics в reader codec не живут.

### 16.3 `pg_kronika-web::overview`

- mutable live builder lifecycle и atomic `IndexView` publication;
- byte-bounded memory caches;
- single-flight registry и global cold-work admission;
- request planning, exact response cache и cursor view registry;
- HTTP validation/serialization;
- background retry/GC orchestration и metrics export.

Внутреннее разбиение: `live`, `view`, `admission`, `memory_cache`, `response_cache`, `cursor`, `handlers`. Новый crate для v1 не нужен: disk index имеет одного reader consumer, а чистая алгебра уже помещается в analytics.

### 16.4 Typed error model

```text
CacheReadError =
  Missing | Incompatible | Corrupt | WrongSource | Oversized | Io

PersistError =
  ReadOnlyFilesystem | PermissionDenied | NoSpace | Quota |
  Io | InvalidWinner

SourceError =
  Missing | Io | Corrupt | UnsupportedFormat | UnsupportedLayout

BuildError =
  Source(SourceError) | LimitExceeded | Overflow | Cancelled | Internal

AdmissionError =
  QueryLimit | QueueTimeout | WorkBudget | CursorViewBudget
```

Cache errors допускают fallback. Source errors меняют coverage/result. Limits/cancellation не выдаются за corruption. Wire получает stable error code/request ID; path и internal chain остаются в structured logs.

## 17. Тестовый контракт

### 17.1 Raw oracle и semantic equality

Для каждой supported query должен существовать forced raw PGM/live oracle, который обходит derived cache. Index и oracle обязаны совпадать по:

- retained observations, IDs, `occurrence_count` и ordering;
- exact counts/joint dimensions;
- samples, reset/gap boundaries и factor reductions;
- coverage/loss/applicability;
- health/notable projection одной policy version;
- range-edge и bucket ownership.

Разрешены только явно versioned различия wire encoding/order полей и заданная tolerance floating arithmetic.

### 17.2 Property tests algebra

- Checked event counts: associativity/commutativity при отсутствии overflow.
- Coverage: union не зависит от split/order, overlap не удваивает duration, ratio всегда `[0,1]`.
- Gauge max/min и sum/count merge associative на одинаковой series semantics.
- Counter merge использует bridge pair ровно один раз; reset/gap запрещает bridge.
- Ratio строится из aggregate numerator/denominator, не из mean ratios.
- Health: finite score, bounds `[0,1]`, fixed-set monotonicity.
- Required gap: numeric score `None`, никогда `1.0`.
- Domain max не double-counts один supporting fact ID.
- Factor permutation не меняет domain/continuous score.
- Floor evidence не исчезает при merge/downsample.

### 17.3 Partition/seal metamorphic suite

Для generated canonical stream:

1. Случайно разбить его на 1..100 active parts и 1..20 sealed segments.
2. Случайно выбрать seal point и merge order.
3. Запросить aligned/unaligned ranges и несколько steps.
4. Сравнить raw unsplit, sealed-only, sealed+live, promoted и rebuilt paths.
5. Повторить минимум 10,000 seeds.

Acceptance:

- exact integer facts/counts/IDs/coverage совпадают;
- float results bit-exact либо в documented tolerance;
- observation на boundary принадлежит ровно одному bucket;
- duplicate timestamps/rows не теряются;
- lossy/incomplete live никогда не становится sealed candidate;
- response cap меняет только page/preview, не authoritative retained set.

### 17.4 Event fixtures

- grouped error с `occurrence_count > 1` остаётся одним item;
- top-32/parser/tailer/dictionary loss делает exactness partial/lower-bound;
- lifecycle+compatibility error representation не даёт два notable crash;
- два distinct stored rows с одинаковыми `(ts,pid,signal)` остаются distinct;
- live и sealed дают тот же content-derived ID;
- repeated scan/retry idempotent;
- signal 9 не создаёт OOM diagnosis/fact;
- PANIC не создаёт corruption diagnosis без integrity evidence;
- immediate shutdown не создаёт uncontrolled-failure floor автоматически;
- current 11 error categories и supported typed log kinds проходят round-trip;
- cursor проходит retained result set ровно один раз и честно expires.

### 17.5 Health/reset/gap fixtures

- total gap, PG-only, OS-only и missing required domain;
- crash, cgroup/host OOM, disk full, integrity evidence;
- auth/connection/application error storm;
- deadlock, blocked sessions, forced checkpoint;
- wraparound danger и replication slot lost;
- reset точно на segment boundary;
- long gap и sparse cadence;
- missing factor block не создаёт zero baseline;
- worst point/floor сохраняется при каждом downsample;
- penalties одной returned point co-temporal.

Sparse cadence golden:

```text
t0  cgroup usage = 0
t5  cgroup sample absent
t10 cgroup usage = 10 CPU-seconds, effective quota = 1 CPU
```

Valid interval — 10 seconds, не 5. Rate равен 100%, а не 200%. Если continuity нельзя доказать, результат `None`, не zero. Cumulative OOM counter `1 -> 1` не создаёт повторное событие.

### 17.6 Binary format and corruption suite

Обязательны unit/property/fuzz tests:

- every truncated header/directory/block length;
- bad magic/version/kind/flags/reserved;
- header/directory/block CRC mismatch;
- directory multiplication/addition overflow;
- overlapping/out-of-file offsets, trailing bytes и wrong exact file length;
- decompression bomb/decoded length mismatch;
- oversized counts/strings/vectors;
- unsorted/duplicate keys, invalid enums, NaN/infinity;
- missing/unknown required block;
- unknown optional accelerator skip;
- wrong source scope/descriptor/range/source ID;
- publish race, invalid winner и stale temp cleanup;
- cross-version rebuild without in-place mutation.

Каждый invalid fact file либо даёт raw fallback, либо source error от PGM. Panic/OOM allocation от untrusted lengths запрещены.

### 17.7 Cache, concurrency и cancellation

- N concurrent misses одного `FactKey` выполняют один build.
- Cancellation первого/последнего waiter не оставляет registry slot.
- Panic/abort worker будит waiters typed error.
- 16 distinct cold keys не превышают global work/FD/write budget.
- Cache/response hits не занимают heavy-analysis/cold permit.
- Live generation/watermark change invalidates response key без TTL race.
- `EROFS`, `EACCES`, `ENOSPC`, quota и transient I/O возвращают корректный response из memory.
- Backoff подавляет write storm и восстанавливается после успешного probe.
- Dense-hour cache sizing/metric учитывает halo и decoded overhead.
- GC race не удаляет in-use file и никогда не касается PGM.

## 18. Бенчмарки и performance gates

### 18.1 Fixtures

Основные диапазоны при fixture cadence 5 s:

| Range | Samples |
| --- | ---: |
| 1 hour | 720 |
| 24 hours | 17,280 |
| 7 days | 120,960 |

Оценки 4/96/672 segments допустимы только в fixture с 900-second rotation без early size seal; реальные segment counts измеряются.

Дополнительные fixtures:

- 30% sparse/missing cadence;
- reset на segment boundary;
- duplicate timestamps и byte-identical rows;
- FATAL/error storm до collector caps;
- explicit `pg_log_gap`;
- two sources;
- two store scopes с `source_id=0`;
- corrupt fact block;
- corrupt PGM section;
- dense one-hour working set;
- mixed 5/10/30/60/3600-second source cadences.

### 18.2 Режимы

1. `derived-cold`: новый process, пустой cache directory.
2. `restart-warm`: новый process, valid disk facts, пустые RAM caches.
3. `process-hot`: второй и последующие identical requests.
4. `range-cold/facts-warm`: новый range/step/filter, response miss, facts hit.
5. `live`: sealed facts + active parts + pending tail.
6. `concurrent-identical`: 16 simultaneous cold misses одного fact set.
7. `concurrent-disjoint`: 16 simultaneous different cold ranges.
8. `memory-only`: persistent write failure, facts остаются в byte-bounded RAM.
9. `reftool-comparator`: prebuilt sidecars, сопоставимые signal/sample cardinality, тот же host/filesystem.

Process-cold и storage-cold/page-cache-cold называются отдельно. Нельзя выдавать warm OS page cache за cold disk.

### 18.3 Измерения

- p50/p95/p99 wall latency;
- CPU time и peak RSS;
- file opens, reads, writes, fsyncs и bytes;
- PGM bodies/sections/rows decoded;
- fact blocks read/decompressed;
- builds/waiters/queue/rejects;
- memory/disk cache bytes and evictions;
- live visibility lag;
- GC/temp/orphan work;
- serialized response bytes.

### 18.4 Initial gates

Это acceptance targets, а не уже измеренные результаты:

1. Correctness fixtures и metamorphic suite дают 100% semantic equality.
2. `restart-warm` sealed interior: `0` PGM body bytes и `0` PGM section decodes.
3. `process-hot`: `0` PGM body reads/decodes и `0` cache writes; p95 не больше 25% `derived-cold` p95.
4. `restart-warm` p95 не больше 25% `derived-cold` p95.
5. Common health/count restart-warm p95 не хуже reftool sidecar p95 более чем на 10% в noise-controlled repeated run.
6. Если HTTP/JSON доминирует в cross-engine gate, обязателен одинаковый microbenchmark `compact facts read + bucket`, а endpoint ratio остаётся отчётной метрикой.
7. 16 identical cold misses: один fact build, 16 successful responses, без overload/503.
8. Disjoint cold workload остаётся в global budget; RSS/FD/build concurrency не выходят за configured caps.
9. Completed active frame visibility p95 не больше 2.5 s при 1-second refresh loop; pending tail явно отмечен.
10. Formula/notable-only bump: zero PGM reads, unchanged fact files и mtimes.
11. Fixed metric fact component стремится не превышать reftool comparator 145 bytes/sample; variable retained event bytes измеряются отдельно.
12. Cache quota stress: steady-state не выше configured quota; temporary excess не больше bounded in-flight publication budget.
13. Memory-only dense-hour request остаётся корректным; если working set ниже ceiling, повтор обслуживается без PGM reread.

## 19. Observability

Metrics минимум:

**Fact/cache**

- `overview_fact_lookup_total{layer,result,reason}`
- `overview_fact_build_total{result,source_type}`
- `overview_fact_build_seconds`
- `overview_fact_read_bytes`, `overview_fact_write_bytes`
- `overview_pgm_body_read_bytes`, `overview_pgm_sections_decoded`
- `overview_cache_mode{mode,reason}`
- `overview_cache_entries`, `overview_cache_bytes{class}`
- `overview_cache_evictions_total{class,reason}`
- `overview_persist_failures_total{reason}`
- `overview_persist_backoff_seconds`

**Concurrency**

- `overview_singleflight_builds`, `overview_singleflight_waiters`
- `overview_cold_work_inflight{kind}`
- `overview_cold_queue_depth`, `overview_cold_reject_total{reason}`
- `overview_open_files`, `overview_inflight_bytes`

**Live/view/cursor**

- `overview_live_state{state,reason}`
- `overview_live_folded_parts_total`
- `overview_live_data_through_us`
- `overview_live_visibility_lag_seconds`
- `overview_view_generation`
- `overview_cursor_views`, `overview_cursor_view_bytes`
- `overview_cursor_expired_total{reason}`

**Correctness/quality**

- `overview_source_failures_total{reason}`
- `overview_coverage_loss_total{source,factor,reason}`
- `overview_retained_observations_total{kind}`
- `overview_overflow_total{kind}`
- `overview_raw_fallback_total{reason}`
- `overview_gc_files_total{action,reason}`
- `overview_gc_bytes_total{action}`

Structured logs включают request ID, source scope, FactKey prefix, view generation и error chain. Full paths, raw patterns и user/database text не логируются без explicit debug/redaction policy.

## 20. Критерии приёмки parity v1

Релиз принимает все условия одновременно:

1. Valid disk facts после process restart дают zero PGM body reads для sealed interior.
2. Raw-vs-index semantic equality доказана для exact/partial ranges, single/multi-segment и sealed+live.
3. Random partition/seal invariance не находит duplicates, loss или boundary drift.
4. Missing/corrupt/incompatible/wrong-source cache всегда fallback-ится; cache persistence failure не ломает correct response.
5. Source corruption остаётся видимым source gap/error и не скрывается derived data после обнаружения.
6. Formula/notable response change не rebuild-ит facts, пока facts содержат нужные dimensions.
7. Каждый retained notable item проходит cursor scan ровно один раз; grouped count и upstream loss сохраняются.
8. Stable event IDs переживают normal live→seal handoff; ограничение content-derived identity опубликовано.
9. Canonical live builder lossless; hard truncation маркирует `Incomplete` и запрещает promotion.
10. Required-domain gap всегда даёт numeric `None`, никогда green one.
11. Trusted floor evidence сохраняется при partition, seal и worst downsample; numeric unknown остаётся unknown.
12. Per-factor coverage/applicability/loss присутствует; один display ratio не используется как correctness gate.
13. Counter rates используют actual adjacent interval, reset families и gaps; storage boundary не меняет результат.
14. reftool-visible signals — его 11 error categories, typed lifecycle/checkpoint/maintenance/lock/query events, blocked/deadlock/forced-checkpoint/cgroup-OOM/replication statuses — представлены EventFact, metric factor или explicit gap.
15. Cache hits не проходят через global heavy-analysis semaphore; identical misses single-flight, disjoint misses globally bounded.
16. Memory-only fallback byte-bounded, retry/backoff наблюдаемы, dense-hour sizing проверено.
17. Quota/GC учитывает все derived bytes, безопасен при concurrent readers и никогда не удаляет source.
18. Benchmark modes и gates §18 воспроизводимо проходят на зафиксированном host/filesystem profile.

## 21. Вехи реализации parity v1

Все этапы ниже входят в один parity v1. Disk layer не переносится в следующую продуктовую веху.

### M0. Контракты и oracle

- Stable fact/event/coverage types и version constants.
- Half-open raw oracle и golden fixtures.
- Property tests reductions, health eligibility и data-honesty.

### M1. Selective extraction и physical format

- Reader source/segment/body/dictionary descriptors.
- Targeted dictionary resolution.
- Timestamped canonical blocks.
- Bounded header/directory/block codec, fuzz/admission tests.

### M2. Persistent sealed facts

- Content-addressed lookup.
- Cold build, durable publish и raw fallback.
- Byte-bounded memory fact cache.
- Restart-warm and multi-day benchmark path.

### M3. Live and seal

- Semantic `RefreshDelta` и journal generation.
- Lossless chunked LiveBuilder.
- Atomic `IndexView` publication.
- Provenance-gated promotion и rebuild fallback.

### M4. Policies and HTTP

- EventObservation/EventFact/NotablePolicy separation.
- Health domains, required coverage, floors и worst downsample.
- Overview/health/events schemas, pinned cursors и response cache.

### M5. Resilience and admission

- Cancellation-safe single-flight.
- Global weighted cold-work budget.
- Memory-only persistence fallback/backoff.
- Quota, GC, source scrub integration and observability.

### M6. Parity qualification

- Raw/index/seal/corruption suites.
- Dense-hour sizing.
- Cold/restart/hot/live/concurrent/reftool benchmarks.
- UI contract tests: no health interpolation, unknown/floor/loss rendering.

## 22. Явные нецели и отклонённые alternatives

### 22.1 Non-goals v1

- Изменение writer, PGM или collector source contracts.
- Source-exact per-log-line history, которой нет в PGM.
- Cross-host/distributed cache coherence.
- Hostile-writer authenticity/signatures без отдельного security requirement.
- Cache как архив после удаления PGM.
- Unbounded startup prewarm всего retention.
- SSE до стабилизации view/cursor semantics.
- Новый crate только ради overview cache.
- Сохранение IncidentDiagnosis как source fact.
- Универсальная вероятность «здоровья» без calibration dataset/outcome.

### 22.2 Отклонённые physical alternatives

**Writer-owned `.heatmap`/`.charts`.** Отклонено: замораживает rates/health semantics, меняет writer/PGM ownership и заставляет data rebuild при formula change.

**Global append-only derived index.** Отклонено для v1: создаёт второй WAL с framing, locks, tombstones, compaction и большим corruption blast radius.

**Hybrid blobs + manifest как authority.** Не нужен в v1: authoritative segment/range catalog уже даёт reader snapshot, а manifest не устраняет N selective payload reads. Позже допускается rebuildable hint, если profiling докажет directory/startup/GC bottleneck или появится второй consumer.

**Только in-memory LRU.** Полезен как prototype, но не достигает restart-warm и multi-day parity.

**Только persistent exact responses.** Не заменяет facts: новый range/step/filter снова потребует raw PGM, а invalidation станет combinatorial.

**Segment-wide EventDigest/endpoints.** Недостаточно для arbitrary partial range, reset-aware rate и stable event pagination.

**Canonical precomputed HealthPoint/notable list.** Отклонено: policy change заставляет rebuild, merge готовых penalties/scores нарушает partition invariance.

**Lossy live top-N.** Отклонено для canonical state: несовместимо с authoritative retained `/events` и seal promotion.

**Gap interpolation.** Запрещено для health и counter continuity: рисует данные, которых source не наблюдал.

## 23. Оставшиеся продуктовые решения

Архитектурный контракт не зависит от четырёх калибруемых решений:

1. Конкретные factor curves, required profiles и state thresholds после выбора outcome и calibration fixtures.
2. Обязателен ли explicit `store_namespace` в production или допустим documented path-derived fallback.
3. Deployment budgets disk/RAM/FD/build queue и cursor TTL в пределах absolute safety caps.
4. Maintenance/topology declarations, которые позволят подавлять planned shutdown и определять required replication members.

Их изменение версионирует policy/configuration, но не меняет PGM, physical fact identity или data-honesty invariants.
