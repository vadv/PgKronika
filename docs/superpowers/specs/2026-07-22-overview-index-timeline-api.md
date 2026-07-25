# PgKronika — индекс обзора и API временной шкалы

Версия: 0.5
Дата: 2026-07-25
Статус: реализованный контракт `parity-v1`, ожидающий итоговой проверки

## 1. Цель и статус решений

PgKronika должна быстро строить обзор событий и линию состояния для
произвольного временного диапазона. Повторный запрос, запрос после перезапуска
процесса и многодневный диапазон не должны заново декодировать тела
PGM-сегментов, если рядом с ними находятся допустимые файлы фактов. Свежие
данные из `active.parts` должны появляться с задержкой не больше нескольких
циклов обновления.

`Parity-v1` включает постоянный индекс фактов на диске, которым управляет
считыватель. Хранение только в памяти может быть промежуточным этапом
разработки, но не считается выпуском `parity-v1`.

PGM остаётся единственным источником истины. Средство записи, формат PGM и
протокол запечатывания не меняются. Индекс принадлежит слою считывания, хранит
факты, не зависящие от формул представления, и может быть удалён без потери
исходных данных.

Нормативные разделы задают контракт `parity-v1`. Таблицы состояния фиксируют
поведение, реализованное в PR #114. Окончательное соответствие подтверждается
только критериями §20 на одном точном коммите и в одной попытке CI.

Числовые health-кривые и продуктовые пороги требуют отдельной калибровки. Эта спецификация фиксирует их входы, алгебру, coverage, версионирование и ограничения, но не выдаёт непроверенные пороги за доказанную модель здоровья.

Нормативные слова «обязан», «нельзя» и «допускается» задают контракт v1.
Псевдокод описывает семантику протокола и хранения, а не ABI Rust.

## 2. Определения

| Термин | Значение |
| --- | --- |
| PGM | Неизменяемый запечатанный сегмент PgKronika, источник истины для закрытого диапазона. |
| Активная часть | Завершённый кадр с допустимой CRC в `active.parts`. Незавершённый хвост частью не считается. |
| Дескриптор сегмента | Идентификатор содержимого PGM, вычисленный из его каталога, хвоста и длины. |
| Факты сегмента | Канонический индекс одного запечатанного PGM: сохранённые наблюдения и факты, отсчёты метрик с временными метками, сбросы, состояния, пропуски, покрытие и происхождение. |
| Построитель активных данных | Единственный изменяемый построитель без потерь, который сворачивает каждую завершённую активную часть ровно один раз. |
| Представление активных данных | Неизменяемый снимок активных фактов с поколением журнала и обработанной позицией. |
| Представление индекса | Атомарный снимок упорядоченных дескрипторов запечатанных сегментов и одного точного поколения активных данных. Все части запроса читаются из одного снимка. |
| `EventObservation` | Сохранённое PGM-наблюдение в форме источника: отдельная строка, сгруппированная строка или пропуск. |
| `EventFact` | Канонический нормализованный факт, не зависящий от политики представления и выведенный из одного или нескольких наблюдений или отсчётов с явным происхождением. Не равен одноимённой текущей структуре веб-API из §7.4. |
| `NotablePolicy` | Версионированное чистое преобразование, которое классифицирует, выбирает и упорядочивает наблюдения и факты для `/events` и краткого списка; результат не записывается в канонические блоки. |
| `IncidentDiagnosis` | Отдельный корреляционный вывод о возможной причине с подтверждениями и оценкой уверенности. Не является наблюдением или фактом и в текущей реализации отсутствует. |
| `FactKey` | Идентификатор фактов, связывающий числовой идентификатор источника, точный дескриптор содержимого и версии контрактов; определён в §10.1. |
| `FactBuildKey` | Полный неизменяемый идентификатор одной задачи построения: `(FactKey, SegmentLineageId)`. Он используется для допуска, координации одной задачи, резервного хранения в памяти и проверки; путь файла от него не зависит. |
| Retained exactness | Точность относительно строк и counts, которые фактически дошли до PGM. Она не означает полноту исходного PostgreSQL log. |
| Проекция | Состояние, краткий список, сводка, прореживание или HTTP-представление, вычисленное из фактов для текущей версии правил. |

## 3. Продуктовый scope и parity contract

### 3.1 Входит в parity v1

- `GET /v1/timeline/overview` для компактной событийной и health-сводки диапазона.
- `GET /v1/timeline/health` для health-line с честным per-point coverage.
- `GET /v1/timeline/events` для стабильной пагинации notable observations/facts.
- Соседний файл фактов `N.ovf` для каждого запечатанного `N.pgm` в одном
  каталоге `KRONIKA_WEB_DIR`, который целиком принадлежит PgKronika.
- Инкрементальный построитель активных данных без потерь для завершённых частей.
- Прямое чтение PGM и ленивое перестроение отсутствующего, несовместимого,
  повреждённого или не соответствующего источнику OVF.
- Ограниченное хранение в памяти декодированных фактов, проекций и точных
  ответов.
- Ограниченное резервное хранение в памяти, если публикация OVF невозможна.
- Ограниченные, безопасные при отмене задачи построения и координация одной
  задачи по каждому `FactBuildKey`.
- Квоты, сборка мусора, метрики, проверки повреждений и измерения после
  перезапуска.

### 3.2 Наблюдаемый parity contract

1. Повторный одинаковый запрос к запечатанным данным в одном процессе не читает
   тела PGM и не повторяет вычисление представления, если точный ответ уже есть
   в памяти.
2. После перезапуска допустимый соседний OVF обслуживает внутреннюю часть
   запечатанного диапазона без чтения тел PGM и декодирования секций.
3. Новый диапазон, шаг или фильтр поверх уже существующих фактов не требует
   декодирования PGM, если сохранённых измерений достаточно.
4. Многодневный запрос читает только пересекающиеся компактные блоки с
   ограниченным параллелизмом.
5. Завершённый кадр активных данных становится виден не позже установленного
   предела свежести; ожидающий или оборванный хвост отмечается отдельно.
6. Запечатывание, разбиение на части и сегменты и порядок объединения не меняют
   сохранённые факты, покрытие и результат одной версии правил.
7. Ни один пропуск required data не превращается в `score=1.0`.
8. Numeric health принимается по зафиксированным oracle fixtures, coverage и отсутствию false-green; latency и размер проверяются на versioned host/filesystem profile без неподтверждённых сравнительных claims.

### 3.3 Границы v1

