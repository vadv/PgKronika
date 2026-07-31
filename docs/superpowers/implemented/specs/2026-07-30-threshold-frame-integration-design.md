# Числовые verdicts Класса 1 в frame API

Дата: 2026-07-30.

Статус: реализовано в отдельной ветке после слияния PR #140 и PR #145.

Пошаговая реализация:
`../plans/2026-07-30-threshold-frame-integration.md`.

Документ задаёт первый HTTP consumer типизированного каталога абсолютных
порогов из
`2026-07-29-absolute-threshold-catalog-design.md`. Он уточняет delivery step 4
из `2026-07-28-web-ui-api-design.md` только в части
`GET /v1/frame/{view}` и числовых verdicts. Категориальная классификация и
остальные endpoints step 4-6 остаются последующей работой.

## Контекст

После PR #145 в `kronika-analytics` существует каталог из 69 числовых политик
Класса 1. Каждая политика принимает типизированный `MetricInput` и возвращает
`Classified::Verdict` либо точную `NotClassifiedReason`. Каталог не знает PGM,
registry type IDs, HTTP routes и UI columns.

В web уже реализованы:

- root-local `GET /v1/ui/catalog`;
- `GET /v1/views/summary` поверх `UiSummary`;
- `GET /v1/timeline/heatmap` поверх `EntitySeries`;
- девять стабильных `WebView`;
- сгенерированный многофайловый OpenAPI.

До integration PR `GET /v1/frame/{view}` не существовал. Production frontend
по-прежнему не реализован: embedded `static/index.html` остаётся заглушкой.
Результатом является проверяемый API vertical slice, а не декоративная
клиентская подсветка.

Текущий runtime обслуживает один storage root и не поддерживает выбор другого
root в HTTP API. Новый endpoint не принимает устаревший параметр выбора root
из ранней версии web UI design.

## Цели

- Реализовать bounded `GET /v1/frame/{view}` для всех девяти существующих
  projections.
- Добавить в projection catalog явную связь совместимой frame column с
  `MetricId`.
- Подготавливать `MetricInput` только из согласованного snapshot и доказанного
  predecessor.
- Возвращать объяснимый результат классификации рядом с ячейкой, не повторяя
  пороги в клиенте.
- В первом vertical slice подключить 14 доказуемых per-cell политик.
- Исчерпывающе зафиксировать причины, по которым остальные 55 политик пока не
  имеют frame binding.
- Сохранить существующие бюджеты: не более двух PGM на frame, не более 24 часов
  OVF для spark, не более 200 строк и не более 1 МиБ JSON.

## Нецели

- Категориальные `state`, `wait_event`, `lock_granted`, log severity, category
  и event type.
- `GET /v1/ui/context`, `GET /v1/entity/{view}/{entity}` и `/v1/storage`.
- Реализация production frontend или изменение HTML-макета.
- Подключение всех 69 политик любой ценой.
- Добавление collector-полей, relation reloptions или новых UI views.
- Вычисление config-bound autovacuum limits без relation reloptions.
- Калибровка provisional-порогов и runtime-настройка каталога.
- Изменение Класса 2, `/v1/anomalies` и incident lenses.
- Реализация decoded-projection cache и singleflight из delivery step 6.

## Решение

Выбран один API vertical slice: projection catalog объявляет typed binding,
frame adapter вычисляет значение ячейки и точный `MetricInput`, analytics
возвращает классификацию, а HTTP слой только сериализует результат.

Отклонены два варианта:

- привязать пороги к `/v1/section/{name}`: raw registry section не является
  стабильным UI-контрактом и не умеет безопасно выражать joins и производные
  operands;
- вернуть клиенту числа порогов и классифицировать в браузере: это дублирует
  операторы, zero semantics и обработку `NotClassified`.

## Компоненты

### Projection binding catalog

