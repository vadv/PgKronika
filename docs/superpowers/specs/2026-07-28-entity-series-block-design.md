# OVF: пер-сущностные ряды для web

Дата: 2026-07-28. Документ определяет текущий формат OVF для данных,
которые нужны интерфейсу `pgkronika-ui-proposal-v5.html`.

Документ является единственным контрактом формата. Reader и writer
реализуются одновременно по этой спецификации. Номера revision ниже
нужны только для будущих независимых расширений блоков, view и метрик.

## Цель

OVF должен закрывать горячие запросы интерфейса на интервале до суток:

- heatmap выбранного view и переключение его метрик без чтения PGM;
- спарклайны строк без повторного декодирования запечатанных данных;
- точные времена соседних снимков и счётчики вкладок без чтения PGM;
- точный `frame` из одного сегмента PGM, на границе — не более двух;
- точный detail по явному действию пользователя.

Приоритеты в порядке убывания:

1. минимальное время ответа на суточном диапазоне;
2. жёстко ограниченная память writer и reader;
3. минимальный объём прочитанных и хранимых байтов;
4. независимое добавление view и метрик.

## Инварианты

- Запрос timeline не декодирует PGM.
- Directory позволяет прочитать только блок выбранного view.
- Ноль, отсутствие сущности, отсутствие снимка, gate и ресурсный
  предел являются разными состояниями.
- Локальный top-K сегмента точен. Top диапазона может быть
  приближённым, но ответ содержит вычисляемую верхнюю границу ошибки.
- Ни одна структура не растёт только от размера входа: у карты,
  словаря, блока, декомпрессии, кеша и параллелизма есть byte-cap.
- Физическая revision блока, revision view и revision метрики
  независимы. Добавление метрики не инвалидирует другие метрики.
- Directory хранит фактический диапазон наблюдений. Внутренняя сетка
  может выходить за него до границ bucket, но не объявляется
  диапазоном блока.

## Физическая раскладка

В directory используются составные адреса `(block_kind, logical_id)`.
Для одного `block_kind` разрешено несколько записей с разными
`logical_id`.

| Block kind | `logical_id` | Назначение |
| --- | --- | --- |
| `UiSummary` | `0` | времена снимков, population и статус всех view |
| `EntitySeries` | стабильный `view_code` | метрики и top-K только одного view |

Один большой блок для всех view запрещён: при выбранной вкладке он
создаёт overfetch остальных метрик. Блок на каждую метрику также
запрещён: он повторяет identity и подписи сущностей. Блок на view
является минимальной единицей, которая сохраняет локальность и
дедупликацию ключей при мгновенном переключении метрики.

Directory entry содержит:

```text
block_kind       u32
block_revision   u16
codec            u8       # none | zstd
flags            u16      # bit 0: has_time_range
logical_id       u32
offset           u64
stored_len       u64
decoded_len      u64
item_count       u32
crc32c           u32      # от stored bytes
min_ts_us        i64      # фактический первый снимок блока
max_ts_us        i64      # фактический последний снимок блока
```

При снятом `has_time_range` оба timestamp равны нулю. При
установленном флаге `min_ts_us <= max_ts_us`, и обе границы лежат
внутри диапазона source PGM. Grid boundaries в эти поля не пишутся.

Пустой или gated view имеет запись в `UiSummary`. Его
`EntitySeries` может отсутствовать: это не `pending`, потому что
summary содержит окончательный статус.

### Кодек

Writer сначала строит каноническое тело. Для `EntitySeries`
допустимы `none` и `zstd` level 1. Zstd выбирается, только если
`stored_len + 64 < decoded_len`; иначе сохраняется исходное тело.
`UiSummary` использует `none`.

Reader до выделения памяти проверяет `decoded_len` по bound,
выделяет ровно этот объём и требует точного размера результата.
CRC проверяется до декомпрессии. Dictionary или trailing bytes,
выходящие за объявленное тело, делают OVF повреждённым.

## Сетка времени

Сетка выровнена по UTC. Базовая ширина — 60 секунд. Если диапазон
сегмента не помещается в 256 bucket, writer выбирает:

```text
bucket_width_s =
  ceil(segment_duration_s / (256 * 60)) * 60
grid_start_us =
  floor(first_snapshot_us / (bucket_width_s * 1e6))
  * bucket_width_s * 1e6
bucket_count =
  floor((last_snapshot_us - grid_start_us) / bucket_width_us) + 1
```

`bucket_count` обязан быть в диапазоне `1..=256`. Урезание хвоста
сегмента запрещено.

Все bitset кодируются младшим битом вперёд и имеют
`ceil(bucket_count / 8)` байтов. Биты за `bucket_count` равны нулю.