- Новые collectors и новые PGM sections не требуются.
- Текущий stderr source остаётся bounded и grouped; overview не обещает восстановить отброшенные строки.
- Историческая причина incident не выводится из одного token/signal/category.
- OVF не становится архивом и не продлевает срок хранения PGM.
- Charts отложены владельцем: chart extraction, chart-specific blocks, endpoint и render contract не входят в parity v1. Их стоимость не измерена и не оценивается подстановкой синтетических размеров.

## 4. Инварианты и честность данных

### 4.1 Источник истины и производные файлы

1. PGM и valid completed active parts — единственные источники фактов.
2. Файл фактов версионируется и является производным. Отсутствующий,
   несовместимый, повреждённый, слишком большой или не соответствующий PGM
   файл игнорируется и перестраивается из PGM.
3. Изменять несовместимый файл фактов на месте нельзя. Новые факты атомарно
   заменяют тот же соседний `N.ovf`.
4. Ошибка чтения, записи или сборки мусора не превращает корректно вычисленный
   ответ в ошибку.
5. Ошибка исходного PGM не маскируется как отсутствие OVF. Она становится
   пропуском источника или типизированной ошибкой источника.
6. Удаление всех производных OVF влияет только на задержку первого чтения.

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

Текущая реализация проходит весь показанный путь для поддерживаемого реестра:
создаёт `EventObservation`, канонические `EventFact`, отсчёты счётчиков и
измерений, маркеры сбросов, состояния сущностей и покрытие. Неподдерживаемая
раскладка остаётся явной ошибкой или пропуском, а не предполагаемым нулём.

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
3. Строит упорядоченный план дескрипторов запечатанных сегментов, одного
   поколения активных данных и граничных соседних отсчётов.
4. Проверяет хранилище точных ответов в памяти.
5. Загружает нужные блоки фактов из памяти или соседних OVF; при отсутствии
   запускает ограниченную общую задачу построения.
6. Обрезает observations/samples по точному диапазону и применяет reducer semantics §6.
7. Применяет текущие health/notable policies.
8. Формирует coverage/loss и response metadata из того же view.
9. Сохраняет проекцию или ответ в памяти только под полным `FactSetId`.

Запрос никогда не смешивает новый sealed set со старым live view.

### 5.3 Multi-segment merge

- Выбор сегментов использует снимок источника как авторитетный каталог
  диапазонов; каталог данных не сканируется при каждом запросе.
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

  source_id: u64,
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
  source_id ||
  source_descriptor ||
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

`first_catalog_entry_content_descriptor` строится из полей каталога, не
зависящих от смещения: `type`, `schema`, `flags`, `body_len`, `rows` и
`body_crc32c`. Поэтому для вычисления происхождения не нужно читать
не относящиеся к нему тела. `section_body_id` хеширует точное тело нужной
секции вместе с `type_id` и длиной. `catalog_entry_ordinal` считается по всему
каталогу сегмента и вместе с `row_ordinal` различает повторения одинакового
тела внутри одного происхождения. Порядковые номера сохраняются после обычного
запечатывания.

Гарантия ограничена текущим source contract:

- идентификатор стабилен при обычном переходе от активных данных к
  запечатанным и при повторном построении производных фактов;
- policy/formula version в ID не входит;
- repack/resegmentation может изменить lineage;
- числовой `source_id`, дескриптор PGM и первый дескриптор записи каталога
  определяют происхождение без пути, имени файла или отдельного идентификатора
  хранилища;
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

`EventFact` остаётся независимым от политики представления: он может утверждать
`pg.lifecycle.child_signal_termination` или `os.cgroup.oom_kill_delta`, но не
`postgres_was_killed_by_oom` без отдельной диагностики.

Текущая реализация материализует и проверяет `EVENT_FACTS`. Веб-API сохраняет
стабильную внешнюю структуру `EventFact`: для наблюдений она содержит
семантический `event_id`, физический `event_instance_id` и точное
подтверждающее наблюдение; для канонических фактов метрик и состояний она
использует `fact_id`, сущность, интервал и сохранённые идентификаторы
подтверждений. Изменение внешней формы требует повышения
`response_schema_version`.

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

### 7.6 Проверенное соответствие источников

Для всех восьми поддерживаемых секций журнала `observation_id` выводится из
`SegmentLineageId`, `source_type_id`, идентификатора тела секции, порядковых
номеров записи каталога и строки, а также контекста словаря. Происхождение
запечатанного сегмента включает числовой `source_id` и точный дескриптор PGM и
не зависит от пути. Идентификатор активных данных имеет качество `Approximate`
до доказанного перехода.

| Секция PGM | Канонический результат | Единицы, происхождение и потери |
| --- | --- | --- |
| `1_022_001 pg_log_errors` | Сгруппированное `EventObservation::ErrorGroup` и соответствующий `EventFact`; `occurrence_count` равен сохранённому `count` | Число сохранённых появлений; усечение словаря и потеря групп отмечаются явно |
| `1_024_001 pg_log_checkpoints` | Отдельное наблюдение и факт начала, завершения или слишком частой контрольной точки | Миллисекунды, KiB и счётчики остаются типизированными полями; потери словаря и качество времени сохраняются |
| `1_025_001 pg_log_autovacuum` | Отдельное наблюдение и факт autovacuum или autoanalyze | Единицы исходных полей сохраняются; несуществующая сущность или сброс не придумываются |
| `1_026_001 pg_log_slow_queries` | Сгруппированное наблюдение и факт медленного запроса | Длительность в миллисекундах, число сохранённых появлений и время представительного запроса |
| `1_027_001 pg_log_lock_waits` | Отдельное наблюдение и факт ожидания или получения блокировки | Длительность в миллисекундах; неподтверждённое постоянство сущности не предполагается |
| `1_028_001 pg_log_lifecycle` | Отдельное наблюдение и факт сигнала дочернему процессу, сбоя, завершения или готовности | PID и сигнал — полезная нагрузка, а не доказательство причины |
| `1_029_001 pg_log_gap` | `EventObservation::Gap` и явное покрытие с потерями | Сохраняются пропущенные байты, число отброшенных строк и доказанный нижний предел |
| `1_030_001 pg_log_temp_files` | Отдельное наблюдение и факт временного файла | Размер в байтах; сущность или сброс без доказательства не создаются |

Разрешённый список метрик материализует:

- `pg_stat_database` типов `1_005_001..=1_005_004`;
- состояние экземпляра и сбросов `1_015_001`, `1_020_001`, `1_021_001`;
- физическую репликацию `1_033_001` и слоты `1_034_001..=1_034_003`;
- файловые системы PostgreSQL `1_036_001..=1_036_002`;
- память процесса в cgroup `1_037_001`;
- `vmstat` `1_106_001` и память cgroup `1_202_001`;
- покрытие сбора `1_023_001` и `1_038_001`.