`bins/pg_kronika-web/src/ui/thresholds.rs` владеет web-specific связью между
`(view, column)` и `MetricId`. Analytics не получает зависимости от web или
registry.

Внутренний контракт:

```rust
pub(crate) struct ThresholdBinding {
    pub metric_id: MetricId,
    pub view: &'static str,
    pub column: &'static str,
    pub operand: OperandKind,
}

pub(crate) enum OperandKind {
    ActivityQueryDuration,
    ActivityTransactionDuration,
    StatementMillisecondsPerRow,
    StatementMeanMilliseconds,
    StatementTimePercent,
    StatementPlanTimePercent,
    TableDeadTupleRatio,
    TableDeadTuples,
    TableSequentialScanPercent,
    TableModifiedSinceAnalyze,
    TableInsertedSinceVacuum,
    TableAutovacuumAge,
    TableAutoanalyzeAge,
    ProcessRssKib,
}

pub(crate) enum DeferredBindingReason {
    AggregateNotCell,
    MissingView,
    MissingCollectedOperand,
    IncompatibleUnit,
    NoStableCellMapping,
}
```

Один compile-time manifest перечисляет все `MetricId::ALL`: ровно 14 записей
`Bound` и 55 записей `Deferred`. Тест запрещает пропуски, дубликаты и binding
на неизвестную column. Добавление нового `MetricId` обязано сломать golden
contract, пока разработчик явно не выберет binding или причину отсрочки.

`ColumnSpec` получает optional поля `unit` и `threshold_metric`. В публичном
`GET /v1/ui/catalog` `threshold_metric` содержит стабильный строковый код
`MetricId`, а не номер enum и не копию thresholds. У deferred columns поле
отсутствует. `unit` устраняет необходимость угадывать преобразование между
значением frame и входом classifier.

### Frame projection

Новый модуль `bins/pg_kronika-web/src/ui/frame/` разделяется по
ответственности:

```text
ui/frame/
├── mod.rs          # orchestration and public build_frame entry point
├── cursor.rs       # bounded opaque pagination cursor
├── dto.rs          # JSON/OpenAPI response types
├── projection.rs   # exact snapshot decode, joins and derived columns
├── query.rs        # filter, sort, page and response budgets
├── spark.rs        # selected EntitySeries extraction
└── threshold.rs    # ProjectedRow -> MetricInput -> Classified
```

Frame projection не интерпретирует строковые `formula` из catalog как язык
выражений. Для каждого `WebView` используется явный Rust evaluator,
проверяемый integration fixtures. Строковые formulas остаются нормативным
описанием публичного catalog и должны совпадать с evaluator tests.

### Snapshot locator

`UiSummaryBlock` получает read-only метод:

```rust
pub struct SnapshotNeighbors {
    pub previous: Option<i64>,
    pub current: i64,
    pub next: Option<i64>,
}

pub fn snapshot_neighbors(
    &self,
    view_code: u16,
    at_us: i64,
) -> Option<SnapshotNeighbors>;
```

`current` — последний точный snapshot `<= at_us`. `previous` и `next`
относятся к тому же view и пропускают timestamps, где view отсутствует.
Метод не выполняет I/O.

Frame читает PGM, содержащий `current`. Второй PGM разрешён только когда
cumulative column требует predecessor, а `UiSummary` доказал его timestamp.
Промежуточные пустые PGM не читаются. Gap, reset, смена identity или превышение
типизированного `max_rate_gap=15m` дают `NotClassified`, а не приблизительный
delta; при превышении границы второй PGM не открывается. Настройка
`track_planning` берётся только из того же PGM, что и current snapshot.

### Endpoint

```text
GET /v1/frame/{view}
```

Параметры:

| Параметр | Контракт |
| --- | --- |
| `at` | обязательный unix timestamp в микросекундах |
| `span` | spark range; default `1h`, maximum `24h` |
| `preset` | optional; default — первый preset view |
| `database` | optional точный database label для database-scoped view |
| `q` | optional UTF-8 substring filter по public label и возвращаемым selected non-lazy cells, не больше 256 bytes |
| `sort` | optional column code; default из preset |
| `order` | `asc` или `desc`; default из preset |
| `limit` | `1..=200`, default `100` |
| `cursor` | optional opaque continuation token, не больше 512 bytes |

`view`, `preset` и `sort` проверяются по projection catalog до чтения PGM.
Filter, sort и matched count вычисляются сервером над точным snapshot.
Сортировка стабильна по `(sort value, entity token)`. Cursor фиксирует:

- schema version;
- view и `view_revision`;
- exact `snapshot_ts_us`;
- normalized query fingerprint без самого cursor;
- последнее `(sort value, entity token)`.

Cursor другой projection revision, snapshot или query возвращает существующий
`cursor_query_mismatch`; исчезнувший snapshot возвращает `cursor_expired`.

### Response

```json
{
  "view": "statements",
  "snapshot_ts_us": "1730000000000000",
  "rate_prev_ts_us": "1729999990000000",
  "neighbors": {
    "prev_us": "1729999990000000",
    "next_us": null
  },
  "columns": [
    {
      "code": "mean",
      "type": "f64",
      "unit": "ms",
      "threshold_metric": "pg.statements.mean_time_ms"
    }
  ],
  "rows": [
    {
      "entity": "AQID",
      "label": "77de",
      "cells": [28.4],
      "classifications": [
        {
          "column": "mean",
          "metric": "pg.statements.mean_time_ms",
          "result": {
            "status": "classified",
            "level": "warning",
            "boundary": {
              "operator": "at_least",
              "value": 10.0
            },
            "evidence": {
              "kind": "scalar",
              "observed": 28.4
            }
          }
        }
      ],
      "spark": {
        "values": [1.0, null, 3.0],
        "complete": false
      }
    }
  ],
  "page": {
    "returned": 1,
    "matched": 1,
    "next": null
  },
  "quality": {
    "status": "complete",
    "snapshots": 2,
    "gaps": [],
    "gated": [],
    "unavailable_revision": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

Для bound column массив `classifications` всегда содержит ровно одну запись:

- `status=classified` сериализует `level`, optional `boundary` и полное
  `Evidence`;
- `status=not_classified` сериализует точную `reason`.

Отсутствие записи означает только одно: column не имеет threshold binding.
Таким образом клиент отличает «нет политики» от «политика есть, но operand
неприменим или недоступен».

Числа JSON обязаны быть конечными. `i64`, `u64` и timestamps, которые могут
потерять точность в JavaScript, сериализуются строками по существующему API
стилю. Lazy query, plan и message не попадают во frame cells.

## Первые 14 bindings

| View.column | `MetricId` | `MetricInput` |
| --- | --- | --- |
| `activity.query_duration_us` | `PgActivityQueryDurationSeconds` | `Scalar((snapshot_ts-query_start)/1e6)` только для `state=active`; иначе `NotApplicable` |
| `activity.transaction_duration_us` | `PgActivityTransactionDurationSeconds` | `Scalar((snapshot_ts-xact_start)/1e6)`; без `xact_start` — `NotApplicable` |
| `statements.ms_per_row` | `PgStatementsMillisecondsPerRow` | `Scalar(delta(total_exec_time)/delta(rows))`; `delta(rows)<=0` — `NotApplicable` |
| `statements.mean` | `PgStatementsMeanTimeMilliseconds` | `Scalar(delta(total_exec_time)/delta(calls))`; `delta(calls)<=0` — `NotApplicable` |
| `statements.time_pct` | `PgStatementsTimePercent` | `Scalar(100*row_exec_delta/snapshot_exec_delta_sum)`; denominator считается после optional database filter, но до `q` и pagination |
| `statements.plan_time_pct` | `PgStatementsPlanTimePercent` | `Scalar(100*plan_delta/(plan_delta+exec_delta))`; layout без planning fields или `pg_stat_statements.track_planning!=on` из того же PGM — `NotApplicable` |
| `tables.dead_pct` | `PgTablesDeadTuplePercent` | `RatioWithFloor { ratio: dead/(live+dead), count: dead }` |
| `tables.dead_tuples` | `PgTablesDeadTuples` | `Scalar(n_dead_tup)` |
| `tables.seq_scan_pct` | `PgTablesSequentialScanPercent` | `Scalar(100*delta(seq_scan)/(delta(seq_scan)+delta(idx_scan)))` |
| `tables.modified_since_analyze` | `PgTablesModifiedSinceAnalyze` | `Scalar(n_mod_since_analyze)` |
| `tables.inserted_since_vacuum` | `PgTablesInsertedSinceVacuum` | `Scalar(n_ins_since_vacuum)`; layout без column — `NotApplicable` |
| `tables.autovacuum_age_seconds` | `PgTablesAutovacuumAgeSeconds` | `Age { epoch_seconds: last_autovacuum, now_seconds: snapshot, gate: n_dead_tup>0 }` |
| `tables.autoanalyze_age_seconds` | `PgTablesAutoanalyzeAgeSeconds` | `Age { epoch_seconds: last_autoanalyze, now_seconds: snapshot, gate: n_mod_since_analyze>=10000 }` |
| `processes.rss` | `OsProcessRssKib` | `Scalar(rmem_kb)` |

`pg_stat_statements.total_exec_time` и `total_plan_time` уже хранятся в
миллисекундах. Существующее публичное обозначение `us` для statement time
нельзя переносить в frame: в следующем PR unit исправляется на `ms`, а
затронутые metric/view revisions увеличиваются до публикации нового endpoint.
`pg_settings` материализуется в каждом segment как last-known snapshot, поэтому
проверка `pg_stat_statements.track_planning` не требует третьего PGM.

## Явно отложенные bindings

Остальные 55 `MetricId` не исчезают из контракта. Manifest присваивает каждому
одну проверяемую причину:

- `AggregateNotCell`: connection capacity, session counts, database ratios,
  checkpoints, bgwriter и другие snapshot-wide показатели;
- `MissingView`: host CPU/memory/PSI/cgroup/disk/network и replication пока не
  имеют frame view;
- `MissingCollectedOperand`: effective autovacuum threshold требует relation
  reloptions; process CPU требует доказанной нормализации scheduler ticks;
- `IncompatibleUnit`: существующая column выражает rate, когда policy ожидает
  delta, либо наоборот;
- `NoStableCellMapping`: например distinct plan count нельзя подменять числом
  запусков planner из `pg_stat_statements.plans`.

Отложенная метрика не классифицируется по похожему числу и не получает
`threshold_metric` в публичном catalog.

## Ошибки и деградация

- Неизвестные view, preset, sort column и order дают `400` до I/O.
- Отсутствующий exact snapshot возвращает успешный пустой frame с quality gap,
  если storage читаем; повреждение storage остаётся `500 store_read_failed`.
- Missing operand, reset, gap, invalid denominator, unsupported layout и
  неприменимый row context становятся точной `NotClassifiedReason`.
- Gated column остаётся `null`; classifier получает `Missing` или
  `NotApplicable`, а не числовой ноль.
- Active tail учитывается существующим `LiveView` и отражается в quality.
- Ответ больше 1 МиБ обрезается только на границе строки и возвращает
  continuation cursor. Одна строка, которая не помещается без lazy columns,
  даёт `413 query_limit_exceeded` с resource `bytes`.

## Ограничения ресурсов

- `limit`: default 100, hard maximum 200.
- `span`: default 1 час, hard maximum 24 часа.
- Current PGM: не более одного.
- Predecessor PGM: не более одного и только по `UiSummary`.
- OVF: только `UiSummary` и `EntitySeries` выбранного view.
- Serialized response: hard maximum 1 МиБ.
- Query string: существующий hard maximum 8192 bytes.
- `q`: hard maximum 256 bytes.
- Cursor: hard maximum 512 bytes.
- Все joins, materialized cells и owned strings учитываются существующими
  reader limits; endpoint не вводит unbounded collections.

## Тестирование

### Contract tests

- Manifest покрывает все 69 `MetricId` ровно один раз.
- Ровно 14 записей имеют `Bound`.
- Каждый binding указывает существующие view и column.
- Catalog публикует `threshold_metric` только для bound columns.
- Statement time unit равен `ms`, а изменённые revisions увеличены.

### Projection tests

- Все девять views строят exact frame из registry fixtures.
- Activity join принимает только `(pid, backend_start=starttime)`.
- Counter formulas используют predecessor и отклоняют reset/gap.
- Filter, sort, matched count и cursor стабильны.
- Lazy columns не попадают в frame.
- Spark не выполняет PGM scan и честно возвращает `complete=false`.

### Threshold tests

- Все 14 adapters проверяются на значения ниже, на и выше boundary.
- `state=idle` не классифицируется как query duration.
- Нулевые calls/rows и отсутствующие planning fields не создают деление через
  искусственный `max(..., 1)`.
- Dead tuple percentage сохраняет absolute count floor.
- `last_autovacuum > snapshot_ts` даёт `OutOfDomain`.
- Missing, reset, gap и gated input различимы.
- JSON сохраняет точную boundary и evidence analytics verdict.

### HTTP и OpenAPI tests

- `/v1/frame/{view}` присутствует в runtime OpenAPI и generated multifile tree.
- Unknown/duplicate parameters отвергаются.
- `limit=201`, `span>24h`, `q>256 bytes` и cursor mismatch покрыты.
- Response hard limit и максимум два PGM доказаны instrumented fixture.
- `make openapi` не оставляет diff после повторной генерации.

## Совместимость

Новый endpoint и optional `threshold_metric` являются добавлением к API.
До реализации frame statement unit/revision можно исправить без миграции
consumer. После публикации любые изменения `MetricId`, boundary semantics,
binding operands или evidence JSON требуют явного projection revision.

## Критерии приёмки

- Endpoint работает для всех девяти views и не читает больше двух PGM.
- Projection catalog содержит ровно 14 корректных numeric bindings.
- Каждая bound cell возвращает classified или not-classified result.
- Ни один deferred `MetricId` не классифицируется по приблизительно похожей
  колонке.
- Пороги и операторы остаются только в `kronika-analytics`.
- DTO/OpenAPI и generated files совпадают.
- Focused, workspace и dependency gates проходят.

## Реализация и qualification

Frame реализован для всех девяти `WebView`. Manifest покрывает 69 `MetricId`
ровно один раз: 14 `Bound` и 55 `Deferred`. Exact snapshot и predecessor
выбираются по `UiSummary`; projection открывает не более двух PGM, а spark
читает только `EntitySeries` выбранного view. Lazy query, plan и message поля
во frame не возвращаются.

Instrumented fixture содержит 96 обычных, 1 440 early-sealed и два process
сегмента. Выбранный view отсутствует во всех промежуточных сегментах, current
содержит 201 строку и EntitySeries top-K misses. Qualification доказала два
открытия PGM до и после spark, ограничение страницы 200 строками и
сериализованный ответ не более 1 МиБ.

## Последующие работы

После этого vertical slice отдельные PR могут добавить:

1. aggregate views для оставшихся host/database/replication policies;
2. relation reloptions и config-bound autovacuum bindings;
3. категориальный каталог;
4. production frontend, который красит ячейки по готовому `level`;
5. `/v1/ui/context`, entity detail/history, storage и caches delivery steps
   4-6.
