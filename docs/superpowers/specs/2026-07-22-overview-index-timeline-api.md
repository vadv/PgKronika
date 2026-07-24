# PgKronika — overview-index и timeline API

Версия: 0.4
Дата: 2026-07-24
Статус: целевой контракт parity v1 с таблицей текущей реализации

## 1. Цель и статус решений

PgKronika должна быстро строить событийный обзор и health-line для произвольного временного диапазона. Повторный запрос, запрос после перезапуска процесса и многодневный диапазон не должны заново декодировать тела PGM-сегментов, если для них существует валидный fact index. Свежие данные из `active.parts` должны появляться с задержкой не больше нескольких refresh-циклов.

Parity v1 включает reader-owned persistent on-disk fact index. In-memory cache без дискового слоя может быть промежуточным этапом разработки, но не считается parity-релизом.

PGM остаётся единственным источником истины. Writer, формат PGM и протокол seal не меняются. Индекс принадлежит reader-слою, хранит formula-neutral facts и удаляется без потери исходных данных.

Документ разделяет два слоя. Нормативные разделы задают целевой контракт parity v1. Пометки «текущая реализация» и таблицы статуса фиксируют фактически доступный data path после M0–M4; они не превращают открытые пункты M5/M6 или незаполненные canonical blocks в реализованное поведение. Проверяемое соответствие target contract закрывается только критериями §20.

Числовые health-кривые и продуктовые пороги требуют отдельной калибровки. Эта спецификация фиксирует их входы, алгебру, coverage, версионирование и ограничения, но не выдаёт непроверенные пороги за доказанную модель здоровья.

Нормативные слова «обязан», «нельзя» и «допускается» задают контракт v1. Псевдокод описывает wire и storage semantics, а не Rust ABI.

## 2. Определения

| Термин | Значение |
| --- | --- |
| PGM | Иммутабельный sealed-сегмент PgKronika, источник истины для закрытого диапазона. |
| Active part | Завершённый CRC-валидный frame в `active.parts`. Незавершённый tail не считается частью. |
| Segment descriptor | Content-bound identity PGM, вычисленная из его catalog/tail/length и source scope. |
| Segment facts | Целевой formula-neutral индекс одного sealed PGM: retained observations, canonical facts, timestamped metric samples, reset/state/gap/coverage и provenance. Текущая наполненность указана в §9.1. |
| Live builder | Единственный mutable lossless builder, сворачивающий завершённые active parts ровно один раз. |
| Live view | Иммутабельный снимок live facts с journal generation и folded watermark. |
| Index view | Атомарный снимок ordered sealed descriptors и одной точной live generation. Все части одного запроса читаются из одного view. |
| EventObservation | Сохранённое PGM-наблюдение в форме источника: individual row, grouped row или gap. |
| EventFact | Canonical policy-neutral нормализованный факт, выведенный из одного или нескольких observations/samples с явной provenance. Не равен одноимённому текущему web DTO, описанному в §7.4. |
| NotablePolicy | Версионированная чистая проекция, которая классифицирует, выбирает и ранжирует входные observations/facts для `/events` и preview; результат не записывается в canonical blocks. |
| IncidentDiagnosis | Отдельный корреляционный вывод о возможной причине с evidence и confidence. Не является observation или fact и в текущей реализации отсутствует. |
| FactKey | Content-addressed identity, связывающая source descriptor и version axes; определена в §10.1. |
| FactBuildKey | Полная immutable identity одного build: `(FactKey, SegmentLineageId)`. Она квалифицирует placement, owner lock, fallback и будущий fact-build single-flight. |
| Retained exactness | Точность относительно строк и counts, которые фактически дошли до PGM. Она не означает полноту исходного PostgreSQL log. |
| Projection | Health, notable, digest, downsample или HTTP-представление, вычисленное из facts текущей policy version. |
| Source scope | Identity набора PGM, вычисленная из обязательного explicit `store_namespace` и `pgm_source_id`; она отделяет одинаковые source IDs в разных stores. |

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
8. Numeric health принимается по зафиксированным oracle fixtures, coverage и отсутствию false-green; latency и размер проверяются на versioned host/filesystem profile без неподтверждённых сравнительных claims.

### 3.3 Границы v1

- Новые collectors и новые PGM sections не требуются.
- Текущий stderr source остаётся bounded и grouped; overview не обещает восстановить отброшенные строки.
- Историческая причина incident не выводится из одного token/signal/category.
- Cache не становится архивом и не продлевает retention PGM.
- Charts отложены владельцем: chart extraction, chart-specific blocks, endpoint и render contract не входят в parity v1. Их стоимость не измерена и не оценивается подстановкой синтетических размеров.

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
- API/presenter не интерполируют health через gaps; будущий UI обязан сохранить тот же contract.
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
sealed PGM -> SegmentDescriptor -> durable lookup     │
        │                         │                    │
        │                         ├-> bounded fallback │
        │                         └-> cold PGM build   │
        │                              + best-effort persist
        ▼                                              ▼