Для этих типов извлечение создаёт доказанные описания рядов, отсчёты
счётчиков и измерений, маркеры сбросов, состояния сущностей, покрытие факторов
и связанные факты событий. Тип, единица, сущность, семейство сброса и качество
покрытия задаются разрешённым списком, а не выводятся из имени столбца. Новая
зарегистрированная раскладка вне списка завершается `UnsupportedLayout`; это
влияет на покрытие источника и не превращается в нулевое измерение.

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

Текущий `NotablePolicy::v1` классифицирует сохранённые `EventObservation` и
доказанные канонические факты метрик или состояний, но не выполняет причинную
корреляцию. Его стабильные коды внешнего протокола:

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
- `oom_kill_observation`;
- `filesystem_capacity_zero`.

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

## 9. Логическое содержимое фактов сегмента

### 9.1 Обязательные канонические блоки

Каждый OVF содержит следующие виды блоков. Блок может быть естественно пустым,
если выбранный PGM не содержит соответствующих строк, но обязательный базовый
блок не может отсутствовать:

| Блок | Содержимое | Реализация |
| --- | --- | --- |
| `SOURCE_MANIFEST` | Перечень записей каталога, раскладка и схема PGM, поддерживаемые и неподдерживаемые секции, происхождение содержимого, источник и диапазон | Заполняется для каждой записи каталога |
| `EVENT_OBSERVATIONS` | Сохранённые наблюдения в форме источника, упорядоченные по `(sort_ts_us, observation_id)` | Заполняется для восьми разрешённых раскладок журнала из §7.6 |
| `EVENT_FACTS` | Нормализованные факты, не зависящие от политики, и ссылки на подтверждающие наблюдения | Заполняется фактами журнала, счётчиков и переходов состояний |
| `LOSS_COVERAGE` | Наличие секций, интервалы покрытия, `pg_log_gap`, ограничения и потери, полнота состава, качество хвоста и источника | Заполняется покрытием каталога и сборщиков, известными пропусками и доказанными нижними пределами |
| `GAUGE_SAMPLES` | Значения с временными метками, идентификаторы фактора, ряда и сущности, единицы, качество и эпоха покрытия | Заполняется разрешёнными измерениями PostgreSQL, ОС и cgroup |
| `COUNTER_SAMPLES` | Накопительные значения с временными метками, ряд и сущность, семейство сброса и эпоха | Заполняется разрешёнными накопительными метриками |
| `RESET_MARKERS` | Границы сбросов по семействам, перезапуска postmaster и источника | Заполняется доказанным контекстом PostgreSQL и ОС |
| `ENTITY_STATES` | Полные ограниченные снимки сущностей, размер состава и состояние для доказанных переходов | Заполняется для репликации, слотов и других поддерживаемых составов |
| `STRING_TABLE` | Ограниченные канонические строки UTF-8 или байты для нормализованных шаблонов и других явно сохранённых ссылок | Заполняется общими строками наблюдений и фактов; может быть естественно пустым |

Контейнер создаёт девять записей каталога блоков. Число записей само по себе не
доказывает наличие данных: естественно пустой блок имеет нулевые число
элементов и тело и остаётся отличим от отсутствующего обязательного блока.

Формат допускает будущее разбиение по виду, логическому идентификатору фактора
или источника и временному диапазону, чтобы запрос декодировал только
пересечение и соседние граничные отсчёты. Текущий писатель PGKOVF создаёт один
базовый блок каждого вида и отвергает дубликат вида; несколько блоков одного
вида требуют явного расширения формата и схемы.

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

## 10. Физический формат фактов сегмента

### 10.1 Размещение и `FactKey`

`KRONIKA_WEB_DIR` — один каталог, который целиком принадлежит PgKronika.
Активный журнал, запечатанный PGM и его производный OVF находятся рядом:

```text
<KRONIKA_WEB_DIR>/active.parts
<KRONIKA_WEB_DIR>/N.pgm
<KRONIKA_WEB_DIR>/N.ovf
```

`N.ovf` имеет в точности ту же основную часть имени, что и `N.pgm`.
`SegmentContext` принимает только имя непосредственного дочернего файла с
непустой основной частью и точным расширением `.pgm`; разделители пути и
нулевой байт запрещены. Дополнительного каталога, идентификатора хранилища,
хеша пути или производного имени нет.

```text
FactKey = SHA-256(
  "pgk-overview-fact-key-v1" ||
  pgm_source_id ||
  source_descriptor ||
  file_kind ||
  fact_schema_version ||
  extractor_semantics_version ||
  registry_contract_version
)
```

Полный неизменяемый идентификатор задачи построения:

```text
FactBuildKey = (FactKey, SegmentLineageId)
```

`FactKey` связывает содержимое источника и версии контрактов.
`SegmentLineageId` различает сохранённые появления одинакового содержимого.
`FactBuildKey` используется для координации одной задачи построения, допуска
ресурсов, резервного хранения в памяти и декодированных записей. Он не
участвует в имени `N.ovf` и не создаёт отдельную блокировку для каждого ключа.

Два сегмента с одинаковым `FactKey`, но разным `SegmentLineageId` не
объединяют задачи построения или записи в памяти. Версии правил состояния,
краткого списка и HTTP-ответа в `FactKey` не входят.

### 10.2 Дескриптор PGM и происхождение

Дескриптор содержимого v1:

```text
source_descriptor = SHA-256(
  "pgk-pgm-catalog-descriptor-v1" ||
  source_file_len_le ||
  exact_tail_index_bytes ||
  exact_raw_catalog_block_bytes
)
```

Исходный каталог содержит идентификатор источника, диапазон, формат, а для
каждой секции — тип, смещение, длину, число строк и CRC32C. Дескриптор тем
самым связан с содержимым PGM в пределах модели целостности каталога и
обнаруживает обычную замену или повреждение без чтения тел секций.

Модель угроз v1 предполагает, что PGM и OVF принадлежат одному доверенному
пользователю операционной системы, а PGM после публикации неизменяем. CRC32C
защищает от случайного повреждения, но не от злоумышленника с правом записи.
SHA-256 над каталогом с CRC не превращает CRC в криптографическую проверку
подлинности тела.

Проверка после перезапуска требует нулевого числа чтений тел PGM. Поэтому
незаметное изменение бита в теле при неизменном каталоге нельзя одновременно
обнаруживать при каждом чтении OVF. Его обнаруживает ограниченная фоновая
проверка исходных секций или последующее прямое чтение. После ошибки такой
проверки сегмент помечается как повреждённый источник, и старый OVF больше не
маскирует пропуск.