## Блок UiSummary

Блок мал и читается отдельно от рядов:

```text
header:
  summary_revision       u16
  grid_start_us          i64
  bucket_width_s         u32
  bucket_count           u16
  snapshot_time_count    u32
  snapshot_time_deltas   [uvarint; snapshot_time_count]
  view_count             u16

view (view_count раз):
  view_code              u16
  view_revision          u16
  status                 u8
  snapshot_presence      [u8; ceil(snapshot_time_count / 8)]
  population_count       u32
  populations            [uvarint; population_count]
  coverage_mask          [u8; ceil(bucket_count / 8)]
```

`snapshot_time_deltas` кодирует union timestamps всех view: первый
timestamp представлен неотрицательной дельтой в микросекундах от
`grid_start_us`, следующие — положительными дельтами от предыдущего.
View ссылается на общую таблицу битами `snapshot_presence`.
`population_count = popcount(snapshot_presence)`, а populations идут
в порядке установленных битов. Поэтому timestamp не повторяется для
каждого view, а population остаётся точным для каждого snapshot.

`coverage_mask` является производной компактной картой bucket:
установленные биты `snapshot_presence` проецируются на grid. Reader
проверяет это равенство и нулевые хвостовые биты обеих масок.

`status`:

| Значение | Смысл |
| --- | --- |
| `complete` | view собран полностью |
| `empty` | источник доступен, строк нет |
| `gated` | extension, privilege или ОС-возможность недоступны |
| `unsupported_type` | нет projection contract для встретившегося type |
| `resource_limited` | writer достиг жёсткого предела |

Статус окончателен для файла. `pending` означает только активный,
ещё не опубликованный хвост и не используется для запечатанного OVF.

## Блок EntitySeries

Блок содержит один view и все его heatmap-метрики. Каноническая
метрика спарклайна является обычной записью metric с флагом
`canonical`.

```text
header:
  block_revision         u16
  view_code              u16
  view_revision          u16
  identity_revision      u16
  status                 u8
  grid_start_us          i64
  bucket_width_s         u32
  bucket_count           u16
  coverage_mask          [u8; ceil(bucket_count / 8)]
  dictionary_count       u16
  metric_count           u16

entity (dictionary_count раз):
  key_len                uvarint
  key                    [u8; key_len]
  label_len              uvarint
  label_utf8             [u8; label_len]

metric (metric_count раз):
  metric_code            u16
  metric_revision        u16
  flags                  u16
  unit_code              u16
  aggregation_code       u8
  status                 u8
  series_count           u16
  cutoff_score           f64

series (series_count раз):
  entity_ref             uvarint
  exact_score            f64
  max_bucket_value       f64
  present_mask           [u8; ceil(bucket_count / 8)]
  quantized_values       [u8; popcount(present_mask)]
```

`series_count <= K`, где `K = 64`. Словарь содержит union сущностей
всех метрик view и сортируется по `key`. Ссылки указывают на
канонический порядок словаря. Метрики сортируются по `metric_code`,
серии — по убыванию `exact_score`, затем по `entity_ref`.

`metric.status` использует `complete`, `gated`, `unsupported_type` и
`resource_limited`. При статусе, отличном от `complete`,
`series_count = 0`, а `cutoff_score = 0`.

`exact_score` вычисляется declared range-ranking агрегатом метрики
по всем bucket сегмента. `cutoff_score` равен точному score сущности
на позиции K. Если сущностей меньше K, он равен нулю. Любая не
сохранённая сущность имеет score не больше `cutoff_score`.

### Presence и квантование

`coverage_mask` отвечает только на вопрос, был ли снимок view.
`present_mask` принадлежит конкретной паре `(entity, metric)`.
Покрытый bucket, в котором сущность или метрика отсутствует, не
превращается в ноль.

Для присутствующих bucket:

```text
q = 0                                      if max_bucket_value == 0
q = round(value / max_bucket_value * 255)  otherwise
value' = q / 255 * max_bucket_value
```

Значения конечны и неотрицательны. Ошибка одного восстановленного
bucket не превышает `max_bucket_value / 255`. `exact_score` не
квантуется и используется для отбора и оценки качества range top.

## Identity сущности

Identity не строится конкатенацией строк. Projection contract
объявляет ordered identity поверх нормализованного logical section.
По умолчанию это `TypeContract.identity`; отклонение допустимо только
для явно агрегированного view, например категорий событий.

Значения кодируются последовательно по объявленным типам:

```text
null        0x00
present     0x01 + canonical scalar
bool        0x00 | 0x01
unsigned    uvarint
signed      zigzag uvarint
f32/f64     raw little-endian bits
timestamp   zigzag i64 microseconds
bytes/text  byte_length uvarint + bytes
```