retained log rows ──> EventObservation ──> canonical EventFact
metric/state rows ──> samples/reset/state ──> EventFact when applicable
        │                                              │
        └──────── ordered SegmentFacts ───────> immutable IndexView
                                                      │
                           capture one generation per request
                                                      ▼
               selective facts + left/right boundary halo
                                                      │
                    health/notable/API projections
                                                      ▼
                       overview / health / events response
```

В текущем data path верхняя ветвь останавливается на `EventObservation`: canonical `EventFact` block ещё пуст. Metric/state rows пока остаются manifest-only и не материализуются в samples/reset/state. Эти пробелы перечислены в §9.1, §20 и последовательности перед M5 в §21; diagram выше задаёт target flow, а не утверждает его полную реализацию.

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
retained log row ─────> EventObservation ─────> canonical EventFact ──┐
retained metric row ──> gauge/counter/reset/state ─> EventFact when applicable ─┤
                                                                    ├─> NotablePolicy result
                                                                    └─> IncidentDiagnosis
```

`NotablePolicy` — presentation projection, а `IncidentDiagnosis` — отдельная корреляционная модель; они не являются последовательными стадиями обязательного преобразования каждого события. Ни один слой не имеет права молча подменять другой:

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

  shape: Individual | GroupedCount | Gap,
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
  quality: Exact | FirstInGroup | RepresentativeSample | MaxDurationSample |
           ParsedWithoutVerifiedOffset | CollectionFallback | IntervalOnly,
}

ObservationProvenance {
  segment_locator: Option<SegmentLocator>,
  section_body_id: [u8; 32],
  catalog_entry_ordinal: u32,
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
  naming_contract_id ||
  segment_locator ||
  first_catalog_entry_type ||
  first_catalog_entry_descriptor_len_le ||
  first_catalog_entry_content_descriptor
)