### 10.3 Фиксированный заголовок v2

Все поля сериализуются по отдельности в порядке little-endian. Использовать
`repr(C)` Rust или размещение полей структуры в памяти нельзя.

Заголовок v2 занимает ровно 192 байта:

| Offset | Поле | Тип | Контракт |
| ---: | --- | --- | --- |
| 0 | `magic` | `[u8;8]` | `b"PGKOVF\0\0"` |
| 8 | `container_version` | `u16` | `2` |
| 10 | `header_len` | `u16` | `192` |
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
| 64 | `source_descriptor` | `[u8;32]` | Дескриптор содержимого PGM |
| 96 | `fact_key` | `[u8;32]` | Проверяемый `FactKey` |
| 128 | `segment_lineage_id` | `[u8;32]` | Проверяемое происхождение сегмента |
| 160 | `directory_offset` | `u64` | v2: `192` |
| 168 | `directory_count` | `u32` | `1..=MAX_DIRECTORY_ENTRIES` |
| 172 | `directory_entry_len` | `u16` | v2: `64` |
| 174 | `descriptor_kind` | `u16` | Дескриптор каталога `1` |
| 176 | `file_len` | `u64` | Точная длина файла фактов |
| 184 | `directory_crc32c` | `u32` | CRC точных байтов каталога блоков |
| 188 | `header_crc32c` | `u32` | CRC заголовка с обнулённым полем CRC |

Неизвестные magic, версия, вид файла, флаги или вид дескриптора делают файл
несовместимым. Условие `source_min_ts_us <= source_max_ts_us` обязательно.
Числовой идентификатор источника, дескриптор, `FactKey`, происхождение, версии,
диапазон и длина PGM сверяются с выбранным источником до допуска блоков.

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

Порядок допуска:

1. Проверить безопасное имя непосредственного дочернего `N.pgm` и вывести из
   него только соседнее имя `N.ovf`.
2. Открыть каталог данных, PGM и OVF относительно дескриптора каталога с
   `NOFOLLOW`; принимать только обычные файлы.
3. Проверить длину OVF и прочитать 192-байтовый заголовок; проверить magic,
   версии, вид, флаги и CRC заголовка.
4. Проверяемой арифметикой подтвердить смещение, число и размер записей
   каталога блоков, а также точный диапазон каталога.
5. Прочитать ограниченный каталог блоков и проверить его CRC.
6. Сравнить ожидаемые числовой идентификатор источника, дескриптор PGM,
   `FactKey`, `SegmentLineageId`, диапазон, формат и длину источника.
7. Проверить канонический порядок, известные флаги, нулевые зарезервированные
   поля, временные границы, непересекающиеся диапазоны блоков и точный
   `file_len`.
8. Выбрать только нужные блоки; до выделения памяти проверить сохранённую и
   декодированную длину и число элементов.
9. Проверить CRC блока и равенство длин для `None`. Для любого будущего кодека
   сначала проверить предел декодированной длины и точную длину результата,
   затем логические инварианты декодера.
10. Проверить упорядоченность и уникальность ключей, диапазоны перечислений,
    конечность чисел с плавающей точкой, переполнение счётчиков, ссылки и
    полное потребление блока.

Ошибка в выбранном блоке отвергает весь OVF. Частично использовать исправные
блоки повреждённого файла нельзя: однозначным запасным путём остаётся
перестроение из PGM.

### 10.7 Durable publication

1. Получить исключительное право записи в каталог через
   `.pgkronika-overview.owner.lock` и локальный шлюз публикации.
2. Повторно открыть и проверить соседний `N.pgm` с `NOFOLLOW`.
3. Создать уникальный временный файл
   `.pgkronika-overview.tmp-<pid>-<sequence>` в том же каталоге с
   `CREATE|EXCL|NOFOLLOW` и правами `0600`.
4. Записать полностью сформированный контейнер и вызвать `sync_all(file)`.
5. Повторно проверить временный файл тем же путём допуска и с ожидаемыми
   заголовочными данными.
6. Атомарно переименовать временный файл поверх того же `N.ovf` и вызвать
   `sync_all` для каталога.
7. Удалить только собственный временный файл при ошибке или после завершения.

Допустимый существующий `N.ovf` переиспользуется. Устаревший, несовместимый,
повреждённый или не соответствующий выбранному PGM файл безопасно заменяется
атомарным переименованием по тому же пути. Ошибка публикации после успешного
построения не отменяет вычисленный ответ.

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
- Несовпадение версий фактов, извлечения или реестра не исправляется внутри
  старого содержимого: соседний OVF отвергается, факты строятся из PGM и
  атомарно заменяют тот же файл.
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

`Missing`, `Incompatible`, `Corrupt`, `WrongSource` и ошибка ввода-вывода OVF
являются устранимыми ошибками производного файла. `SourceMissing`, `SourceIo`,
`SourceCorrupt` и `UnsupportedLayout` влияют на покрытие результата и не
переименовываются в отсутствие OVF.

Текущая реализация сначала проверяет декодированные факты в памяти и допустимый
соседний OVF. Только затем она входит в ограниченный планировщик построения.
Одинаковые `FactBuildKey` объединяются одной безопасной при отмене задачей, а
разные задачи ограничены общей взвешенной ёмкостью, числом работников,
очередью, параллелизмом одного запроса и временем ожидания.

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

1. Считыватель строит авторитетный каталог запечатанных данных из заголовков и
   каталогов PGM.
2. Для каждого непосредственного дочернего `N.pgm` рассматривается только
   соседний `N.ovf`; каталог данных не становится источником диапазонов.
3. Заголовки и каталоги OVF проверяются лениво, а тела блоков читаются по
   необходимости.
4. Допустимый OVF обслуживает запрос после перезапуска без чтения тела PGM.
5. Активный журнал получает новое доказанное поколение и входит в `Warming`.
6. Завершённые кадры сворачиваются ровно один раз. До состояния `Current`
   ответы показывают прогрев и состояние хвоста либо используют допущенное
   прямое сворачивание.
7. Хранилища фактов, проекций и ответов в памяти начинают работу пустыми.

### 12.5 Corruption и schema change

- Torn active tail не продвигает valid watermark и публикуется как `tail_pending`; это не corruption completed frame.
- Повреждённый или несовместимый OVF закрывается, учитывается метрикой,
  перестраивается из PGM и атомарно заменяется по тому же пути.
- Файл, заголовок которого описывает другой источник, никогда не допускается
  только из-за совпадения имени.
- Повреждённый исходный PGM создаёт пропуск или ошибку источника; старый OVF не
  становится самостоятельным источником истины после обнаружения повреждения.
