# Машинный HTTP-контракт `pg_kronika-web` до первого релиза

Дата: 2026-07-21. Статус: нормативный целевой контракт.

Документ фиксирует атомарное изменение существующего `/v1` до первого релиза.
Предыдущая форма ошибок не поддерживается, переходный режим отсутствует.
OpenAPI-представление этого контракта находится в
`bins/pg_kronika-web/openapi.json`; тесты web-пакета проверяют его против
реестров кода.

## 1. Граница языка

- Публичный API и API first-party UI полностью language-neutral: locale не
  меняет success data, ошибки, идентификаторы, enum, числа, timestamps, units,
  formulas, evidence или причины неполного результата.
- `/v1` игнорирует `Accept-Language`. Data и problem responses не отправляют
  `Content-Language` и не добавляют языковой `Vary`.
- API не принимает locale query/header, не возвращает translation keys и не
  имеет server-side переводов.
- Product-owned presentation принадлежит будущим UI-каталогам. Этот контракт
  не реализует UI или i18n runtime.
- Literal source/user values остаются байт-в-байт теми данными, которые
  декодировал reader: SQL, PostgreSQL log message/detail/hint/context,
  database/user/application/relation names, paths, host/process strings и
  другие сохранённые значения не переводятся и не используются как шаблоны.

## 2. Routing и media types

- Успехи существующих `/v1` routes возвращают `application/json`.
- Любая application error существующего или неизвестного `/v1` route, включая
  неверный HTTP-метод, query validation, auth, reader, admission и worker
  failure, возвращает только `application/problem+json`.
- Нет serializer alias, legacy `{error,detail}`, alternative media type,
  negotiation по `Accept`, deprecation или новой параллельной версии API.
- `GET` — единственный application method; `HEAD` имеет стандартную HTTP
  семантику. Остальные методы получают `405 method_not_allowed` и
  `Allow: GET, HEAD`.
- `401 unauthorized` сохраняет
  `WWW-Authenticate: Basic realm="pg_kronika-web"`.
- `503 analytic_capacity_unavailable` сохраняет `Retry-After: 1`.
- Каждый Problem получает `Cache-Control: no-store`.
- `/healthz` и `/readyz` остаются machine probe representations в
  `application/json`; `503` от `/readyz` описывает состояние readiness, а не
  Problem Details. `/metrics` остаётся Prometheus exposition. Static SPA/asset
  responses вне `/v1` следуют собственной cache/media policy. Ошибка Basic
  Auth для защищённой статики использует тот же `unauthorized` Problem.

## 3. Problem Details

Каждое тело имеет ровно пять полей:

```json
{
  "type": "https://pgkronika.dev/problems/invalid-query-parameter",
  "status": 400,
  "code": "invalid_query_parameter",
  "params": {
    "parameter": "source",
    "expected": "uint64"
  },
  "instance": "urn:pgkronika:request:0123456789abcdef0123456789abcdef"
}
```

`title`, `detail`, `error` и любые дополнительные поля запрещены.

- `type` — фиксированный абсолютный URI класса проблемы.
- `status` точно совпадает с HTTP status.
- `code` — закрытый enum клиентской логики.
- `params` — обязательный закрытый object, определённый `code`.
- `instance` — occurrence URN. Суффикс — 32 lowercase hex символа из
  server-generated SHA-256 correlation token; он не содержит URI, query,
  credential, path, SQL или error chain.
- Тот же суффикс находится в `X-Request-ID`. Входной `X-Request-ID` не
  принимается и не отражается.

### 3.1. Реестр кодов

| `code` | HTTP | `params` |
| --- | ---: | --- |
| `unauthorized` | 401 | `{}` |
| `route_not_found` | 404 | `{}` |
| `method_not_allowed` | 405 | `{}` |
| `missing_query_parameter` | 400 | `parameter` |
| `invalid_query_parameter` | 400 | `parameter`, `expected` |
| `unknown_query_parameter` | 400 | bounded `parameter` token |
| `duplicate_query_parameter` | 400 | `parameter` |
| `invalid_query_constraint` | 400 | `constraint` |
| `unknown_section` | 404 | bounded `section` token |
| `invalid_cursor` | 400 | `{}` |
| `query_limit_exceeded` | 413 | `resource`, `limit`, optional `observed` |
| `analytic_capacity_unavailable` | 503 | `retry_after_seconds` |
| `store_read_failed` | 500 | `{}` |
| `internal_error` | 500 | `{}` |

`parameter` для известных полей: `query`, `source`, `from`, `to`, `window`,
`step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section`,
`names`, `limit`, `cursor`. `query` обозначает malformed URL-encoded query, а
не разрешённый query key.

`expected`: `url_encoded_query`, `uint64`, `int64`, `positive_duration`,
`non_negative_finite_number`, `non_negative_integer`, `section_list`.

`constraint`: `from_before_to`, `window_within_interval`,
`epsilon_not_greater_than_max_cluster_span`,
`max_cluster_span_within_interval`, `finite_scan`.

