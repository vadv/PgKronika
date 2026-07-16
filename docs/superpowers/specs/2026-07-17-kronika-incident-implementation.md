# Контракт реализации incident-анализа

Дата: 2026-07-17. Статус: целевой дизайн; код incident-движка не реализован.

Этот документ задаёт границы, поток данных, ресурсную модель, API и порядок
реализации `GET /v1/incidents`. Семантика отдельных линз задана в
[`2026-07-16-kronika-incident-lenses-design.md`](2026-07-16-kronika-incident-lenses-design.md).

## 1. Цель и потолок атрибуции

Движок группирует аномальные эпизоды завершённого периода, применяет к каждому
кластеру подходящие PostgreSQL/Linux-линзы и возвращает проверяемые
диагностические гипотезы.

Результат не является root-cause analysis. PgKronika не располагает
непрерывным ASH-подобным потоком wait events и аддитивным бюджетом DB time,
поэтому не приписывает долю задержки запросу, ожиданию или ресурсу. Термин
`root cause` не используется в API.

Допустимые роли finding:

- `lead` — направление подтверждено структурным ребром или известным порядком
  наблюдений;
- `amplifier` — наблюдение могло усилить инцидент;
- `downstream` — наблюдение совместимо со следствием известного lead;
- `coincident` — временное совпадение без доказанного направления.

В первом срезе единственный структурный `lead` — blocker, который присутствует
в сохранённом `pg_locks.blocked_by` конкретного снимка. Это доказывает только
направление lock wait в момент снимка, но не причину долгой транзакции и не
непрерывность ребра на всём интервале.

## 2. Архитектурное решение

### 2.1. Целевая топология workspace

Целевая топология содержит 13 активных packages:

```text
kronika-format
kronika-derive
kronika-registry
kronika-writer
kronika-store
kronika-analytics        [diff, anomaly]
kronika-reader
kronika-bdd
pg_kronika-collector     [sources::pg, sources::os, sources::log]
pg_kronika-web
pg_kronika-archiver
pg_kronika-dump
xtask
```

`kronika-charts`, `kronika-store-http` и `kronika-store-s3` исключаются из
активного workspace до появления реализации и реального потребителя.

`kronika-diff` и `kronika-anomaly` механически объединяются в
`kronika-analytics`. Новый package не содержит сведений о PostgreSQL, Linux,
registry, reader, Axum или JSON. Его модули:

```text
crates/kronika-analytics/
  src/
    lib.rs               # контролируемые re-export существующих типов
    diff/                 # DiffPoint, Reason, Scalar, diff_pair
    anomaly/              # score_window, episodes и чистый scan numeric timelines
```

Первый incident-срез не создаёт package `kronika-incident`: реальный consumer
пока один — `pg_kronika-web`. PostgreSQL/Linux-правила размещаются в приватном
domain-модуле web-пакета.

### 2.2. Направление зависимостей

Стрелка означает «пакет слева зависит от пакета справа»:

```text
kronika-registry  -> kronika-derive
kronika-writer    -> kronika-format + kronika-registry
kronika-store     -> kronika-format
kronika-analytics -> std
kronika-reader    -> kronika-format + kronika-store
                     + kronika-registry + kronika-analytics

pg_kronika-collector -> kronika-format + kronika-registry + kronika-writer
pg_kronika-web       -> kronika-reader + kronika-registry + kronika-analytics
pg_kronika-archiver  -> kronika-format + kronika-store
pg_kronika-dump      -> kronika-format + kronika-store
                        + kronika-registry + kronika-reader + kronika-analytics
kronika-bdd          -> pg_kronika-web + kronika-reader
                        + kronika-registry + kronika-format
xtask                -> production graph не входит
```

`kronika-analytics` не зависит от reader: reader собирает `SeriesDiff` и
`SeriesValues` из storage/registry data и вызывает чистые функции analytics.
Incident-каталог не входит ни в analytics, ни в registry.

### 2.3. Неподвижные границы

- `kronika-derive` остаётся отдельным proc-macro package.
- `kronika-format` отдельно владеет durable byte contract.
- `kronika-registry` отдельно владеет `type_id`, layouts и column semantics.
- `kronika-writer`, `kronika-store` и `kronika-reader` не объединяются: у них
  разные durability, dependency и resource contracts.