Имена полей в ключ не входят: порядок и типы задаёт
`identity_revision`. Null отличается от пустой строки и нуля.

Обязательные identity:

| View | Identity |
| --- | --- |
| statements | `(queryid, userid, dbid, toplevel)` после нормализации версии extension |
| plans | точная identity соответствующего registry contract |
| activity | `(pid, backend_start)` |
| tables | `(datid, relid)` |
| indexes | `(datid, indexrelid)` |
| vacuum | registry identity progress row |
| processes | `(pid, starttime)` |
| locks | registry identity lock row или явно объявленная identity агрегированного дерева |
| events | `(category)` для агрегированного heatmap; строки событий имеют отдельную identity |

`label_utf8` — только отображение и не участвует в равенстве. Он
обрезается по границе UTF-8 до 160 байтов. Rename меняет label, но не
разрывает ряд таблицы или индекса. Label обязан быть компактным
идентификатором (`queryid`, PID, relation name), а не SQL, plan или
log message; тяжёлый текст загружается detail-запросом.

HTTP entity token — base64url без padding от
`view_code || identity_revision || key`. Клиент считает token
непрозрачным.

## Семантика метрик

Каждая метрика в projection catalog фиксирует:

- входные logical sections и минимальные type revisions;
- формулу и join key;
- `counter | gauge | fraction | duration | event_count`;
- bucket aggregation и range ranking aggregation;
- единицу, gate, reset policy и `metric_revision`;
- является ли метрика канонической для spark.

Правила bucket:

- Counter: сумма доказанных неотрицательных дельт, правая точка пары
  попадает в bucket.
- Gauge: максимум, если projection не объявляет иной агрегат.
- Fraction: числитель и знаменатель агрегируются до деления.
- Duration: максимум достигнутой длительности в bucket.
- Event count: число событий в bucket.

Для первой counter-точки сегмента writer использует последнюю точку
той же identity из предыдущего сегмента. В последовательном seal
она переносится в bounded state. При независимой сборке разрешено
прочитать только последнюю snapshot-section предыдущего PGM.

Пара не создаёт значение, если predecessor отсутствует, между
точками есть доказанный gap, identity изменилась или reset нельзя
исключить. Отрицательная дельта всегда reset. Доступный reset marker
имеет приоритет над эвристикой отрицательной дельты.

## Построение top-K

Writer обрабатывает по одному view и по одной metric. Повторного
чтения PGM нет: два логических прохода выполняются по уже
декодированным и ограниченным строкам section.

1. Первый проход строит byte-accounted map
   `typed identity -> exact_score`, population и reset state.
2. После прохода выбираются точные K победителей по
   `(exact_score desc, key asc)` и фиксируется `cutoff_score`.
3. Второй проход заполняет bucket только выбранных K.
4. Серии квантуются, словарь и metric records канонизируются, затем
   выбирается codec.

Map, временные ряды и строки source buffer входят в общий отчёт
пиковой памяти. Нельзя называть алгоритм bounded, ограничив только
число элементов без учёта длины ключей.

При достижении лимита metric получает `resource_limited`; writer не
вытесняет произвольные identity и не выдаёт неточный локальный top за
точный.

## Слияние диапазона

Для метрики диапазона сервер берёт union сохранённых локальных
top-K. Projection catalog задаёт оператор объединения score между
сегментами: `sum` или `max`.

Если запрос покрывает все bucket сегмента, вклад сохранённой сущности
точно равен `exact_score`. Для частично пересечённого сегмента сервер
восстанавливает нужные bucket и строит интервал с учётом ошибки
каждого кванта. Нижняя граница bucket не бывает отрицательной.

Если `e` отсутствует в локальном top-K, её вклад в любую часть
сегмента лежит в `[0, cutoff_score]`, поскольку значения
неотрицательны. Интервалы сегментов объединяются объявленным
оператором:

```text
lower(e) = merge(selected_lower(e, segment) or 0)
upper(e) = merge(selected_upper(e, segment) or cutoff_score(segment))
unseen_upper = merge(cutoff_score(segment))
```

Следовательно, полностью невидимый кандидат не может иметь score
выше `unseen_upper`. Это консервативная, но доказуемая граница.
Сегмент со статусом, отличным от `complete`, не получает конечную
оценку и явно попадает в `quality.unbounded_segments`.

`ranking_exact = true` только если:

- все сегменты метрики complete;
- для каждой соседней пары возвращённого порядка
  `lower(i) > upper(i + 1)`;
- нижняя граница последней возвращённой строки выше upper всех
  исключённых известных кандидатов и `unseen_upper`.