`resource`: `query_bytes`, `query_parameters`, `query_span_us`,
`window_positions`, `rows`, `cells`, `bytes`, `units`, `sections`,
`identity_bytes`, `series_points`, `episodes`, `clusters`,
`incident_key_bytes`, `total_incident_key_bytes`.

Unknown query and section names are reflected only when they match the bounded
ASCII token grammar `[A-Za-z0-9_.-]{1,64}`; otherwise the stable token
`invalid` is returned. Query parsing retains at most 32 pairs after enforcing
an 8192-byte raw-query ceiling.

### 3.2. Disclosure

- `QueryError::Read` always becomes `store_read_failed {}`.
- Join failure, registry inconsistency, duplicate series/lens, invalid internal
  cursor, worker panic/cancellation and every other non-actionable internal
  failure become `internal_error {}`.
- Client bodies never receive source paths, errno text, SQL, raw query values,
  credentials, join errors, panic payloads, debug formatting or error chains.
- Server logs may record a bounded event name, the generated request id and
  structured internal error; problem codes are not new metric labels.

## 4. Типизированные причины в success data

Product-owned reason всегда имеет одну форму:

```json
{
  "kind": "materialization_limit",
  "params": {
    "resource": "cells",
    "limit": 1000000
  }
}
```

`params` обязателен и закрыт даже для причины без аргументов (`{}`). Аргументы
не дублируются рядом с `kind`.

| `kind` | `params` |
| --- | --- |
| `materialization_limit` | `resource: cells|bytes`, `limit` |
| `incomplete_page` | `{}` |
| `scoring_work_budget` | `required`, `available` |
| `scan_budget` | `required`, `available` |
| `conflicting_timestamp` | `timestamp` |
| `identity_byte_limit` | `observed`, `limit` |
| `series_point_limit` | `observed`, `limit` |
| `typed_gauge_point_limit` | `observed`, `limit` |
| `snapshot_row_limit` | `observed`, `limit` |
| `incomplete_snapshot` | `{}` |
| `retention_limit` | `dropped` |
| `no_data` | `{}` |
| `missing_node_identity` | `{}` |
| `conflicting_node_identity` | `{}` |
| `producer_unavailable` | `{}` |
| `provenance_or_input_missing` | `{}` |
| `complete_provenance` | `gap_count` |
| `section_absent` | `{}` |
| `complete_coverage` | `gap_count` |
| `coverage_gap` | `gap_count` |

`anomalies.skipped[].reason`, incident section/analysis skips и
`catalog.capabilities[].reason` используют этот реестр. Diff `nodata` values
(`reset`, `gap`, `first_point`, `anomaly`, `not_collected`) остаются bounded
domain enum, а не prose.

## 5. Incident catalog

- Stable `lens_id`, `slug`, `domain`, `confidence_cap`, capability codes,
  requirements/status и evidence остаются machine data.
- `log.catalog[].question`, `log.catalog[].text_locale`,
  `catalog.dormant[].title`, `catalog.dormant[].question` и
  `catalog.dormant[].text_locale` удалены из response и domain structs.
- Finding summary/detail не добавляются. Будущий UI владеет EN/RU catalog по
  `lens_id`; начальные языки — продуктовая гипотеза, не API constraint.
- Locale не участвует в `incident_key`, clustering, scoring, archive или PGM.

## 6. Совместимость, хранение и rollback

- Контракт `/v1` меняется in place до первого релиза. Promise совместимости с
  текущим checkout отсутствует.
- Нет migration сохранённых данных: PGM codecs и raw values не меняются.
- Все in-repo clients, snapshots и docs изменяются атомарно.
- Rollback — только возврат git/deploy artifact; старый wire format не
  включается feature flag.

## 7. Исполняемая приёмка

- Unit registry test строит пример каждого `ProblemCode` и каждого reason
  `kind`, проверяет exact keys, status/type, закрытый params allowlist и
  отсутствие prose fields.
- HTTP tests проверяют media type, `no-store`, correlation, `WWW-Authenticate`,
  `Retry-After`, `Allow`, unknown route, invalid method и probe/metrics
  exceptions.
- `Accept-Language` matrix проверяет равенство machine success и problem
  semantics и отсутствие `Content-Language`/language `Vary`.
- Security fixtures проверяют, что path/error chain/incoming request id не
  появляются в body.
- Incident/anomaly snapshots проверяют удаление presentation и единую reason
  shape.
- OpenAPI JSON парсится тестом; его code/kind enum и schemas сверяются с Rust
  registries.
- BDD transport сохраняет status, headers, media type и body; отдельный
  сценарий проверяет нейтральный RFC 9457 response.

Новые структуры имеют постоянный размер. Query parser ограничен 8192 bytes и
32 pairs; response params не содержат коллекций или неограниченного текста.
Изменение не добавляет materialization данных reader, cache variants или
high-cardinality metric labels.
