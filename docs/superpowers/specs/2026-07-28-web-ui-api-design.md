# Web API для полного интерфейса

Дата: 2026-07-28. Спецификация закрывает данные и жесты макета
`docs/web/pgkronika-ui-proposal-v5.html`.

OVF, reader, writer и API реализуются как один текущий контракт.

**Статус: PARTIAL.**

- **Уже реализовано:** `/v1/ui/catalog`, `/v1/views/summary` и
  `/v1/timeline/heatmap` с OpenAPI и integration tests.
- **Осталось:** `/v1/ui/context`, `/v1/frame/{view}`,
  `/v1/entity/{view}/{entity}` для point/history и `/v1/storage`;
  predecessor-aware 1-2-PGM frames, server filter/sort/page counts, lazy
  detail, bounded history, whole-storage/statvfs/write-rate data; три
  byte-accounted cache с reservation/singleflight/cancellation и N=96/N=1 440
  resource qualification. Это delivery steps 4-6.

Первый отдельный vertical slice для `/v1/frame/{view}` и числовых verdicts
Класса 1 уточнён в
`2026-07-30-threshold-frame-integration-design.md`. Он не возвращает
устаревший параметр `source`, потому что runtime обслуживает один storage root
и не поддерживает выбор другого root в HTTP API.

## Задача

Интерфейс одновременно показывает:

- суточную timeline: состояние, события, пропуски и инциденты;
- heatmap top-сущностей выбранного view с переключением метрики;
- точный frame под курсором: сортировка, presets, rate и spark;
- detail выбранной строки, связи и точную историю;
- состояние источника, базы, репликации и диска.

Основной сценарий — диапазон 24 часа, один выбранный source и один
активный view. Timeline обслуживается только из OVF и активного
представления. PGM декодируется только для точного frame или detail.

## Нормативные бюджеты

| Операция | PGM | OVF |
| --- | ---: | --- |
| heatmap до 24 часов | 0 | один `EntitySeries` выбранного view на сегмент |
| смена метрики heatmap | 0 | 0 после попадания view в кеш |
| summary вкладок в точке | 0 | один `UiSummary` нужного сегмента |
| source context в точке | 1 сегмент | metadata |
| frame в точке | 1 сегмент | summary и series выбранного view |
| frame с counter predecessor | не более 2 сегментов | то же |
| выбор строки | 0 | данные frame |
| полный detail строки | 1 сегмент | metadata |
| история сущности | постранично, не более 32 сегментов за ответ | metadata |
| фокус инцидента | существующий bounded-контракт | существующие facts |

Число сегментов в сутках не фиксируется как 96: ранний size-seal
может создать значительно больше файлов. Heatmap budget выражается
через фактическое `N`; qualification отдельно измеряет `N=96` и
`N=1440`.

Горячий путь не сканирует directory целиком при каждом запросе.
Индекс `(source, time range) -> OVF descriptors` и directory entries
кешируются отдельно от тел блоков.

## Публичная модель

API оперирует следующими идентификаторами:

- `view` — стабильная UI-проекция, например `statements`;
- `metric` — стабильный код метрики внутри view;
- `entity` — непрозрачный base64url token из typed identity;
- `preset` — именованный набор колонок view.

Raw registry section не является UI-контрактом. Один view может
объединять несколько logical sections, вычислять колонки и
показывать gated-поля. Детали физической раскладки PGM и OVF наружу
не выходят.

## Projection catalog

`GET /v1/ui/catalog` возвращает единственный нормативный каталог интерфейса.
Клиент не зашивает source sections, формулы,
единицы или допустимость поля.

Для каждого view каталог содержит:

```text
view_code
view_revision
scope                  # database | host | instance
identity_revision
inputs[]               # logical section + type constraints
joins[]                # left/right fields, cardinality, provenance
metrics[]              # formula, unit, aggregation, gate, revision
columns[]              # type, source/formula, availability, lazy
presets[]               # ordered column codes, default sort
canonical_metric
```