observation_id = SHA-256(
  "pgk-overview-observation-v1" ||
  segment_lineage_id ||
  source_type_id ||
  section_body_id ||
  catalog_entry_ordinal ||
  row_ordinal ||
  dictionary_context_id
)
```

`first_catalog_entry_content_descriptor` строится из offset-independent catalog fields (`type/schema/flags/body_len/rows/body_crc32c`), поэтому для lineage не требуется читать нерелевантное body. `section_body_id` хеширует exact relevant section body вместе с `type_id` и длиной. `catalog_entry_ordinal` считается по всему segment catalog и вместе с `row_ordinal` различает повторения одинакового body внутри lineage. Ordinals остаются теми же после normal seal.

Гарантия ограничена текущим source contract:

- ID стабилен при normal active→sealed handoff и повторном derived rebuild;
- policy/formula version в ID не входит;
- repack/resegmentation может изменить lineage;
- byte-identical source bodies под разными proven locators имеют разные lineage и observation IDs;
- когда source не содержит file offset/session identity, API возвращает `identity_quality=ContentDerived`, а не обещает source-level identity.

Target logical dedup, например compatibility error row и lifecycle row одного crash, может выполнять `NotablePolicy` только по доказанной relation. Он не меняет canonical observation IDs и не складывает независимые counts. Текущий policy классифицирует observations по одной записи и такой relation не заявляет.

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

Текущая реализация не материализует этот тип и оставляет `EVENT_FACTS` canonical-empty. Тип `EventFact` в `pg_kronika-web` — стабильный redacted DTO одной выбранной `EventObservation`: у него отдельные semantic `event_id` и physical `event_instance_id`, а `supporting_evidence` содержит ровно эту observation. Совпадение имени не делает DTO canonical fact. После заполнения `EVENT_FACTS` presenter обязан либо сохранить совместимый wire contract, либо повысить `response_schema_version`.

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

Перечень §7.5 — target taxonomy. Фактически подтверждённое преобразование текущего PGM приведено ниже; отсутствующий mapping нельзя считать реализованным только потому, что machine code перечислен в taxonomy.

### 7.6 Проверенный current source mapping

Для всех восьми поддерживаемых log sections `observation_id` выводится из `SegmentLineageId`, `source_type_id`, section body identity, catalog/row ordinal и dictionary context. Normal sealed lineage включает explicit source scope и locator/naming contract; live identity имеет `Approximate` quality до доказанного handoff. Эта identity не заменяет source-level occurrence UUID, которого PGM не хранит.

| PGM source / retained row | Текущая materialization | Identity, units и reset | Coverage/loss | Открытый target mapping |
| --- | --- | --- | --- | --- |
| `1_022_001 pg_log_errors`, grouped row с `count > 0` | `EventObservation::GroupedCount(ErrorGroup)`, `occurrence_count=count`, evidence `Heuristic` | Общая provenance identity; единица count — retained occurrences; reset не применим | Dictionary truncation/drop отмечается как loss; timestamp может стать `CollectionFallback`; source completeness из отсутствия gap не выводится | Canonical `EventFact` отсутствует |
| `1_024_001 pg_log_checkpoints`, одна retained row | Individual `CheckpointStarted`, `CheckpointCompleted` или `CheckpointTooFrequent`, evidence `Parsed` | Общая provenance identity; `ms`, `kB` и counts остаются полями typed payload; counter/reset family не определены | Dictionary loss и timestamp fallback сохраняются | Нет metric sample/reset mapping; canonical `EventFact` отсутствует |
| `1_025_001 pg_log_autovacuum`, одна retained row | Individual `AutovacuumReported` или `AutoanalyzeReported`, evidence `Parsed` | Общая provenance identity; source units остаются в typed payload; stable entity/reset contract не определён | Dictionary loss и timestamp fallback сохраняются | Нет sample/entity-state mapping; canonical `EventFact` отсутствует |
| `1_026_001 pg_log_slow_queries`, grouped row с `count > 0` | `EventObservation::GroupedCount(SlowQueryGroup)`, время относится к max-duration sample, evidence `Parsed` | Общая provenance identity; duration — `ms`, count — retained occurrences; reset не применим | Dictionary loss и timestamp fallback сохраняются | Canonical `EventFact` отсутствует |
| `1_027_001 pg_log_lock_waits`, одна retained row | Individual `LockWaitReported` или `LockAcquiredAfterWait`, evidence `Parsed` | Общая provenance identity; duration — `ms`; stable lock entity/reset contract не определён | Dictionary loss и timestamp fallback сохраняются | Нет state/counter mapping; canonical `EventFact` отсутствует |
| `1_028_001 pg_log_lifecycle`, одна retained row | Individual child signal/crash, shutdown или ready observation, evidence `Parsed` | Общая provenance identity; PID/signal — retained payload, не causal identity; reset не применим | Dictionary loss и timestamp fallback сохраняются | Canonical `EventFact` и `IncidentDiagnosis` отсутствуют |
| `1_029_001 pg_log_gap`, одна retained row | `EventObservation::Gap(LogGap)` с interval `[ts, ts+1)`, evidence `DerivedExact` | Общая provenance identity; skipped bytes и dropped-line counters сохраняют исходные единицы; reset не применим | Known gap, loss reasons и доказанный lower bound выводятся из retained gap row | Дополнительный fact не требуется; factor applicability для других source families остаётся открытой |
| `1_030_001 pg_log_temp_files`, одна retained row | Individual `TempFileReported`, evidence `Parsed` | Общая provenance identity; size — bytes; entity/reset contract не определён | Dictionary loss и timestamp fallback сохраняются | Нет gauge/counter mapping; canonical `EventFact` отсутствует |
| Другие catalog entries с semantics не `EventStream` | Event/metric facts не создаются; descriptor остаётся в `SOURCE_MANIFEST`. Targeted dictionary bodies могут читаться только для разрешения ссылок уже выбранных observations | Units, reset family и entity identity для overview не определены | Presence в manifest не доказывает factor coverage | Explicit unsupported mapping; не zero и не sample/state |
| Новый registered `EventStream` вне восьми строк выше | Build завершается `UnsupportedLayout` | Identity/facts не создаются | Ошибка влияет на source result/coverage, а не маскируется cache miss | Сначала требуется явный bounded mapping и version bump |

Таким образом, текущий source→retained row→observation path проверяем, но source→metric sample/state и source→canonical `EventFact` остаются открытыми. Для них нельзя угадывать units, reset family, entity identity, applicability или loss по имени section либо тексту.

### 7.7 Payload error group

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

Целевой canonical `EventFact` сохраняет только bounded normalized pattern и явно перечисленные policy-neutral dimensions. Текущий source-shaped `EventObservation` также удерживает bounded sample/detail/hint/context/statement, database и user в `STRING_TABLE`; это не означает, что их надо дублировать в `EVENT_FACTS`. Если будущему canonical policy contract потребуются дополнительные поля, mapping и redaction фиксируются явно, `extractor_semantics_version` повышается, а facts перестраиваются из PGM.

### 7.8 NotablePolicy

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

Целевой policy может использовать severity, category, SQLSTATE, event kind, rate/window, entity, occurrence count и evidence quality. Она обязана:

- не менять `observation_id`/`fact_id`;
- не записывать notable class обратно в canonical facts;
- применять cap только к response page/preview;
- возвращать `omitted_count` и `next_cursor`, если элементы остались;
- сохранять upstream loss отдельно от response omission;
- различать `PANIC`, `integrity_evidence`, `out_of_memory_observation`, `sigkill_observation`, `storage_capacity`, `authentication`, `contention`, `connection_capacity`, `replication`, `maintenance` и `system` без причинного overclaim.

Текущий `NotablePolicy::v1` классифицирует retained `EventObservation` по одной записи, не создаёт canonical fact и не делает causal correlation. Его фактические stable wire codes:

- `server_child_sigkill` и `server_child_signal_termination`;
- `panic_severity_observation`;
- `filesystem_space`;
- `postgres_out_of_memory_observation`;
- `connection_saturation`;
- `deadlock_observation`;
- `corruption_sqlstate_observation`;
- `lock_not_available_observation`;
- `query_canceled_observation`;
- `serialization_failure_observation`;
- `auth_failure_observation`;
- `authorization_failure_observation`;
- `permission_denied_observation`.

Эти codes называют observations. `sigkill`, `out_of_memory` и `integrity_error` остаются разными evidence classes и не объединяются в cause без `IncidentDiagnosis`.

Начальные thresholds из прежних эвристик не являются correctness contract. Auth storm, query-cancel storm, application errors и connection exhaustion требуют rate/window calibration и coverage, а не безусловного catastrophic verdict.

### 7.9 IncidentDiagnosis

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

В текущей реализации зарезервирован только `diagnosis_policy_version`; production `IncidentDiagnosis` и causal correlation отсутствуют.

### 7.10 Roadmap evidence inputs

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

Каждый fact file содержит следующие block kinds. Block может быть пустым, но required baseline block не может отсутствовать. Target content и фактическая наполненность текущего writer различаются:

| Block | Целевое содержимое | Текущая реализация |
| --- | --- | --- |
| `SOURCE_MANIFEST` | Catalog-entry inventory, PGM layout/schema, supported/unsupported sections, body/content provenance, source/range metadata | Заполнен для всех catalog entries |
| `EVENT_OBSERVATIONS` | Retained source-shaped observations, sorted by `(sort_ts_us, observation_id)` | Заполнен только для восьми log `EventStream` layouts из §7.6 |
| `EVENT_FACTS` | Policy-neutral normalized facts и links к observations | Canonical-empty; web DTO с таким именем не записывается сюда |
| `LOSS_COVERAGE` | Section presence, coverage intervals, `pg_log_gap`, caps/drop counters, population completeness, tail/source quality | Заполнен доступными catalog coverage, known gaps и retained lower bounds; полнота неподдержанных factors не додумывается |
| `GAUGE_SAMPLES` | Timestamped values, factor/series/entity identity, units, quality и coverage epoch | Canonical-empty |
| `COUNTER_SAMPLES` | Timestamped cumulative values, series/entity, counter family и reset epoch | Canonical-empty |
| `RESET_MARKERS` | Per-family reset/postmaster/source epoch boundaries | Canonical-empty |
| `ENTITY_STATES` | Complete bounded entity snapshots, population totals и state needed for proven transitions | Canonical-empty |
| `STRING_TABLE` | Bounded canonical UTF-8/bytes для normalized patterns и других explicitly retained text refs | Заполняется строками, на которые ссылаются текущие `EVENT_OBSERVATIONS`; может быть естественно пустым |

Текущий container всегда создаёт девять directory entries, поэтому число blocks само по себе не доказывает наличие canonical event facts или metric/state data. Canonical-empty block имеет нулевые item count и body и остаётся отличим от отсутствующего required block.

Target layout допускает partitioning по kind, logical factor/source ID и time range, чтобы query декодировал только пересечение и соседний halo. Текущий PGKOVF writer создаёт ровно один baseline block каждого kind и отвергает duplicate kind; multi-block partitioning требует явного format/schema extension.

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
<cache_root>/overview/v1/<source_scope_hex>/<prefix>/<fact_key_hex>-<segment_lineage_hex>.ovf
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

Полная immutable build identity:

```text
FactBuildKey = (FactKey, SegmentLineageId)
```

`FactKey` остаётся content-addressed identity для descriptor/version axes. `SegmentLineageId` отделяет разные retained occurrences с одинаковым содержимым. Поэтому `FactBuildKey`, а не один `FactKey`, квалифицирует:

- durable filename;
- owner lock `.lock-<fact_key_hex>-<segment_lineage_hex>`;
- process-local fallback residency;
- будущий cancellation-safe fact-build single-flight;
- decoded fact-block cache entry.

Два segments с одинаковым `FactKey`, но разными lineage не coalesce и не используют один final file. Health/notable/response versions в `FactKey` не входят.

### 10.2 Source scope и PGM descriptor

```text
source_scope_id = SHA-256(
  "pgk-overview-source-scope-v1" ||
  normalized_store_namespace ||
  pgm_source_id
)
```

Target contract требует `normalized_store_namespace` из explicit reader configuration. Текущие внутренние constructors отклоняют пустое значение и namespace длиннее 4096 bytes. Однако production web startup при отсутствии настройки пока подставляет canonical absolute store path; это implementation gap, а не разрешённый target fallback. До §20 `PASS` этот путь должен быть удалён: перенос store path не должен неявно менять или переиспользовать identity. Правила нормализации explicit namespace являются частью deployment configuration и должны оставаться стабильными для существующего store.

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
- bit 2 `HAS_TIME_RANGE`;
- bits 8..11 codec: `0=None`; значение `1` зарезервировано для возможного будущего `Zstd`;
- остальные bits обязаны быть zero.

Текущий writer всегда пишет `BlockCodec::None`, а текущий reader принимает только его. Для `None` CRC считается по stored bytes, а `stored_len` и `decoded_len` обязаны совпадать. `Zstd` сейчас считается incompatible, не является реализованной оптимизацией и не используется в size/performance claims. Его будущее включение требует явного compatibility/versioning решения, bounded exact-length decompression и отдельной corruption suite.

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
6. Сравнить expected source scope/descriptor/source ID/range/format/file length и полный `FactBuildKey`.
7. Проверить canonical order, known flags, zero reserved fields, timestamp bounds, non-overlapping block extents и exact final `file_len`.
8. Выбрать только нужные blocks; до allocation проверить stored/decoded length и item count.
9. Проверить block CRC и `None` length equality; для любого включённого в будущем codec — decoded bound и exact-length decompression; затем проверить logical decoder invariants.
10. Проверить sorted/unique keys, enum ranges, finite floats, count overflow, references и полное потребление block.

Bad selected block отвергает весь segment fact file. Partial use хороших blocks из corrupt file запрещено: PGM rebuild остаётся однозначным fallback.

### 10.7 Durable publication

1. Создать process-unique temp в том же cache directory через `create_new`, с правами не шире `0600`; cache namespace не шире `0700`, если operator явно не задал другой безопасный режим.
2. Записать header placeholder, directory и blocks; flush.
3. Записать финальные CRC/lengths, вызвать `sync_all(file)`.
4. Повторно валидировать собственный file тем же admission path.
5. Выполнить atomic same-filesystem rename с no-clobber semantics. Если platform не даёт no-clobber rename, owner по полному `FactBuildKey` сериализует rename; существующий target не перезаписывается до validation. Content-addressed race winner допустим, loser принимает winner только после полной validation.
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
  ordered sealed FactBuildKeys ||
  boundary-halo FactBuildKeys ||
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

Build --target global admission + FactBuildKey single-flight--> Building
  ├─ PGM success -----------------------------------> ReadyMemory
  │                                                    └─ persist best-effort
  │                                                         ├─ success -> ReadyDisk
  │                                                         └─ failure -> PersistBackoff
  ├─ source failure ---------------------------------> SourceFailed
  └─ fact safety limit ------------------------------> Uncacheable
```

