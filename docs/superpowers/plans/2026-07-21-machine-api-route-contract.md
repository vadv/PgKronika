# Маршруты машинного API — план развития

Статус: follow-up к
[`2026-07-21-i18n-machine-api-contract.md`](../specs/2026-07-21-i18n-machine-api-contract.md).
Текущий `/v1` содержит только `GET`-ресурсы со стандартным `HEAD`; их
`Allow: GET, HEAD` корректен.

## Границы

- План относится только к `/v1`; probes, metrics и static UI сохраняют свои
  transport-контракты.
- Wire format, problem/reason registries и ручной OpenAPI остаются без нового
  генератора или compatibility-слоя.
- Streaming success responses и оптимизации входят в работу только после
  измеренного превышения действующих memory/latency limits.

## Порядок

| № | Задача | Trigger и причина | Scope | Acceptance |
|---:|---|---|---|---|
| 1 | Единый manifest `/v1` routes | Перед добавлением, удалением или переименованием route. Текущий тест доказывает OpenAPI → Router, но не обнаруживает новый undocumented Router path. | Одна статическая декларация задаёт path template, methods и query allowlist; router registration и contract-test metadata строятся из неё. | Множества manifest/OpenAPI совпадают в обе стороны; каждый manifest path достижим через production `app()`; добавление route только в Router или OpenAPI ломает тест; manifest не добавляет request-time allocations. |
| 2 | Resource-specific `Allow` | До первого `/v1` resource с методом кроме `GET`. Глобальная строка корректна только пока method sets одинаковы. | Method set хранится в manifest; `HEAD` добавляется только к `GET`; 405 fallback и OpenAPI header берут значение конкретного resource. | Тесты проверяют 405 body/header для каждого различного method set, отсутствие лишних методов и точное соответствие OpenAPI. |

Сначала вводится manifest, затем из него выводится `Allow`; отдельная таблица
методов до manifest создала бы второй источник истины.