- Четыре deployable binaries остаются отдельными packages.
- `kronika-bdd` и `xtask` не входят в production graph.

## 3. Ответственность и файлы

Целевое размещение incident-кода:

```text
bins/pg_kronika-web/src/
  incident/
    mod.rs
    model.rs             # domain types, state, canonical key
    cluster.rs           # deterministic sweep-line
    evidence.rs          # evidence ceiling и confidence
    lens.rs              # Lens, SectionColumn, EvalContext
    catalog.rs           # PostgreSQL/Linux rules и inverted index
    engine.rs            # bounded evaluation над prepared input
  incident_input.rs      # adapter reader -> domain input; I/O и gates
  handlers/incidents.rs  # query validation, semaphore, HTTP errors
  serialize.rs           # domain result -> JSON
```

`incident/{model,cluster,evidence,lens,catalog,engine}.rs`:

- не импортируют `axum`, HTTP status/response types или `serde_json`;
- не открывают files и не обращаются к `LocalDirSnapshot`;
- не сериализуют JSON;
- могут знать имена PostgreSQL/Linux sections и колонок из каталога линз.

`incident_input.rs` вызывает reader APIs: `section`, `logical_section`,
`diff_section`, `gauge_section`, `gate_readings` и
`apply_collection_gating`. Он отдаёт движку owned `PreparedIncidentData` и не
возвращает файловые handles или ссылки на snapshot.

`handlers/incidents.rs` отвечает только за параметры, permit, вызов input
adapter/engine, status codes и передачу результата в `serialize.rs`.

### Условие выделения отдельного package

Incident-domain выделяется из web только при выполнении хотя бы одного
проверяемого условия:

- появился второй реальный consumer;
- появился отдельный binary или самостоятельный API;
- нужен независимый fuzz/benchmark/release lifecycle;
- измерено, что web dependency мешает compile/test isolation.

Планируемый consumer, каталог линз или размер модуля сами по себе не являются
основанием для нового package.

## 4. Текущее состояние и предусловия

| Контракт | Состояние | Следствие для первого среза |
|---|---|---|
| `LogicalSection::diff_key()` | реализован | ключ серии берётся из `identity`, иначе из `sort_key` без `ts` |
| `gated_by -> Reason::NotCollected` | реализован в registry/reader и применяется в single diff, batch diff и anomalies | I/O timing может использовать typed no-data; ноль при выключенном gate не считается измерением |
| PG18 row override | реализован: `object=wal` выбирает `track_wal_io_timing`, прочие строки — `track_io_timing`; `writeback_time` остаётся на `track_io_timing` | PG18 WAL/non-WAL timing не требует нового incident workaround |
| `track_planning` | queryable gate отсутствует в `reset_metadata`; planning columns не объявляют полный gate contract | planning-ветки dormant до нового registry layout/`type_id`, source mapping и propagation tests |
| producer-session coverage | `reset_metadata` хранит GUC collector session | `gate=on` не доказывает полноту агрегата всех producer sessions; положительный timing допустим, ноль не даёт отрицательного вывода, confidence снижается |
| source period и clock domain | reader/`Semantics` их не несут | P/I/D direction dormant; первый срез не назначает temporal `lead`/`downstream` |
| `pg_locks.blocked_by` | collector один раз вычисляет `pg_blocking_pids`, сохраняет deduplicated directed list и при превышении lock cap пропускает весь snapshot вместо усечения | sampled edge разрешает blocker `lead`; пропущенный/неполный snapshot не разрешает направление |
| `type_id` после union | `SectionPage` и `SeriesDiff` его не сохраняют | `type_id` не входит в episode key, evidence или API первого среза |
| unresolved `StrId` | `cell_to_value` различает его, но `SectionPage` отбрасывает signal и оставляет `Value::Null` | любой `Null` в identity консервативно исключает episode как `identity_null_or_unresolved`; точный `dropped_unresolved` нельзя обещать без нового reader contract |
| каталог линз | отдельный design contract | implementation использует только ветки со статусом ready; остальные явно возвращаются как dormant |

Новый registry layout для `track_planning` обязан получить новый `type_id`;
старые on-disk layouts не переопределяются. Данные, записанные до появления
layout, остаются читаемыми.