`Missing`, `Incompatible`, `Corrupt`, `WrongSource` и cache I/O error являются soft cache errors. `SourceMissing`, `SourceIo`, `SourceCorrupt` и `UnsupportedLayout` влияют на result coverage и не переименовываются в cache miss.

Текущий M4 path уже выполняет durable lookup и bounded fallback до raw build, но отдельного fact-build single-flight и weighted global cold-work admission ещё нет. Переход к `Building` выше является target M5 contract; он не описывает уже существующий response-level `ResponseKey` flight.

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
L1f memory: admitted publication-failure fallback
L2 memory:  byte-bounded decoded fact blocks + projections
L3 memory:  exact serialized response cache
```

L1 переживает restart. L1f/L2/L3 очищаются при restart. Ни один слой не меняет correctness semantics нижнего слоя.

Текущий `FactStore::load_or_build` соблюдает строгий порядок: durable read → lookup в fallback → raw PGM build → best-effort durable publication. L1f заполняется только после recoverable publication failure уже построенного и admitted fact set. Это deterministic LRU по полному `FactBuildKey`, одновременно ограниченный canonical bytes и суммой segment-hours; oversized entry обслуживает текущий request, но не остаётся resident. Общий decoded-block/projection L2 остаётся target work. Текущие exact responses уже byte-bounded и имеют отдельный `ResponseKey` single-flight.

### 13.2 Memory fact/projection cache

- Key decoded fact block: `(FactBuildKey, directory entry identity)`.
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

Полная state machine с retry/backoff, one-GC retry и background probe — target M5. Текущая реализация уже возвращает корректно admitted facts через bounded L1f при recoverable publication failure, но это не считается реализацией backoff/mode/quota contract.

### 13.5 Quota

- Отдельные byte budgets: disk facts, memory facts/projections, exact responses, pinned cursor views и in-flight builds.
- Disk quota считает committed files, stale namespaces, orphans и temp files.
- Временное превышение ограничено одним bounded in-flight file на writer slot.
- Eviction никогда не удаляет in-use `Arc`; file unlink безопасен только после исключения из lookup и с учётом platform semantics.
- При невозможности освободить quota system остаётся memory-only, а не обрезает facts.

## 14. Конкурентность, single-flight и admission

### 14.1 Per-key single-flight

Целевой single-flight key равен полному `FactBuildKey`. Build принадлежит registry-owned task, а не request leader:

```text
get_or_build(build_key):
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