`availability` имеет значения:

- `available`;
- `gated` — extension, privilege или ОС-функция недоступны;
- `not_collected` — макет знает поле, но collector его не пишет;
- `unsupported_type` — встретился неизвестный type contract.

Недоступная колонка возвращается как `null` с причиной. API не
подставляет ноль и не фабрикует данные макета.

### View catalog

| View | Inputs и join | Identity | Heatmap; canonical spark |
| --- | --- | --- | --- |
| `activity` | `pg_stat_activity`; process sample по доказанному совпадению процесса | `(pid, backend_start)` | `wait`, `cpu`, `io`; `active_fraction` |
| `statements` | logical `pg_stat_statements` | `(queryid, userid, dbid, toplevel)` | `time`, `calls`, `io`, `temp`; `time` |
| `plans` | выбранный registry contract `pg_store_plans` | identity соответствующего extension | `time`, `calls`; `time` |
| `tables` | logical `pg_stat_user_tables` с объявленными I/O и age inputs | `(datid, relid)` | `io`, `writes`, `dead`; `writes` |
| `indexes` | logical `pg_stat_user_indexes` | `(datid, indexrelid)` | `io`, `scans`; `scans` |
| `vacuum` | `pg_stat_progress_vacuum`; optional derived queue только при полном наборе inputs | registry progress identity | `progress`; `progress` |
| `processes` | process samples; optional activity label join | `(pid, starttime)` | `cpu`, `io`; `cpu` |
| `locks` | lock snapshot и activity labels с сохранённой provenance | registry lock identity или revision агрегированного дерева | `wait`; `wait` |
| `events` | нормализованные log/event facts | row identity; heatmap агрегируется по category | `count`; `count` |

Обязательные формулы метрик:

| Метрика | Формула bucket |
| --- | --- |
| statements `time` | `sum(positive_delta(total_exec_time))` |
| statements `calls` | `sum(positive_delta(calls))` |
| statements `io` | `sum(positive_delta(shared_blks_read + local_blks_read))` |
| statements `temp` | `sum(positive_delta(temp_blks_written))` |
| activity `active_fraction` | `active_samples / observed_samples` |
| activity `wait` | время в состоянии wait внутри bucket |
| tables `writes` | `sum(positive_delta(n_tup_ins + n_tup_upd + n_tup_del))` |
| tables `dead` | `max(n_dead_tup / max(n_live_tup + n_dead_tup, 1))` |
| indexes `scans` | `sum(positive_delta(idx_scan))` |
| processes `cpu` | `positive_delta(utime + stime) / elapsed` |
| processes `io` | `positive_delta(read_bytes + write_bytes) / elapsed` |
| locks `wait` | максимальная доказанная длительность wait/hold |
| events `count` | число событий category |

Остальные формулы каталога обязаны быть столь же явными до
реализации metric. Ссылка только на имя UI `cpu` или `io` не является
формулой.

PSS в текущих входах отсутствует. Колонка `pss` остаётся в каталоге
с `not_collected`, пока collector не добавит bounded
`smaps_rollup`. `cpu` и `io` Activity доступны только при
доказанном join с process identity; совпадения только по PID
недостаточно.

### Независимое расширение

Новый view получает новый `view_code`. Новая metric или column
добавляется с собственной revision. Неизвестные аддитивные записи
игнорируются по длине. Изменение одной формулы не меняет revision
других метрик и не делает весь OVF устаревшим.

## Данные OVF

Web использует два блока, определённых в
`2026-07-28-entity-series-block-design.md`:

- `UiSummary` — времена снимков, population и status всех view;
- `EntitySeries(view_code)` — top-K и все heatmap-метрики одного
  view.

`UiSummary` нужен отдельно: панель вкладок не должна читать ряды
девяти view. Шардирование `EntitySeries` по view позволяет холодному
суточному запросу не читать метрики других вкладок.
Reader адресует блок парой `(EntitySeries, view_code)`, проверяет CRC
сохранённых байтов и распаковывает ровно объявленный `decoded_len`.