## 5. Domain model, поток данных и ownership

### 5.1. Поток

```text
LocalDirSnapshot
  -> reader::section по одному logical section
  -> SectionPage { rows, gaps, next_cursor }
  -> reader diff/gauge + collection gating
  -> kronika-analytics::anomaly numeric scan
  -> EnrichedEpisode без type_id
  -> deterministic clusters
  -> private incident catalog/evidence engine
  -> domain IncidentsResponse
  -> thin JSON adapter
```

`section()` используется по одному имени. `sections()` имеет общий
all-or-nothing materialization budget на batch; отказ одной широкой секции не
должен скрывать пригодные данные остальных sections.

### 5.2. Типы

Окончательные имена могут следовать стилю реализации, но поля и инварианты
контракта фиксированы:

```rust
struct EpisodeRefV1 {
    logical_section: &'static str,
    column: &'static str,
    identity: Arc<[IdentityValue]>,
    start_us: i64,
    end_us: i64,
}

struct EnrichedEpisode {
    episode: kronika_analytics::anomaly::Episode,
    reference: EpisodeRefV1,
}

struct PreparedIncidentData {
    episodes: Vec<EnrichedEpisode>,
    series: SeriesSet,
    coverage_by_section: BTreeMap<&'static str, Vec<kronika_reader::Gap>>,
    data_quality: DataQuality,
}
```

`IdentityValue` допускает только canonical scalar variants, реально
разрешённые `diff_key`: signed/unsigned integer, boolean и resolved UTF-8 text.
`Value::Null`, float, blob и list в identity не кодируются: episode исключается
с typed data-quality reason.

Одна identity хранится в `Arc<[IdentityValue]>` и разделяется эпизодами серии.
`logical_section` и `column` берутся из registry static names. `SeriesSet`
владеет numeric buffers на время запроса; линзы получают slices и не копируют
series.

### 5.3. Детерминированный ключ

```text
IncidentKeyV1 = (
  node_self_id,
  incident_start_us,
  incident_end_us,
  sorted EpisodeRefV1[]
)

EpisodeRefV1 = (
  logical_section,
  column,
  typed identity values,
  start_us,
  end_us
)
```

`type_id` отсутствует. Union reader не позволяет восстановить его надёжно, а
выбор layout последней точки был бы описательным предположением.

Canonical encoding содержит version byte, variant tag, length и bytes каждого
поля. `DefaultHasher` и process-local hash запрещены. Если `node_self_id` не
прочитан или не разрешён, ключ не строится: ответ помечает request как
`missing_node_identity` и не выдаёт incident с подменным id.

Episodes сортируются по `(start_us, end_us, logical_section, column, identity)`.
Findings сортируются по `(confidence desc, role_rank, lens_id, scope_key)`, где
`role_rank = lead, amplifier, downstream, coincident`. `scope_key` использует
то же canonical encoding.

## 6. Движок

### 6.1. Чтение, gating и coverage

Input adapter:

1. Разрешает logical sections через `logical_section`.
2. Читает gate sections и data sections вызовами `section()` с существующим
   `DIFF_MAX_ROWS = 262_144`.
3. Отвергает page с `next_cursor`: анализ неполной страницы запрещён.
4. Применяет `diff_section`, затем `apply_collection_gating`; gauges не
   дифференцирует.
5. Переносит `SectionPage.gaps` в `coverage_by_section`.
6. На `QueryError::ResultTooLarge` или неполную page добавляет section-level
   `skipped` и продолжает. `UnknownSection`, повреждение/ошибка чтения и
   несогласованный registry contract завершают запрос typed error: они не
   маскируются как отсутствие аномалий.

Текущий `Gates` находится в web handler. До incident behavior его orchestration
переносится в reader как нейтральный `GateSet` с операциями
`required_sections`, `from_pages` и `apply`; значения gate остаются registry
contract, а endpoint перестаёт дублировать их загрузку.

`data_age_seconds` считается по последнему `UnitMeta.max_ts` выбранного source.
Текущий одноимённый helper приватен для `/metrics`; его следует вынести в общий
web helper. Нет units или валидного timestamp — JSON `null`, не `0` и не `NaN`.

### 6.2. Ресурсные границы

Наследуемые точные границы current code:

| Ось | Текущий предел | Применение |
|---|---:|---|
| rows одной section scan | `DIFF_MAX_ROWS = 262_144` | `section(..., limit, ...)`; cursor означает incomplete scan |
| materialized cells одного reader query | `MAX_MATERIALIZED_CELLS = 10_000_000` | reader возвращает `ResultTooLarge` до превышения |
| anomaly point-position work | `MAX_SCORE_WORK = 50_000_000` | общий бюджет numeric scan на request |
| anomaly positions | `MAX_POSITIONS = 10_000` | период/step отклоняется до чтения |
| одновременный тяжёлый analytic request | `1` | общий permit для anomalies/incidents |

Новые incident ceilings нельзя выбирать по аналогии. Следующие значения
блокируют merge incident behavior, пока не утверждены load-budget измерением на
нормальной машине:

| Compile-time ceiling | Что ограничивает | Как принимается значение |
|---|---|---|
| `MAX_INCIDENT_QUERY_SPAN_US` | запрошенный reader-интервал `[from,to]` и расширенный P/D-интервал | retention profile + worst-case section load; endpoint default строго ниже ceiling |
| `MAX_INCIDENT_SERIES_POINTS` | суммарные owned points в `SeriesSet` | peak RSS на full first-slice catalog |
| `MAX_INCIDENT_EPISODES` | episodes до кластеризации | anomaly worst case при текущем `MAX_SCORE_WORK` |
| `MAX_INCIDENT_CLUSTERS` | clusters после span split | RSS и deterministic sort cost |
| `MAX_INCIDENT_LENS_EVALUATIONS` | candidate lens × cluster после inverted index | worst-case catalog dispatch benchmark |
| `MAX_INCIDENT_FINDINGS` | retained findings на request/cluster | response-size budget и heap cost |
| `MAX_INCIDENT_EVIDENCE_ROWS` | evidence rows на finding/request | serialized response-size budget |
| `MAX_INCIDENT_WORK` | points inspected + lens evaluations + evidence rows built | wall-time/RSS load budget; charge formula фиксируется до benchmark |

Каждый ceiling — compile-time hard limit. Server config задаёт меньший operational
limit; HTTP query не может его поднять. Исчерпание возвращает
`complete=false` и typed `skipped { scope, reason, observed, limit }`.
Детерминированный evaluation order делает частичный результат воспроизводимым.
Никакой limit не применяется молча.

Для anomalies и incidents используется общий `ANALYTIC_REQUESTS` semaphore с
одним permit — это уже действующий concurrency ceiling anomalies. Handler
использует `try_acquire`: занятый permit даёт `503` и `Retry-After`, а не
неограниченную очередь. Переход anomalies с текущего `acquire().await` является
отдельным явно проверяемым behavior change, не частью crate move.

### 6.3. Кластеризация

Sweep-line объединяет следующий episode, если:

```text
next.start_us <= current_end_us + epsilon_us
```

и новый span от первого start до `max(current_end_us, next.end_us)` не превышает
`max_cluster_span_us`. При слиянии:

```text
current_end_us = max(current_end_us, next.end_us)
```

Переполнение checked; отказ записывается как typed skip. Complexity —
`O(E log E)` из-за сортировки. Разбиение по span отражается в data quality.

### 6.4. P/I/D и часы

Для линзы `L`:

```text
h(L) = max(window_us, 2 * source_period(L), clock_skew_us)
P = [incident.start_us - h, incident.start_us)
I = [incident.start_us, incident.end_us)
D = [incident.end_us, incident.end_us + h)
```

Вся арифметика checked и ограничена temporal ceiling. `source_period` — runtime
период collector source, не медиана timestamps. Пока period и clock domain не
переданы reader, `clock_relation = Unknown`: temporal `lead` и `downstream`
запрещены. Наблюдения первого среза получают `coincident`/`amplifier`, кроме
sampled `blocked_by` edge.

### 6.5. Линзы и dispatch

```rust
trait Lens {
    fn id(&self) -> &'static str;
    fn inputs(&self) -> &'static [SectionColumn];
    fn confidence_cap(&self) -> Confidence;
    fn evaluate(
        &self,
        cluster: &Cluster,
        series: &SeriesSet,
        context: &EvalContext,
        budget: &mut WorkBudget,
    ) -> Vec<Finding>;
}

struct SectionColumn {
    section: &'static str,
    column: &'static str,
}
```