Текущий M4 single-flight действует только для exact HTTP `ResponseKey`; он не coalesce cold fact extraction. M5 обязан добавить отдельный registry по `FactBuildKey` и не объединять разные `SegmentLineageId`.

### 14.2 Global cold-work bounds

Fact-build single-flight не защищает от одного запроса на сотни разных cold segments. Target M5 одновременно требует:

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

Routes `/v1/timeline/overview`, `/v1/timeline/health` и `/v1/timeline/events` существуют в текущем M4. Текущие query contracts: overview — `source/from/to`; health — `source/from/to/step`; events — repeatable `source`, `from/to/limit/cursor/min_severity/kind`. `profile` и `entity` ниже остаются target additions. Структуры задают target schema; фактический production surface проверяется OpenAPI/handler fixtures, а незаполненные canonical blocks не считаются доступными только из-за наличия поля в target response.

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
GET /v1/timeline/overview?source=...&from=...&to=...
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
GET /v1/timeline/health?source=...&from=...&to=...&step=...&profile=...
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
GET /v1/timeline/events?source=...&from=...&to=...&limit=...&cursor=...
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

В текущем response schema `events` сериализует web `EventFact` projection из §7.4, а не canonical `EVENT_FACTS` block. M6 presentation acceptance проверяет именно production handler/OpenAPI JSON: stable machine fields, `event_id`/`event_instance_id`, supporting evidence, occurrence count и loss. Наличие настоящего UI и render coverage из этого не следует.

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