- Formula/notable-only change очищает projection/response keys, но не меняет fact files/mtimes.
- Изменение версии фактов, извлечения или реестра меняет `FactKey`; старый OVF
  перестраивается и атомарно заменяется на том же соседнем пути.

### 12.6 Retention и GC

- Набор живых `FactBuildKey` строится только из последнего успешного полного
  снимка источников.
- Неопределённость при сканировании каталога запрещает удаление и не продвигает
  льготный период.
- PGM, исчезнувший из авторитетного представления, немедленно перестаёт
  участвовать в новых ответах.
- Производный OVF не продлевает срок хранения источника.
- Удаление устаревшего OVF откладывается минимум на два разных авторитетных
  поколения и на заданный временной льготный период.
- Плоское ограниченное сканирование учитывает только распознанные соседние OVF
  и собственные временные файлы публикации. Квоты по логическому размеру и
  числу файлов применяются только к этим производным объектам.
- Перед удалением повторно проверяются имя, вид файла, inode, заголовок и
  ожидаемый `FactBuildKey`; открытие выполняется с `NOFOLLOW`.
- Право изменения каталога удерживает один процесс через
  `.pgkronika-overview.owner.lock`. Остальные процессы могут читать допустимые
  OVF, но не публикуют файлы и не выполняют сборку мусора.
- Сборка мусора никогда не удаляет PGM, `active.parts` или другие файлы
  источника.

## 13. Слои чтения и резервные данные в памяти

### 13.1 Слои

```text
L0 source: immutable PGM + completed active parts
L1 disk:   соседние канонические файлы N.ovf
L1f memory: допущенные факты после ошибки публикации
L2 memory:  ограниченные по объёму декодированные блоки и проекции
L3 memory:  точные сериализованные ответы
```

L1 сохраняется после перезапуска. L1f/L2/L3 существуют только в процессе.
Ни один слой не меняет семантику корректности нижнего слоя.

`FactStore::load_or_build` соблюдает строгий порядок: чтение соседнего OVF →
поиск в резервных данных памяти → построение из PGM → попытка атомарной
публикации OVF. Резервный слой заполняется только после устранимой ошибки
публикации уже построенного и полностью проверенного набора фактов. Это
детерминированный LRU по полному `FactBuildKey`, одновременно ограниченный
каноническими байтами и суммой часов сегментов. Слишком крупная запись
обслуживает текущий запрос, но не остаётся в памяти. Декодированные факты и
точные ответы также имеют отдельные ограничения по объёму и числу записей.

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
  source_ids,
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

Значение — неизменяемое сериализованное тело вместе с типом содержимого и
метаданными состояния. Хранилище ограничено по числу байтов. Ключ ответа с
активными данными всегда включает поколение журнала и обработанную позицию;
короткий TTL не заменяет эти признаки.

### 13.4 Persistent cache modes

Возможности чтения и записи OVF отслеживаются независимо:

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

1. вычисленные `SegmentFacts` остаются в памяти и обслуживают текущий ответ;
2. новые задачи построения используют ограниченный резервный слой памяти;
3. уже допустимые OVF продолжают читаться, если чтение доступно;
4. причина учитывается отдельно от ошибок исходного PGM;
5. `ENOSPC` или превышение квоты один раз запускает ограниченную сборку мусора,
   затем повторяет запись;
6. задержка между попытками не позволяет каждому запросу повторять одну и ту
   же ошибку;
7. фоновая проба возвращает `ReadWrite` после успешного создания,
   синхронизации и удаления собственного временного файла.

Backoff v1: initial 1 s, multiplier 2, cap 5 min, jitter ±20%; для permission/read-only причин первая повторная проверка начинается с capped interval. Успех сбрасывает backoff. Эти значения operational, а не health/benchmark thresholds.

Cache persistence state виден в metrics/admin diagnostics. Он не попадает в source coverage и не делает корректный timeline partial.

Текущая реализация использует эту машину состояний записи во всех API
`FactStore`: одна готовая резервировка, очистка при отмене, одна ограниченная
попытка сборки мусора и фоновая проба из обновления веб-сервера. Чтение OVF не
зависит от задержки повторной записи. Координация построений и взвешенный
допуск реализованы отдельно и не блокируют попадания в память или OVF.

### 13.5 Quota

- Отдельные byte budgets: disk facts, memory facts/projections, exact responses, pinned cursor views и in-flight builds.
- Дисковая квота считает распознанные соседние OVF и собственные временные
  файлы публикации; PGM и `active.parts` в неё не входят.
- Временное превышение ограничено одним bounded in-flight file на writer slot.
- Eviction никогда не удаляет in-use `Arc`; file unlink безопасен только после исключения из lookup и с учётом platform semantics.
- При невозможности освободить quota system остаётся memory-only, а не обрезает facts.

Текущая реализация поддерживает необязательные пределы суммы логических
`st_size` и числа распознанных производных файлов. По умолчанию они отключены.
Допуск требует полного ограниченного сканирования и при неопределённости ничего
не удаляет. Это не квота физической файловой системы; рабочий набор «плотного
часа» проверяется отдельным квалификационным артефактом.

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

Текущая реализация содержит отдельные координаторы для точного HTTP
`ResponseKey` и для `FactBuildKey`. Одинаковые задачи построения получают один
общий результат. Разные `SegmentLineageId` не объединяются, отмена ожидающего
запроса не отменяет общую задачу, а завершение или ошибка освобождает запись.

### 14.2 Global cold-work bounds

Координация одной задачи не защищает от запроса на сотни разных сегментов,
которые нужно построить из PGM. Поэтому реализация одновременно ограничивает:

- weighted global budget по estimated PGM bytes, decoded bytes и CPU work units;
- hard max concurrently building keys;
- bounded blocking worker pool;
- per-request parallelism;
- max in-flight FD/read/write bytes;
- число одновременных публикаций OVF;
- fair queue между requests/sources;
- admission timeout и `Retry-After` для overload;
- max range/segments/points/page до materialization.

Попадания в декодированные данные памяти, OVF и точные ответы не занимают
разрешения на построение из PGM. Короткое изменение метаданных LRU допустимо;
полезная нагрузка возвращается как `Arc` без копирования.

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

Рабочие маршруты `/v1/timeline/overview`, `/v1/timeline/health` и
`/v1/timeline/events` описаны в OpenAPI. Параметры обзора —
`source/from/to`; состояния — `source/from/to/step`; событий — повторяемый
`source` и `from/to/limit/cursor/min_severity/kind`. Параметры `profile` и
`entity` в псевдокоде ниже остаются будущими расширениями. Фактическая внешняя
форма проверяется OpenAPI и наборами данных обработчиков.

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