Вход задаётся logical section/column, не `type_id`. `catalog.rs` строит один
inverted index `logical_section -> lens indices`; engine не перебирает полный
каталог для каждого cluster. `evaluate` не делает I/O.

Top-K поддерживается bounded `BinaryHeap` с push-if-better. Indexed joins
разрешены только по объявленным entity keys; декартовы произведения series
запрещены.

### 6.6. Evidence, confidence и lead

`Finding` строится smart constructor-ом:

```text
confidence = min(lens.confidence_cap(), evidence_ceiling(evidence))
```

Поле confidence и конструктор `Confidence::High` приватны. High ceiling дают
только прямой sampled lock edge и точно сохранённое событие resource limit
(например, kill/OOM/ENOSPC) с известным scope. Ratio, gauge, score, нулевой
timing и текстовый verdict high не дают.

`blocked_by` применяется только при одновременном выполнении условий:

- snapshot не был пропущен по lock cardinality cap;
- waiter row и blocker id сохранены в одном snapshot;
- `blocked_by` содержит blocker id (`0` допустим только как prepared holder);
- evidence содержит snapshot timestamp, waiter, blocker и lock target/mode;
- роль описывает sampled blocking edge, а не весь incident interval.

Статическое ребро horizon -> `n_dead_tup` в первом срезе отсутствует. Без
entity join по совместимому relation key оно не доказывает, что удерживаемый
xmin относится к той же relation. Horizon и dead tuples показываются как
`coincident`/`amplifier`. Directional edge разрешается во втором срезе после
явного join contract и P/I/D clock contract.

## 7. Детерминированный HTTP API

### 7.1. Request

```text
GET /v1/incidents?source=<u64>&from=<i64>&to=<i64>
                  [&window=<duration>][&step=<duration>]
                  [&threshold=<f64>][&eps_rel=<f64>]
                  [&epsilon=<duration>][&max_cluster_span=<duration>]
                  [&limit=<usize>]
```

Hard ceilings не являются client parameters. До snapshot/read проверяются:

- `from < to`, checked span и hard query-span ceiling;
- положительные `window`/`step`, finite non-negative threshold/eps;
- число positions не выше текущего `MAX_POSITIONS`;
- `epsilon <= max_cluster_span <= query_span`;
- operational limits не выше compile-time ceilings.

### 7.2. Response

```text
IncidentsResponse {
  source_id,
  from, to,
  complete,
  incidents: [Incident],
  coverage_by_section: { section: { gaps: [{from,to}] } },
  data_age_seconds: number | null,
  catalog: {
    applied: [lens_id],
    dormant: [{lens_id, awaiting: [prerequisite]}]
  },
  data_quality: {
    dropped_identity_null_or_unresolved,
    cluster_span_splits,
    top_n_unknown: [section]
  },
  skipped: [{scope, reason, observed, limit}]
}

Incident {
  interval,
  incident_key,
  members: [EpisodeRefV1],
  findings: [Finding],
  unclassified_members: [EpisodeRefV1],
  context: [ContextFact],
  data_quality
}
```

`coverage_by_section`, `data_age_seconds`, `catalog`, `data_quality`, `complete`
и `skipped` присутствуют даже при пустом `incidents`. Пустой список не означает
полное отсутствие проблем без этих полей.

Status mapping:

- `400` — invalid parameters/config relation;
- `404` — unknown explicitly requested section, если такой filter будет добавлен;
- `413` — request не может начаться из-за hard span/position/materialization cap;
- `503 + Retry-After` — analytic permit занят;
- `500` — snapshot/read/registry invariant error;
- `200` с `complete=false` — отдельные sections/lenses пропущены по bounded
  evaluation, с полным typed `skipped`.

## 8. Срезы каталога

Readiness задаётся по ветке линзы, а не только по lens id: одна линза может иметь
готовую counter/gauge-ветку и dormant planning/timing direction.

### Первый срез

- `PG-LOCK-012`: sampled `blocked_by` — единственный structural `lead`; без
  полного snapshot линза dormant для этого интервала.