- current `EventObservation`, factor/sample/coverage types и target canonical `EventFact`;
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

Целевая зона ответственности:

- mutable live builder lifecycle и atomic `IndexView` publication;
- byte-bounded memory caches;
- single-flight registry и global cold-work admission;
- request planning, exact response cache и cursor view registry;
- HTTP validation/serialization;
- background retry/GC orchestration и metrics export.

Внутреннее разбиение: `live`, `view`, `admission`, `memory_cache`, `response_cache`, `cursor`, `handlers`. Новый crate для v1 не нужен: disk index имеет одного reader consumer, а чистая алгебра уже помещается в analytics.

Текущий M4 уже имеет atomic publication пары snapshot/timeline, handlers, pinned cursors, byte-bounded response cache, `ResponseKey` flight и fail-fast heavy-analysis limit. Fact-build flight, weighted cold admission, persistent backoff, quota/GC и source scrub из списка выше остаются target M5.

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

Текущий oracle уже сравнивает retained observations/counts/coverage в реализованном log data scope и имеет restart-warm/raw-index checks. M6 не считается закрытым, пока oracle не охватит все заполненные canonical event/sample/reset/state blocks, live/seal variants, ranges, identities, units и loss из §7.6.

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
- для любого включённого в будущем compressed codec: decompression bomb/decoded length mismatch;
- oversized counts/strings/vectors;
- unsorted/duplicate keys, invalid enums, NaN/infinity;
- missing/unknown required block;
- unknown optional accelerator skip;
- wrong source scope/descriptor/range/source ID;
- publish race, invalid winner и stale temp cleanup;
- cross-version rebuild without in-place mutation.

Каждый invalid fact file либо даёт raw fallback, либо source error от PGM. Panic/OOM allocation от untrusted lengths запрещены.

### 17.7 Cache, concurrency и cancellation

- N concurrent misses одного `FactBuildKey` выполняют один build.
- Одинаковый `FactKey` с разными `SegmentLineageId` выполняет независимые builds и создаёт независимые fallback entries.
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
9. `oracle-profile`: зафиксированные raw и fact fixtures, exact data cardinality, один versioned host/filesystem profile.

Process-cold и storage-cold/page-cache-cold называются отдельно. Нельзя выдавать warm OS page cache за cold disk.

### 18.3 Измерения

- p50/p95/p99 wall latency;
- CPU time и peak RSS;
- file opens, reads, writes, fsyncs и bytes;
- PGM bodies/sections/rows decoded;
- fact blocks read/decoded;
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
5. `range-cold/facts-warm`: `0` PGM body reads/decodes; p95 не больше 50% `derived-cold` p95 на том же exact fixture/profile.
6. Если HTTP/JSON доминирует в endpoint measurement, отдельно запускается microbenchmark `compact facts read + bucket`; raw results обоих измерений сохраняются.
7. 16 identical cold misses: один fact build, 16 successful responses, без overload/503.
8. Disjoint cold workload остаётся в global budget; RSS/FD/build concurrency не выходят за configured caps.
9. Completed active frame visibility p95 не больше 2.5 s при 1-second refresh loop; pending tail явно отмечен.
10. Formula/notable-only bump: zero PGM reads, unchanged fact files и mtimes.
11. Fixed metric fact component измеряется как exact encoded bytes с относимой долей header/directory и allocation overhead на retained sample. Variable event/string bytes выводятся отдельно. Qualification блокируется, пока результат не укладывается в заранее утверждённые disk и dense-hour budgets; универсальный bytes/sample claim без artifact запрещён.
12. Cache quota stress: steady-state не выше configured quota; temporary excess не больше bounded in-flight publication budget.
13. Memory-only dense-hour request остаётся корректным; если working set ниже ceiling, повтор обслуживается без PGM reread.

Charts не входят в fixtures или gates §18. Их размер и latency остаются неизмеренными до отдельного owner-approved contract.

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

Structured logs включают request ID, source scope, `FactKey` и lineage prefixes, view generation и error chain. Full paths, raw patterns и user/database text не логируются без explicit debug/redaction policy.

## 20. Критерии приёмки parity v1

### 20.1 Target acceptance и текущий статус

Статусы «Есть в current scope», «Частично» и «Открыто» описывают implementation snapshot, а не waiver. Parity v1 принимается только когда все строки имеют доказанный `PASS` на одном exact release head.