Иначе порядок помечается approximate. Значения heatmap для
несохранённой пары `(segment, entity)` отсутствуют, а не равны нулю.
Точные frame и entity detail всегда читаются из PGM.

## Версионирование и расширение

- `block_revision` меняется только при несовместимом изменении wire
  layout данного block kind.
- `view_revision` меняется при изменении identity, join или набора
  обязательных inputs view.
- `metric_revision` меняется только при изменении формулы, единицы,
  reset policy или bucket/ranking aggregation этой метрики.
- Новый view получает новый `view_code`.
- Новая metric получает новый `metric_code` внутри view.

Неизвестный view или metric пропускается по длинам записей. Reader
может использовать известные метрики того же блока. Глобальной
`semantics_version`, делающей весь файл устаревшим, нет.

Исторический OVF без добавленной позже метрики остаётся корректным.
API возвращает для неё `unavailable_revision`; фоновая пересборка
может добавить данные, но не является условием чтения остальных
метрик.

## Жёсткие границы

Значения по умолчанию являются частью квалификации:

| Предел | Значение |
| --- | ---: |
| view в summary | 32 |
| union snapshot timestamps в summary | 4096 |
| метрик на view | 16 |
| top-K на metric | 64 |
| bucket на сегмент | 256 |
| identity bytes | 256 |
| label bytes | 160 |
| dictionary entries на view | 1024 |
| decoded `UiSummary` | 64 КиБ |
| decoded `EntitySeries` view | 256 КиБ |
| stored `EntitySeries` view | 128 КиБ |
| decoded source rows одного view | 64 МиБ |
| дополнительная память builder `UiSummary` | 4 МиБ |
| дополнительная память builder одного view | 32 МиБ |
| одновременно строящихся view на writer | 1 |

Oversized directory entry отвергается до чтения тела. Декомпрессия,
которая не укладывается в `decoded_len` или bound, прекращается
типизированной ошибкой. Writer при невозможности уложить корректный
результат публикует `resource_limited`, а не частичный блок.
С учётом source rows, map, рядов и codec scratch дополнительный peak
writer обязан оставаться не выше 100 МиБ. Все reservations делаются
до чтения или роста соответствующей структуры.

## Смета размера

При 15 bucket одна полная серия занимает примерно 35 байтов до
сжатия: ссылки и маски, два `f64` и 15 квантов. Верхняя оценка для
18 пар `(view, metric)` при K=64 — около 40 КиБ серий на сегмент до
словарей и заголовков. Identity обычно занимает 8–32 байта; label
ограничен 160 байтами и хранится один раз на union метрик view.

Размер нельзя экстраполировать только из 96 сегментов: ранний seal
повторяет словари и заголовки. Квалификация обязана отдельно
измерить:

- 96 сегментов по 15 минут;
- 1440 сегментов по одной минуте;
- size-seal production fixture;
- максимальный union top-K у statements.

Целевые, но не подменяющие hard bounds показатели:

- медианный stored `EntitySeries` выбранного view не больше 24 КиБ;
- все web-блоки сегмента не больше 10% PGM на production fixture;
- суточный объём web-блоков при штатном seal не больше 10 МиБ.

Если цель не выполняется, сначала сокращаются label и число
материализованных метрик. K и наличие `exact_score` не уменьшаются
без нового анализа погрешности.

## Тестирование

Обязательные golden и property tests:

- каноническое кодирование, CRC, codec и отказ от trailing bytes;
- отдельное чтение `(EntitySeries, view_code)` без других view;
- adaptive grid для 1, 15, 256 и более 256 минут без усечения;
- actual directory range при округлённой внутренней сетке;
- `missing != zero`, per-series presence и population каждого snapshot;
- nullable identity, UTF-8 label, `toplevel`, одинаковые OID в разных
  database, rename без смены key;
- predecessor на границе сегмента: normal, reset, gap, отсутствует;
- точный локальный top при churn и сущности, ставшей лидером в конце;
- range top: точный порядок, approximate порядок, невидимый кандидат
  и проверка `lower <= truth <= upper`;
- произвольные `from`/`to` внутри первого и последнего сегмента с
  учётом ошибки квантования;
- добавление metric без инвалидирования соседних metric;
- каждый memory/size bound и окончательный `resource_limited`;
- 96 и 1440 сегментов на суточном запросе;
- byte accounting кеша и oversized compressed block.

Qualification report сохраняет peak RSS writer/reader, stored и
decoded bytes по block/view, число positional reads, число
декомпрессий и погрешность range ranking.

## Вне объёма

- Точная история сущности в OVF: её читает detail из PGM.
- Точный глобальный top суток: он требует хранения всех totals или
  отдельного суточного rollup и противоречит цели минимального
  размера.
- Миграция или чтение файлов, созданных по другому контракту.