`source_set_id` хеширует упорядоченный набор выбранных числовых
идентификаторов источников. `query_hash` включает диапазон, нормализованные
фильтры, порядок, версию правил краткого списка и схему ответа. Первая страница
закрепляет неизменяемое представление запроса и индекса. Следующая страница
обязана использовать то же поколение и тот же хеш запроса.

`min_severity` применяется только к observations с severity. Typed lifecycle/state facts без severity остаются eligible по `kind` и NotablePolicy; caller, которому нужны только log error groups, задаёт соответствующий `kind` filter.

Pinned view registry bounded по count, bytes и TTL. Она удерживает нужные `Arc` fact/event views, поэтому GC/refresh не меняют уже начатую pagination. Если view не помещается в budget, первая страница возвращает `cursor_view_limit` до обещания stable pagination.

Cursor errors:

- `invalid_cursor` — decode/MAC/version;
- `cursor_query_mismatch` — изменён range/filter/policy;
- `cursor_expired` — TTL/count eviction;
- `view_gone` — source/view больше нельзя удержать;
- HTTP status для expired/gone — `410 Gone`; invalid/mismatch — `400 Bad Request`.

После process restart in-memory pinned views исчезают, поэтому старые cursors честно expire. Stateless continuation на новом live generation запрещено.

### 15.6 Ошибки источника, OVF и перегрузка

- Повреждение OVF или ошибка его записи не включается в потери источника; это
  диагностические сведения об ускоряющем слое.
- Unreadable/corrupt source segment становится explicit segment/source gap. Если другие данные позволяют корректный partial response, endpoint возвращает `200` с `source_status=Partial/Gap`.
- Если authoritative store view вообще недоступен, возвращается `503 source_unavailable`.
- Request shape выше hard limit — `400 query_limit_exceeded` или `413` для oversized body, без partial work.
- Истечение ожидания допуска построения — `503 cold_build_overloaded` с
  `Retry-After`; попадание в память или допустимый OVF не должно приводить к
  такому ответу.

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
- числовой идентификатор источника, дескрипторы сегмента, части и тела, а также
  происхождение;
- semantic `RefreshDelta`;
- disk header/directory/block codec;
- допуск OVF, атомарная публикация и типизированные ошибки чтения и записи;
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

Текущая реализация атомарно публикует пару снимка источников и временной шкалы,
обслуживает конечные точки, закрепляет представления курсоров и ограничивает
точные ответы по объёму. После M4 добавлены полный `FactBuildKey`, резервный
слой памяти с двумя пределами, единственный владелец каталога, необязательные
пределы размера и числа производных файлов, безопасная сборка мусора,
типизированное восстановление записи, координация построений, взвешенный допуск
и фоновая проверка исходных секций.

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

Текущий эталон прямого чтения сравнивает все заполненные блоки событий,
отсчётов, сбросов, состояний и покрытия. Проверки охватывают активный и
запечатанный пути, повторное чтение OVF после перезапуска, границы диапазонов,
идентификаторы, единицы и потери из §7.6. Итоговый M6 подтверждается только
артефактом и CI по правилам §20.

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
- неверные `source_descriptor`, `FactKey`, `SegmentLineageId`, диапазон,
  длина или `source_id`;
- состязание публикаций, замена устаревшего OVF и очистка собственных временных
  файлов;
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
- два источника с различными `source_id`;
- corrupt fact block;
- corrupt PGM section;
- dense one-hour working set;
- mixed 5/10/30/60/3600-second source cadences.

### 18.2 Режимы

1. `derived-cold`: новый каталог данных с PGM без соседнего OVF и пустое
   состояние процесса.
2. `restart-warm`: новый процесс, допустимый соседний OVF и пустые хранилища
   памяти.
3. `process-hot`: второй и последующие одинаковые запросы.
4. `range-cold/facts-warm`: новый диапазон, шаг или фильтр при наличии фактов.
5. `live`: sealed facts + active parts + pending tail.
6. `concurrent-identical`: 16 simultaneous cold misses одного fact set.
7. `concurrent-disjoint`: 16 simultaneous different cold ranges.
8. `memory-only`: ошибка публикации OVF; факты остаются в памяти с ограничением
   по объёму.
9. `oracle-profile`: зафиксированные исходные данные и файлы фактов, точная
   мощность набора и один версионированный профиль узла и файловой системы.

Новое состояние процесса и холодный накопитель или страничный кэш называются
отдельно. Нельзя выдавать прогретый страничный кэш ОС за холодный накопитель.

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
11. Fixed metric fact component измеряется как exact encoded bytes с относимой долей header/directory и allocation overhead на retained sample. Variable event/string bytes выводятся отдельно. Если владелец задал disk и dense-hour budgets, qualification блокируется при их превышении. Пока значения остаются owner-deferred по §23, artifact записывает точные размеры и статус `owner_deferred` без deployment verdict; это не отменяет structural/I/O/performance gates. Универсальный bytes/sample claim без artifact запрещён.
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

Структурированные журналы включают идентификатор запроса, числовой
`source_id`, сокращённые `FactKey` и `SegmentLineageId`, поколение
представления и цепочку ошибок. Полные пути, исходные шаблоны и текст
пользователя или базы данных не записываются без явного режима диагностики и
правил сокрытия данных.

## 20. Критерии приёмки parity v1

### 20.1 Матрица приёмки и состояние реализации

Все восемнадцать требований реализованы и имеют локальные проверки-кандидаты.
Итоговый `PASS` присваивается всему набору только после успешной проверки
структуры квалификационного артефакта и всех связанных заданий в одной попытке
GitHub Actions на одном точном коммите.