| ID | Обязательное target acceptance | Текущий статус |
| ---: | --- | --- |
| 1 | Valid disk facts после process restart дают zero PGM body reads для sealed interior | Есть в current log scope: durable file читается без PGM bodies |
| 2 | Raw-vs-index semantic equality доказана для exact/partial ranges, single/multi-segment и sealed+live | Частично: observations/counts/coverage покрыты; canonical event/sample/reset/state blocks ещё пусты |
| 3 | Random partition/seal invariance не находит duplicates, loss или boundary drift | Частично: current observation/live identity имеет properties; полный target data set не проверен |
| 4 | Missing/corrupt/incompatible/wrong-source cache всегда fallback-ится; cache persistence failure не ломает correct response | Есть в current log scope, включая admitted bounded fallback после recoverable publication failure |
| 5 | Source corruption остаётся видимым source gap/error и не скрывается derived data после обнаружения | Частично: source errors отделены от cache misses; bounded background source scrub ещё не реализован |
| 6 | Formula/notable response change не rebuild-ит facts, пока facts содержат нужные dimensions | Есть для текущих dimensions: policy versions не входят в `FactKey` |
| 7 | Каждый retained notable item проходит cursor scan ровно один раз; grouped count и upstream loss сохраняются | Частично: current observation projection и cursor это проверяют; canonical `EventFact` input отсутствует |
| 8 | Stable event IDs переживают normal live→seal handoff; `store_namespace` задаётся явно; ограничения content-derived identity опубликованы | Частично: handoff и different-lineage isolation есть, но web startup ещё допускает path-derived namespace |
| 9 | Canonical live builder lossless; hard truncation маркирует `Incomplete` и запрещает promotion | Есть для текущего observation data scope |
| 10 | Required-domain gap всегда даёт numeric `None`, никогда green one | Частично: current event-only health сохраняет unknown, но required metric factors не materialized |
| 11 | Trusted floor evidence сохраняется при partition, seal и worst downsample; numeric unknown остаётся unknown | Частично: evidence-quality floor rules существуют; полная factor/partition qualification открыта |
| 12 | Per-factor coverage/applicability/loss присутствует; один display ratio не используется как correctness gate | Открыто для metric factors; current coverage описывает retained event scope |
| 13 | Counter rates используют actual adjacent interval, reset families и gaps; storage boundary не меняет результат | Частично: algebra существует в analytics, production `COUNTER_SAMPLES`/`RESET_MARKERS` пусты |
| 14 | Каждая target taxonomy/factor family имеет проверяемый source→retained row→fact/sample/state→identity/units/reset→coverage/loss mapping и представлена fact/factor либо explicit gap | Частично: §7.6 подтверждает восемь log layouts; остальные mappings остаются explicit gaps |
| 15 | Cache hits обходят cold admission; identical `FactBuildKey` misses single-flight, disjoint misses globally bounded | Открыто для fact builds; текущий M4 объединяет только identical `ResponseKey` |
| 16 | Memory-only fallback ограничен bytes и segment-hours; retry/backoff наблюдаемы, dense-hour sizing проверено | Частично: dual-budget LRU есть; persistence modes/backoff и dense-hour qualification открыты |
| 17 | Quota/GC учитывает все derived bytes, безопасен при concurrent readers и никогда не удаляет source | Открыто |
| 18 | Benchmark modes и gates §18 воспроизводимо проходят на зафиксированном host/filesystem profile | Открыто; текущие точечные measurements не являются M6 dossier |

### 20.2 Доказательство приёмки

Для каждой строки §20.1 итоговый пакет обязан связать exact requirement с test/benchmark name, fixture schema, exact git head, CI run/attempt/jobs, artifact checksum, raw result и pass/fail. Любой `Частично`, `Открыто`, mixed-run artifact или результат не с exact head блокирует parity qualification; требование при этом не ослабляется.

M6 presentation evidence применяется к существующим production handlers и OpenAPI/JSON fixtures. Оно обязано проверить `score=null`/unknown при required gap, сохранение trusted floor, явные loss/partial/applicability, отсутствие interpolation и locale-neutral stable machine fields. Это API/presenter acceptance, не утверждение о render coverage несуществующего UI.

## 21. Вехи реализации parity v1

Все этапы ниже входят в parity v1. Persistent disk index остаётся обязательным и не переносится в будущую продуктовую веху.

### 21.1 Фактически влитые M0–M4