- `PG-VACUUM-005`, `PG-FREEZE-006`, `PG-HORIZON-013`, `PG-CONN-014`,
  `PG-CACHE-010`, `PG-HOT-007`, `PG-CHKPT-008`, `PG-SLOT-016`, `PG-ARCH-017`:
  только `coincident`/`amplifier`.
- `OS-CPU-020`, `OS-CGRP-021`, `OS-MEM-022`, `OS-CGMEM-023`, `OS-BLOCK-024`,
  `OS-WB-025`, `OS-IOWHO-026`, `OS-FS-027`, `OS-NET-028`: только
  `coincident`/`amplifier`; cross-clock direction запрещён.
- Counter/event branches `PG-QRY-001`, `PG-TEMP-003`, `PG-ANALYZE-004`,
  `PG-WAL-009`, `PG-REPL-015`, `PG-SYNC-018`, `PG-WAIT-019` допускаются только
  если их exact inputs из каталога имеют fixtures и не требуют P/I/D.
- `PG-IO-011` и timing branches могут использовать положительные значения и
  `NotCollected`. При `gate=on` полнота producer sessions остаётся unknown;
  measured zero не даёт отрицательного finding.
- Planning branches `PG-QRY-001`/`PG-PLAN-002` dormant до нового
  `track_planning` registry/source contract.

### Второй срез

- runtime source periods и clock domains;
- P/I/D directional roles для WAL/checkpoint, plan/execution, temp/I/O,
  slot/archiver/FS и OS/PG observations;
- entity join contracts, включая horizon -> relation dead tuples;
- `track_planning` layout/gate;
- per-entity attribution там, где registry sections имеют совместимые keys.

Каталог ответа перечисляет applied/dormant branches честно; отсутствие finding
не подменяет отсутствие prerequisites.

## 9. Проверка реализации

### Unit tests

- `kronika-analytics::diff` и `::anomaly`: все перенесённые tests без изменения
  формул, defaults и результатов.
- Cluster: epsilon/span boundaries, вложенные episodes, `max(end)`, checked
  overflow, deterministic split accounting.
- Key: отсутствие `type_id`, canonical tags/lengths, стабильный order,
  `Null`/unresolved rejection, missing `node_self_id`.
- Confidence: `min(cap, evidence_ceiling)`, private high path, high-cap без
  direction, co-leads с полным порядком.
- Work budget: каждая ось, observed/limit, `complete=false`, deterministic
  partial order.
- Clock: `Unknown` запрещает temporal lead; blocked edge не зависит от clock.

### Store/web integration

- two-segment fixtures с gaps/reset/`NotCollected`/old layouts;
- `section()` oversized skip не отменяет пригодные sections;
- union series не выдаёт `type_id`;
- empty response различает no episodes, no coverage и all lenses dormant;
- golden JSON для full/partial/invalid/busy responses.

### BDD

- live lock waiter сохраняет exact `blocked_by`; blocker получает sampled lead,
  waiter — downstream только для этого edge;
- lock snapshot over cap не создаёт lead;
- horizon/dead-tuples без entity join остаются coincident/amplifier;
- planning branch отображается dormant до нового layout;
- PG15-18 gating regressions сохраняют `NotCollected`, включая PG18 WAL override.

### Load-budget validation

До утверждения новых ceilings запускаются clean/warm build timings и
criterion/load profiles на нормальной машине. Worst case включает максимальные
rows/positions, полный первый каталог, широкий cluster и response evidence.
Фиксируются wall time, peak RSS, evaluation count и response bytes. Значения из
таблицы §6.2 выбираются из явного service budget с запасом и затем становятся
compile-time constants. Они не выводятся из числа линз или существующего
`MAX_SCORE_WORK`.

## 10. Порядок миграции

Mechanical moves и behavior changes идут разными PR.

1. **C0 — убрать пустые placeholders.** Исключить `kronika-charts`,
   `kronika-store-http`, `kronika-store-s3` из active members и dependency
   policy; сохранить условия их возврата в architecture docs. Production graph
   и поведение не меняются.
2. **C1 — создать `kronika-analytics`.** Перенести `kronika-diff` в `diff`,
   `kronika-anomaly` в `anomaly`, сохранить root re-exports текущих public
   types/functions, перенести tests без правок, обновить imports reader/web.
   Старые packages удалить в том же PR. Формулы и API semantics не менять.