| ID | Обязательное требование | Реализация и проверка-кандидат |
| ---: | --- | --- |
| 1 | После перезапуска допустимый соседний OVF обслуживает внутреннюю часть запечатанного диапазона без чтения тел PGM | `restart-warm` и счётчики ввода-вывода требуют ноль чтений тел и ноль записей |
| 2 | Прямое чтение и индекс дают одинаковый результат для полных и частичных диапазонов, одного и нескольких сегментов, запечатанных и активных данных | Проверки всех семейств фактов сравнивают события, метрики, состояния, покрытие и границы диапазонов с исходным эталоном |
| 3 | Случайное разбиение и запечатывание не создают повторов, потерь или смещения границ | Метафорфные и свойственные проверки охватывают происхождение, объединение, граничные отсчёты и переход активных данных |
| 4 | Отсутствующий, повреждённый, несовместимый, слишком большой или чужой OVF перестраивается; ошибка публикации не ломает корректный ответ | Набор проверок контейнера и публикации подтверждает перестроение, атомарную замену и ограниченный резервный слой памяти |
| 5 | Обнаруженное повреждение PGM остаётся видимым пропуском или ошибкой источника и не скрывается старым OVF | Фоновая потоковая проверка CRC исключает повреждённый источник из допустимого набора и не использует его OVF |
| 6 | Изменение формулы состояния или правил краткого списка не перестраивает факты, пока сохранённых измерений достаточно | Эти версии не входят в `FactKey`; проверки версий подтверждают разделение фактов и представления |
| 7 | Каждый сохранённый элемент краткого списка встречается при проходе курсором ровно один раз; групповой счётчик и потери сохраняются | Проверки курсора используют закреплённое представление и канонический порядок событий |
| 8 | Идентификаторы событий сохраняются при обычном переходе активных данных в запечатанные; ограничения идентификатора по содержимому опубликованы | Проверки перехода и разных `SegmentLineageId` не используют путь, имя файла или отдельный идентификатор хранилища |
| 9 | Канонический построитель активных данных не теряет факты; превышение жёсткого предела даёт `Incomplete` и запрещает продвижение | Проверки машины состояний активных данных и продвижения подтверждают оба пути |
| 10 | Пропуск обязательного домена всегда даёт числовое `None`, а не ложное зелёное значение | Наборы данных веб-API и свойства аналитического ядра проверяют неизвестный результат |
| 11 | Доверенный нижний предел сохраняется при разбиении, запечатывании и худшем прореживании; неизвестное значение остаётся неизвестным | Свойства состояния и прореживания проверяют сохранение подтверждений |
| 12 | Для каждого фактора опубликованы покрытие, применимость и потери; один отображаемый коэффициент не служит критерием корректности | Проверки API охватывают все реализованные семейства и причины потерь |
| 13 | Скорости счётчиков используют фактический соседний интервал, семейства сбросов и пропуски; граница файла не меняет результат | Проверка граничных отсчётов охватывает сбросы, пропуски и произвольный диапазон |
| 14 | Каждое семейство имеет проверяемый путь от источника к факту, отсчёту или состоянию с единицами, сбросом, покрытием и потерями | Проверки извлечения охватывают реестр событий и метрик и отвергают неподдерживаемую раскладку явно |
| 15 | Попадания в память и OVF обходят допуск построения; одинаковые `FactBuildKey` объединяются, разные задачи глобально ограничены | Проверки веб-допуска охватывают очередь, взвешенную ёмкость, ожидание, отмену и независимые ключи |
| 16 | Резервный слой памяти ограничен байтами и часами сегментов; повторные попытки наблюдаемы; размер «плотного часа» измерен | Режим `memory-only`, точный учёт памяти и машина восстановления записи входят в квалификационный артефакт |
| 17 | Квота и сборка мусора учитывают производные файлы, безопасны при нескольких читателях и никогда не удаляют источник | Плоское сканирование, единственный владелец, льготный период, повторная проверка inode и сохранение PGM/`active.parts` покрыты набором GC |
| 18 | Все девять режимов §18 воспроизводимо выполняются на зафиксированном профиле узла и файловой системы | Средство `overview_qualification` записывает режимы, размеры, ввод-вывод, задержки, профиль хранения и ссылки на проверки |

### 20.2 Доказательство приёмки

Для каждой строки §20.1 итоговый пакет обязан связать требование с именем
проверки или измерения, схемой набора данных, точным коммитом Git, запуском,
попыткой и заданиями CI, контрольной суммой артефакта, исходным результатом и
решением `PASS` или `FAIL`. Смешивание разных запусков, результат из рабочего
дерева с незакоммиченными изменениями или несовпадающий коммит блокируют
приёмку.

M6 presentation evidence применяется к существующим production handlers и OpenAPI/JSON fixtures. Оно обязано проверить `score=null`/unknown при required gap, сохранение trusted floor, явные loss/partial/applicability, отсутствие interpolation и locale-neutral stable machine fields. Это API/presenter acceptance, не утверждение о render coverage несуществующего UI.

## 21. Вехи реализации `parity-v1`

Все этапы входят в `parity-v1`. Постоянные соседние OVF обязательны и не
переносятся в будущую продуктовую веху.

### 21.1 Основные этапы

| Этап | Изменение | Результат |
| --- | --- | --- |
| M0–M4 | PR #97–#103 | Наблюдения и покрытие, выборочное чтение PGM, контейнер PGKOVF, извлечение событий, активные данные, атомарные представления, API временной шкалы, курсоры и точные ответы в памяти |
| Завершение данных | PR #114, `de70586a`–`5454381e` | Канонические события, счётчики, измерения, сбросы и состояния; извлечение метрик; граничные отсчёты; запросы по всем поддерживаемым семействам |
| Устойчивость M5 | PR #114, `4da60055`–`8521384a` | Координация по `FactBuildKey`, взвешенный допуск, очередь и работники, типизированная перегрузка, фоновая проверка источника и полное покрытие API |
| Проверка M6 | PR #114, начиная с `5610686c` | Исходный эталон для всех семейств, квалификационный артефакт, валидатор, девять режимов и строгие локальные и CI-проверки |
| Соседнее хранение | PR #114, начиная с `ad57c6dd` | Один каталог `KRONIKA_WEB_DIR`, пары `N.pgm`/`N.ovf`, заголовок v2, атомарная замена, безопасные операции `NOFOLLOW`, единственный владелец и плоская сборка мусора |

Точный итоговый коммит PR #114 указывается квалификационным артефактом и
запуском CI. Промежуточный коммит или смешанные результаты доказательством
приёмки не являются.

### 21.2 Завершение канонических данных

PR #114 заполняет зарезервированные блоки только при доказанном соответствии
источника:

1. `EVENT_FACTS` хранит нейтральные к политике факты с идентификаторами
   подтверждающих наблюдений, групповыми счётчиками, качеством подтверждений и
   происхождением. `NotablePolicy` остаётся проекцией, а
   `IncidentDiagnosis` не сохраняется.
2. `COUNTER_SAMPLES` и `RESET_MARKERS` создаются только для секций PGM с
   доказанными единицами, идентификаторами рядов и сущностей, семейством сброса,
   покрытием и потерями.
3. `GAUGE_SAMPLES`, `ENTITY_STATES` и покрытие факторов строятся из
   естественных отсчётов с временными метками и полных ограниченных составов.
   Готовые точки состояния и интервалы графиков не сохраняются.