Локальный top-64 каждого сегмента точен. Top диапазона является
bounded approximation: OVF хранит exact score победителей и
`cutoff_score` для пропущенных сущностей. API раскрывает нижнюю и
верхнюю границы, а не называет слияние локальных top глобально
точным.

Активный хвост строит те же структуры в памяти с теми же bounds.
После seal канонические bytes публикуются в OVF; ответ не меняет
форму на границе sealed/live.

## Общие правила HTTP

- Wire-параметры времени: `from`, `to`, `at`, signed decimal
  microseconds UTC.
- Все data endpoints требуют `source`.
- `from` включительно, `to` исключительно.
- `limit` имеет endpoint-specific default и hard maximum.
- Pagination cursor непрозрачен и связывает source, view, snapshot,
  projection revision, sort и последний key.
- Неизвестные query parameters отклоняются.
- Ошибки используют `application/problem+json` и стабильный `code`.
- Числа вне диапазона JSON integer передаются строкой согласно
  machine API contract.
- Null означает отсутствие значения; числовой ноль всегда является
  наблюдаемым нулём.

Каждый ответ с данными несёт:

```json
{
  "quality": {
    "status": "complete",
    "snapshots": 0,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

`quality.status` принимает `complete`, `partial` и `unavailable`.
Partial-ответ не превращается в HTTP error, если его границы явно
описаны. Повреждённый OVF, неверный source и превышение request bound
являются ошибками.

## GET /v1/ui/catalog

Параметр: `source`.

Ответ содержит каталог, его ETag и фактическую availability по
source. Клиент кеширует его и отправляет `If-None-Match`.

```json
{
  "revision": 1,
  "views": [
    {
      "code": "statements",
      "scope": "database",
      "identity_revision": 1,
      "canonical_metric": "time",
      "metrics": [
        {
          "code": "time",
          "revision": 1,
          "unit": "us",
          "availability": "available"
        }
      ],
      "columns": [
        {
          "code": "query",
          "type": "text",
          "availability": "available",
          "lazy": true
        }
      ],
      "presets": [
        {
          "code": "time",
          "columns": ["queryid", "query", "calls", "total", "mean"],
          "sort": { "column": "total", "order": "desc" }
        }
      ]
    }
  ]
}
```

## GET /v1/timeline/heatmap

Параметры:

- `source`;
- `view`, `metric`;
- `from`, `to`, диапазон не больше 24 часов;
- `buckets` — `1..=256`, default 56;
- `top` — `1..=64`, default 8.

Сервер читает только `EntitySeries(view_code)` пересекающихся
сегментов и активный хвост. Кандидаты — union локальных top-K.
Значения сервер агрегирует в запрошенную сетку, сохраняя null для
отсутствия. Label берётся из самого нового пересекающегося блока, где
сущность сохранена; rename не меняет entity token.

```json
{
  "grid": {
    "from_us": "0",
    "to_us": "0",
    "bucket_count": 56
  },
  "ranking": {
    "exact": false,
    "unseen_upper": 12.5
  },
  "rows": [
    {
      "entity": "AQID",
      "label": "77de",
      "unit": "us",
      "score": { "lower": 120.0, "upper": 127.5 },
      "values": [10.0, null, 0.0]
    }
  ],
  "quality": {
    "status": "complete",
    "snapshots": 0,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "unbounded_segments": [],
    "active_tail": false
  }
}
```

`ranking.exact` вычисляется по формулам спеки OVF. Если metric одного
сегмента не complete, finite upper bound для него не выдумывается:
сегмент попадает в `unbounded_segments`, а ranking exact равен false.

Δ-режим не меняет контракт. Клиент запрашивает тот же диапазон
baseline вторым вызовом и сопоставляет строки по `entity`. Оба
ответа остаются кешируемыми независимо.

## GET /v1/views/summary

Параметры: `source`, `at`.

Сервер находит последний snapshot каждого view с `ts <= at` по
`UiSummary`. Population равен числу строк именно этого snapshot.
Notable берётся из существующих event facts в выбранном окне UI.

```json
{
  "at_us": "0",
  "views": [
    {
      "view": "statements",
      "snapshot_ts_us": "0",
      "population": 500,
      "status": "complete",
      "notable": true
    }
  ],
  "quality": {
    "status": "complete",
    "snapshots": 0,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

Для `gated` view population равен null. Счётчик активного frame
точен после server-side filter и может отличаться от общего
population вкладки.

## GET /v1/ui/context

Параметры: `source`, `at`.

Endpoint закрывает source-level элементы шапки, которые не принадлежат
табличному view:

```json
{
  "snapshot_ts_us": "0",
  "databases": [
    { "entity": "AQID", "name": "postgres" }
  ],
  "instance": {
    "role": "primary",
    "server_version": "17.2"
  },
  "replication": [
    {
      "entity": "BAUG",
      "kind": "sender",
      "state": "streaming",
      "lag_us": 1200
    }
  ],
  "quality": {
    "status": "complete",
    "snapshots": 1,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

Данные берутся из зарегистрированных database, instance и replication
projections одного PGM. Запрос выполняется при открытии страницы и
смене курсора; он разделяет content cache с frame того же сегмента.
Имена баз не извлекаются из строк активной вкладки, поскольку такая
выборка неполна.

## GET /v1/frame/{view}

Параметры:

- `source`, `at`;
- `span` для spark, не больше 24 часов;
- `preset`;
- optional `database`;
- `q` для server-side текстового filter;
- `sort`, `order`;
- `limit` — `1..=200`, default 100;
- `cursor`.

Сначала `UiSummary` определяет точный последний snapshot
`ts <= at`, его предыдущий и следующий timestamps. Для snapshot
декодируются только inputs и columns выбранного preset из одного
PGM.

Для counter/rate может понадобиться predecessor из другого PGM.
Его segment заранее известен из `UiSummary`, поэтому пустые
промежуточные сегменты не читаются. Если разрыв больше
`max_rate_gap` метрики или reset нельзя исключить, второй PGM не
читается, а rate равен null с причиной.

Spark берётся из `EntitySeries(view)`. Сущность, не попавшая в
локальные top-K нужных сегментов, получает отсутствующие точки и
`complete=false`; API не делает многосегментный PGM scan ради
столбца таблицы.

Ответ содержит только preset columns. Тяжёлые SQL, plan text,
messages и все прочие поля загружаются detail-запросом. Полный row
не прячется внутри frame payload.

```json
{
  "view": "statements",
  "snapshot_ts_us": "0",
  "rate_prev_ts_us": "0",
  "neighbors": {
    "prev_us": "0",
    "next_us": "0"
  },
  "columns": [
    { "code": "queryid", "type": "u64", "unit": null },
    { "code": "total", "type": "f64", "unit": "us" }
  ],
  "rows": [
    {
      "entity": "AQID",
      "label": "77de",
      "cells": ["123", 28.4],
      "verdicts": [
        {
          "column": "total",
          "level": "warning",
          "threshold": 25.0,
          "baseline": 5.2
        }
      ],
      "spark": {
        "values": [1.0, null, 3.0],
        "complete": false
      }
    }
  ],
  "page": {
    "returned": 100,
    "matched": 500,
    "next": "opaque"
  },
  "quality": {
    "status": "complete",
    "snapshots": 0,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

`q`, sort и pagination выполняются сервером над точным snapshot.
Клиентский filter только уже загруженной страницы запрещён: он
создаёт ложный `matched`.

Вердикты включаются только для реализованных lens contracts.
Колонка без lens не получает декоративный severity.

## GET /v1/entity/{view}/{entity}

Endpoint имеет два взаимоисключающих режима.

Точка:

- `source`, `at`;
- optional `include=related`.

Возвращает все доступные поля точной строки выбранного snapshot,
availability каждого недоступного поля и связанные сущности только
при сохранённой provenance. Это источник данных дока «Строка».

История:

- `source`, `from`, `to`;
- `columns` — allowlist кодов каталога;
- `limit` — число snapshots, `1..=2000`;
- `cursor`.

Один ответ декодирует не более 32 PGM-сегментов. Общий range одного
запроса — не более 6 часов. Длинную историю клиент продолжает
cursor-вызовами; cursor фиксирует `to` и projection revisions.

```json
{
  "entity": "AQID",
  "columns": ["total_exec_time", "calls"],
  "snapshots": [
    {
      "ts_us": "0",
      "values": [28.4, "840"]
    }
  ],
  "page": {
    "next": null
  },
  "quality": {
    "status": "complete",
    "snapshots": 1,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

Entity ищется сравнением typed identity либо её canonical token.
Текстовый label, query preview и dot-separated relation name не
используются как ключ.

## GET /v1/storage

Параметр: `source`.

```json
{
  "used": {
    "pgm": 0,
    "ovf": 0,
    "journal": 0,
    "quarantine": 0
  },
  "fs_free": 0,
  "write_rate_per_day": 0,
  "full_in_days": null
}
```

Занятые bytes считаются по инвентарю файлов, свободное место —
`statvfs`. `write_rate_per_day` использует bounded окно последних
запечатанных сегментов. `full_in_days` равен null при отсутствии
положительного тренда. Конфигурация retention не угадывается, если
web её не получает.

## Карта макета

| Поверхность v5 | Источник |
| --- | --- |
| source chip | `/v1/sources` |
| database filter | `/v1/ui/context` |
| role и replication | `/v1/ui/context` |
| data health, stale/down, gaps | `/v1/timeline/health` |
| disk popup | `/v1/storage` |
| critical/warning counters | `/v1/timeline/events` |
| timeline curve и markers | существующие timeline endpoints |
| tab counts и status | `/v1/views/summary` |
| heatmap и metric switch | `/v1/timeline/heatmap` |
| таблица, presets, sort, filter | `/v1/frame/{view}` |
| row dock | `/v1/entity/{view}/{entity}?at=...` |
| entity history | `/v1/entity/{view}/{entity}?from=...&to=...` |
| incident findings | `/v1/incidents` |
| baseline/Δ | второй timeline/heatmap range, client merge |
| replay/live, cursor, zoom | client state поверх timestamps API |

Меню «dump snapshot JSON» может использовать frame/detail response.
Ссылка Grafana и share-link собираются клиентом из source, view,
entity и времени. API не встраивает vendor URL.

## Кеши и память web

Кеши разделены по стоимости и ограничены байтами:

| Кеш | Default cap | Единица |
| --- | ---: | --- |
| source/OVF metadata | 16 МиБ | descriptors и directory |
| decoded `EntitySeries` | 64 МиБ | `(content descriptor, view_code)` |
| decoded PGM projection | 128 МиБ | `(content descriptor, projection inputs)` |

Вес включает capacity коллекций, строки, dictionary и служебные
индексы, а не только длину payload. Entry больше половины cap не
кешируется. Eviction выполняется до публикации entry.
In-flight decode резервирует вес в том же cap до выделения памяти;
параллельная работа не является неучтённой добавкой поверх кеша.

Одновременно разрешены:

- 8 чтений/decompress `EntitySeries`;
- 2 PGM decode;
- 1 detail scan на request.

Ожидающий одинаковый key request присоединяется к in-flight работе,
а не запускает второй decode. Отмена HTTP request прекращает
непубликованную работу и освобождает её reservation.

History scan обрабатывает PGM последовательно и удерживает вне кеша
не более одного decoded segment. Page builder резервирует не более
8 МиБ, включая JSON values и cursor state.

## Бюджеты ответа и чтения

| Ответ | Default target | Hard maximum |
| --- | ---: | ---: |
| heatmap, top 8 × 56 | 64 КиБ | 512 КиБ |
| frame, 100 строк | 256 КиБ | 1 МиБ |
| entity point | 128 КиБ | 512 КиБ |
| entity history page | 512 КиБ | 2 МиБ |
| catalog | 128 КиБ | 512 КиБ |

Writer и API оценивают encoded size до накопления внешне
неограниченного ответа. При hard maximum возвращается partial page с
cursor, если контракт допускает pagination; иначе `response_too_large`.

Для холодного heatmap на 24 часа:

- hard bytes read не больше
  `N * max_stored_entity_series_view`;
- target при `N=96` — не больше 2 МиБ stored bytes выбранного view;
- directory metadata не перечитывается при каждом metric switch;
- после warm-up metric switch не делает storage I/O.

Latency не фиксируется без профиля машины. Qualification хранит
wall time вместе с bytes/read calls, но успешность определяется
сначала структурным бюджетом, чтобы быстрый SSD не скрывал overread.

## Ошибки и деградация

Стабильные problem codes:

| Code | Когда |
| --- | --- |
| `unknown_source` | source отсутствует |
| `unknown_view` | view не найден в catalog |
| `unknown_metric` | metric не принадлежит view |
| `invalid_entity` | token malformed или имеет другую identity revision |
| `range_too_wide` | endpoint range превышен |
| `page_stale` | cursor не соответствует snapshot/revision |
| `resource_limited` | hard bound reader/writer |
| `response_too_large` | непагинируемый ответ превысил предел |
| `corrupt_ovf` | framing, CRC или block invariant нарушен |

Gated metric, неполный spark и approximate ranking являются
состоянием данных в успешном ответе, а не transport error.

## Порядок реализации

1. Новый OVF writer/reader: directory addressing, `UiSummary`,
   `EntitySeries(view)`, bounds, codec и typed identity.
2. Projection catalog с формулами, gates, presets и availability.
3. `/v1/timeline/heatmap` и `/v1/views/summary` только поверх OVF.
4. `/v1/ui/context` и `/v1/frame/{view}` с projection decode,
   pagination и sparks.
5. `/v1/entity/{view}/{entity}` и lazy row dock.
6. `/v1/storage`, кеши и end-to-end budget qualification.

Каждый шаг обязан иметь работающий consumer-тест. Endpoint не
добавляется раньше блока или projection, который обеспечивает его
заявленный бюджет.

Числовой vertical slice из
`2026-07-30-threshold-frame-integration-design.md` реализует frame раньше
context, но не меняет порядок остальных delivery steps.

## Проверка контракта

Обязательные API и end-to-end сценарии:

- все девять view макета присутствуют в catalog;
- preset с недоступным PSS возвращает null и `not_collected`;
- Activity CPU не join-ится только по совпавшему PID;
- heatmap читает только выбранный view и 0 PGM;
- смена metric использует тот же decoded view;
- range top содержит truth внутри `[lower, upper]`;
- `ranking.exact` включается только при доказанном порядке;
- missing bucket, observed zero, gated и resource limit различимы;
- frame использует один PGM, counter boundary — максимум два;
- context возвращает полный список database независимо от active view;
- predecessor через несколько пустых сегментов находится по summary
  без чтения этих PGM;
- тяжёлый query/plan text отсутствует во frame и доступен в detail;
- server filter, sort и pagination дают стабильный `matched`;
- entity token различает database, nullable identity и `toplevel`;
- PGM/OVF кеши выдерживают byte cap при конкурентных запросах;
- payload hard maximum нельзя обойти длинными labels или columns;
- 24 часа при 96 и 1440 сегментах публикуют bytes/read/decode/RSS;
- active/sealed boundary не меняет JSON schema и presence.

## Вне объёма

- Пишущие API и пользовательские annotations.
- Точная материализация всех сущностей суток в OVF.
- Значения колонок, которых collector ещё не собирает.
- Клиентская реализация, i18n и accessibility.
- Миграция файлов и поддержка другого формата OVF.