3. **C2 — чистый anomaly scan.** Отделить numeric timeline scan от
   reader-specific `SeriesDiff`/`Value`: pure kernel переезжает в
   `kronika-analytics::anomaly`, web adapter оставляет identity enrichment.
   JSON и endpoint behavior не меняются.
4. **I0 — reader/web preparation.** Перенести `Gates` orchestration в reader
   `GateSet`, вынести shared data-age helper, добавить typed prepared input и
   conservative identity rejection. Incident findings ещё не включать.
5. **I1 — private domain core.** Добавить `incident/model`, `cluster`,
   `evidence`, `lens`, `catalog`, `engine` с unit tests, без HTTP route.
6. **I2 — выбрать resource constants.** Запустить §9 load-budget validation,
   зафиксировать ceilings/defaults и shared fail-fast analytic semaphore.
7. **I3 — первый каталог.** Реализовать ready branches §8; dormant registry
   является частью результата. Horizon lead не включать.
8. **I4 — endpoint.** Добавить `/v1/incidents`, JSON adapter, metrics,
   fixture/golden tests и BDD. Сохранить действующие query-honesty и resource
   bounds без ослабления.
9. **I5 — prerequisites второго среза.** Отдельно добавить period/clock,
   entity joins и новый `track_planning` layout; только затем включать
   directional/planning branches.

Перенос `source-pg`, `source-os`, `source-log` в private collector modules —
общая roadmap консолидации, но не prerequisite и не часть incident stack. Каждый
source переносится отдельным PR. До и после него измеряются clean и warm
`cargo build --timings` для collector и edit/rebuild соответствующего source на
нормальной машине. При существенном ухудшении iteration time/cache move
останавливается или откатывается; runtime benefit не предполагается.

## 11. Отклонённые варианты и компромиссы

### Incident rules в `kronika-analytics`

Отклонено: PostgreSQL/Linux names, thresholds и causal hypotheses загрязнят
source-independent diff/anomaly package и развернут dependency direction.

### Новый package `kronika-incident` сейчас

Отложено: один consumer, общий web release/test lifecycle и отсутствие
независимого artifact не оправдывают package API. Условие extraction задано в
§3.

### Один macro-crate для format/registry/store/reader/analytics

Отклонено: archiver достигнет registry/Parquet decode, изменения layout будут
инвалидировать весь core, а durable bytes смешаются с query/domain code.

### Несколько binaries в одном application package

Отклонено: Cargo features/dependencies задаются package-level; исчезнут
проверяемые границы credentials, sources, assets и storage SDK.

### `type_id` последней точки как episode identity

Отклонено: union rows теряют provenance layout. Best-effort значение нельзя
использовать ни в stable key, ни как надёжное evidence.

### Horizon -> `n_dead_tup` без entity join

Отклонено для первого среза: section-level совпадение не связывает backend xmin
с конкретной relation. Цена — меньше lead findings, но отсутствует ложная
атрибуция.

## 12. Нерешённые решения, блокирующие behavior

| Решение | Кто/какими данными закрывает | Gate |
|---|---|---|
| Числа новых ceilings §6.2 | владелец service budget по load profile §9 | I2 не merge до фиксации всех значений |
| Operational defaults `window`, `step`, `epsilon`, cluster span, limit | продуктовый выбор на реальном retention/cadence profile; compile ceiling выше, но конечен | endpoint route не включается без таблицы defaults |
| Состав optional first-slice branches из §8 | только branches с exact fixture и documented DQ | branch остаётся dormant, пока fixture отсутствует |
| Точный producer-session coverage для timing | новый collector/registry contract либо сохранённое `unknown` | negative timing findings запрещены |
| `track_planning` | новый reset metadata layout/`type_id`, source query, gated columns, old-segment tests | planning branches dormant |
| P/I/D direction | runtime period + clock-domain provenance | все temporal roles остаются coincident/amplifier |
| Horizon relation join | объявленный compatible entity key и coverage rules | cross-section horizon lead запрещён |

Эти gates не являются разрешением использовать временные числа. До их закрытия
можно merge только mechanical consolidation и чистые domain tests, не публичное
incident behavior.