4. Неизвестные единицы, сброс, сущность или покрытие дают явный пропуск или
   `UnsupportedLayout`, а не предполагаемое нулевое значение.

### 21.3 M5. Устойчивость и допуск

M5 реализован отдельными проверяемыми частями:

1. Безопасная при отмене координация построения по `FactBuildKey`, включая
   разделение разных `SegmentLineageId`, отмену ожидающего запроса и очистку
   после любого результата.
2. Справедливый взвешенный допуск по байтам PGM, декодированным байтам, строкам
   для обработки, файловым дескрипторам, чтению, записи и публикации; число
   работников, очередь, параллелизм запроса и ожидание ограничены.
3. Состояния записи и задержка повторных попыток для `EROFS`, `EACCES`,
   `ENOSPC`, квоты и временных ошибок ввода-вывода; допускаются одна попытка
   сборки мусора и одна фоновая проба.
4. Квота и безопасная для срока хранения сборка мусора по полному
   авторитетному снимку, двум поколениям и временному льготному периоду.
   Изменять каталог может только владелец; PGM и `active.parts` не удаляются.
5. Ограниченная потоковая проверка CRC исходных секций, после которой
   повреждённый источник не может быть скрыт старым OVF.
6. Метрики §19, точный учёт рабочего набора «плотного часа» и типизированная
   диагностика перегрузки и публикации.

На всех путях сохраняются первоочередное чтение OVF, резервный слой памяти с
двумя пределами, атомарная публикация OVF и атомарная публикация представления.

### 21.4 M6. Итоговая проверка

M6 состоит из отдельных обязательных проверок:

1. Полный эталон прямого чтения, метаморфные проверки разбиения,
   запечатывания и активных данных, а также проверки повреждений и допуска для
   всех реализованных канонических семейств.
2. Версионированный набор «плотный час» и точное измерение декодированного,
   размещённого и закреплённого объёма. Утверждённые пределы развёртывания
   обязательны; до решения владельца артефакт сохраняет `owner_deferred` без
   вывода о допустимости размера.
3. Девять режимов §18 на одном точном профиле узла и файловой системы.
4. Проверки рабочего API и его JSON по §20.2. Проверки отрисовки появляются
   только вместе с отдельно утверждённым контрактом пользовательского
   интерфейса.
5. Один машиночитаемый артефакт, который связывает все восемнадцать строк
   §20.1 с одной попыткой CI и одним точным коммитом.

Графики не входят в завершение данных, M5 или M6. Возврат к ним требует
отдельного решения владельца, точного перечня рядов, соответствия источникам,
реально поддерживаемого кодека и измеренного изменения размера.

## 22. Явные нецели и отклонённые варианты

### 22.1 Нецели v1

- Изменение средства записи, PGM или контрактов источников сборщиков.
- Точная история каждой строки журнала, если её нет в PGM.
- Согласованность производных данных между узлами.
- Криптографическая подлинность при недоверенном процессе записи без
  отдельного требования безопасности.
- Использование OVF как архива после удаления PGM.
- Неограниченный предварительный прогрев всего срока хранения.
- SSE до стабилизации представлений и курсоров.
- Новый крейт только для фактов обзора.
- Сохранение `IncidentDiagnosis` как факта источника.
- Универсальная вероятность «здоровья» без калибровочного набора и исхода.

### 22.2 Отклонённые физические варианты

**Принадлежащие средству записи `.heatmap` или `.charts`.** Такой вариант
замораживает семантику скоростей и состояния, меняет владельца PGM и требует
перестраивать данные при изменении формулы.

**Глобальный дописываемый производный индекс.** Для v1 он создаёт второй журнал
предзаписи с кадрами, блокировками, надгробиями, уплотнением и широкой областью
последствий повреждения.

**Гибридные объекты с авторитетным манифестом.** Авторитетный каталог
сегментов и диапазонов уже даёт считывателю снимок, а манифест не устраняет
выборочное чтение нескольких полезных нагрузок. Восстанавливаемая подсказка
допустима позже, если измерения докажут узкое место при запуске, сканировании
каталога или сборке мусора либо появится второй потребитель.

**Только LRU в памяти.** Не обеспечивает быстрый путь после перезапуска и для
многодневного диапазона.

**Только постоянные точные ответы.** Не заменяют факты: новый диапазон, шаг или
фильтр снова потребует чтения PGM, а правила признания ответа устаревшим станут
комбинаторными.

**Сводка событий на весь сегмент.** Недостаточна для произвольного частичного
диапазона, скоростей с учётом сбросов и стабильной постраничной выдачи.

**Заранее вычисленные `HealthPoint` или краткий список.** Изменение правил
потребует перестроения, а объединение готовых штрафов и оценок нарушит
инвариантность разбиения.

**Краткий список активных данных с потерями.** Несовместим с авторитетной
сохранённой выдачей `/events` и продвижением после запечатывания.

**Интерполяция пропусков.** Запрещена для состояния и непрерывности счётчиков:
она изображает данные, которых источник не наблюдал.

## 23. Оставшиеся продуктовые решения

Следующие решения уже закрыты:

- `KRONIKA_WEB_DIR` — один каталог, который целиком принадлежит PgKronika;
  `active.parts`, `N.pgm` и `N.ovf` находятся рядом, а PGM и OVF имеют
  одинаковую основную часть имени;
- дополнительного корня, уровня каталогов, идентификатора хранилища, хеша пути
  и производного имени файла нет; `FactKey`, происхождение и версии контрактов
  остаются проверяемыми данными заголовка;
- графики отложены владельцем и не входят в `parity-v1`; их стоимость не
  измерена;
- пока рабочего пользовательского интерфейса нет, M6 проверяет API и его JSON,
  но не заявляет покрытие отрисовки.

Открыты четыре калибруемых продуктовых решения:

1. Конкретные кривые факторов, обязательные профили и пороги состояния после
   выбора измеримого исхода и калибровочных наборов.
2. Пределы диска, памяти, файловых дескрипторов, очереди построения и времени
   жизни курсора для конкретного развёртывания в пределах абсолютных
   ограничений безопасности.
3. Описания обслуживания и топологии, которые позволят подавлять плановое
   завершение и определять обязательных участников репликации.
4. Объём, отрисовка и локализация будущего рабочего пользовательского
   интерфейса, если владелец решит его добавить.

Отсутствие калибруемых значений не ослабляет обязательные проверки данных,
структуры, ввода-вывода и производительности и не означает одобрения размера
для развёртывания. Заданные значения становятся обязательными проверками
развёртывания. Их изменение версионирует правила или конфигурацию, но не меняет
PGM, физическое соответствие PGM/OVF или инварианты честности данных.