| Веха | Merge | Реализовано в текущем data scope | Не закрывает target contract |
| --- | --- | --- | --- |
| M0 | PR #97, `ed812259a64f12f5b12f75cb87bde3939ce9de7f` | `EventObservation`, counts/coverage/reduction contracts, half-open oracle и базовые properties | Canonical `EventFact` и production metric mappings |
| M1 | PR #98, `8cfc560cf7d927514b649719fe794fb5805e2eb7` | Selective PGM reads, descriptors, targeted dictionary resolution, bounded PGKOVF header/directory/block admission | Наполнение всех target blocks; `Zstd` не реализован, current codec — `None` |
| M2 | PR #99, `6a21924b797d4b6aa5e55c931d42166f6ec418cf` | Extraction восьми log layouts, sealed `SegmentFacts`, atomic no-replace durable publication, raw fallback и restart-warm path | `EVENT_FACTS`, gauges, counters, resets и entity states остаются canonical-empty |
| M3 | PR #100, `be39d8c1f9989565def797bb32eb0d7fb72ef894` | Authoritative refresh, live/seal state machines, lineage, provenance-gated promotion, durable-first dual-budget fallback LRU | Persistence backoff, quota/GC, source scrub и fact-build admission |
| M4 | PR #103, `0c1a37e347d112b02943aaed0c28f6ad90a4e7f3` | Atomic publication одной snapshot/timeline pair, timeline APIs, pinned cursors, byte-bounded response cache и `ResponseKey` single-flight | Canonical event/metric data, explicit-only startup namespace, fact-build single-flight, weighted cold admission и full numeric health |

Test-only split PR #106 (`0b3d0bacfa1f4253294e46daa06681b719e9b19e`) не меняет production semantics. Demo PR #105 (`d6852a64759ce6852ec6b2492ba248961ed4c4a6`) не является доказательством §20 или M6 qualification.

### 21.2 Data closure перед финальной qualification

Следующие slices заполняют уже зарезервированный target schema. Каждый начинается только после подтверждённой строки source mapping; неизвестные units/reset/entity/coverage остаются explicit gap.

1. Материализовать policy-neutral `EVENT_FACTS` для поддерживаемых observations, сохранив supporting IDs, grouped counts, evidence и lineage. `NotablePolicy` остаётся projection, `IncidentDiagnosis` не персистится.
2. Заполнить `COUNTER_SAMPLES` и `RESET_MARKERS` только для существующих PGM sections с доказанными unit, series/entity identity, reset family, coverage и loss.
3. Заполнить `GAUGE_SAMPLES`, `ENTITY_STATES` и factor coverage только из natural timestamped samples и complete bounded populations. Готовые health points и chart buckets не сохраняются.
4. Интегрировать versioned health profiles только после решения владельца по curves, required profiles и thresholds.

M5 infrastructure может разрабатываться параллельно data closure, но M6 full oracle и итоговый §20 dossier не начинаются как финальная qualification до заполнения согласованного data scope.

### 21.3 M5. Resilience и admission

M5 делится на независимо проверяемые обязательные slices:

1. Cancellation-safe fact-build single-flight по `FactBuildKey`, включая different-lineage isolation, waiter cancellation и typed terminal cleanup.
2. Weighted/fair global cold-work admission: PGM bytes, decoded bytes, CPU, FD/read/write/publication bounds, bounded workers/queue, per-request parallelism и `Retry-After`.
3. Persistent cache modes и retry/backoff для `EROFS`, `EACCES`, `ENOSPC`, quota и transient I/O; один bounded GC retry и successful background probe.
4. Disk quota и retention-safe GC по authoritative successful scan, two-generation grace и single owner; concurrent readers безопасны, PGM/`active.parts` никогда не удаляются.
5. Bounded source scrub, который после обнаружения source damage не позволяет старому derived file маскировать gap.
6. Все metrics §19, dense working-set accounting и typed overload/persistence diagnostics.

Во всех slices сохраняются durable-first order, dual-budget fallback, atomic fact publication и atomic view publication.

### 21.4 M6. Parity qualification

M6 также проходит отдельными gates:

1. Полный forced-raw oracle, metamorphic partition/seal/live suite и corruption/admission suite для всех реализованных canonical families.
2. Versioned dense-hour fixture и измерение decoded/resident/pinned overhead относительно утверждённых deployment budgets.
3. Cold/restart/hot/range-warm/live/concurrent/memory-only benchmarks §18 на одном exact host/filesystem profile.
4. Production API/presenter fixtures §20.2. Настоящие render tests появляются только вместе с отдельным owner-approved UI contract и не подменяются fake UI.
5. Единый machine-readable dossier, который закрывает все 18 строк §20.1 на одном exact head.

Charts не входят ни в data closure, ни в M5/M6. Возврат к ним требует отдельного решения владельца, exact series inventory, source mapping, реально поддерживаемого codec и измеренного encoded delta.

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

Следующие решения уже закрыты:

- target contract требует explicit non-empty `store_namespace`; path-derived fallback не допускается, а имеющийся startup fallback остаётся implementation gap, не открытым продуктовым выбором;
- charts owner-deferred и не входят в parity v1; их стоимость не измерена;
- пока production UI отсутствует, M6 проверяет API/presenter fixtures, а не заявляет render coverage.

Открыты четыре калибруемых продуктовых решения:

1. Конкретные factor curves, required profiles и state thresholds после выбора outcome и calibration fixtures.
2. Deployment budgets disk/RAM/FD/build queue и cursor TTL в пределах absolute safety caps.
3. Maintenance/topology declarations, которые позволят подавлять planned shutdown и определять required replication members.
4. Scope и rendering/i18n contract будущего production UI, если владелец решит его добавить.

Их изменение версионирует policy/configuration, но не меняет PGM, physical fact identity или data-honesty invariants.
