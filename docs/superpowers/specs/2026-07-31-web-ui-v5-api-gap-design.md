# Полный Web API для UI proposal v5

Дата: 2026-07-31.

Статус: DESIGN, подтверждён целевой объём backend-only.

Документ задаёт недостающий read-only HTTP-контракт для реализации
`docs/web/pgkronika-ui-proposal-v5.html` без фронтенда. Он дополняет
`2026-07-28-web-ui-api-design.md` и уточняет текущее состояние на `main`
после коммита `45fbd64`.

## Решение

Выбран полный контракт proposal v5:

- реализовать пять недостающих `GET`-маршрутов;
- расширить четыре существующих маршрута, чьих DTO или параметров недостаточно
  для макета;
- заполнить каталог колонок и presets фактически собираемыми полями;
- зарегистрировать все операции и именованные DTO в `utoipa`, Axum и
  многофайловом OpenAPI;
- не реализовывать фронтенд и пишущие действия.

HTTP-понятие `source` удаляется полностью. Runtime обслуживает ровно один
storage root:

- маршрута `/v1/sources` нет и не будет;
- query-параметра `source` нет;
- cursor, ETag и cache key не содержат `source`;
- deep-link не должен содержать `source`;
- идентичность текущего PostgreSQL-инстанса возвращает `/v1/ui/context`.

Слово `source` остаётся допустимым только в других предметных значениях,
например `ColumnSpec.source` как происхождение колонки или поле
`pg_settings.source`, полученное от PostgreSQL.

## Цели

- Дать backend-контракт каждой read-only поверхности proposal v5.
- Не заставлять будущий UI читать raw `/v1/section/*` для штатных сценариев.
- Отделить качество сохранённых данных от диагностического состояния БД.
- Возвращать доказанные значения, `null` с причиной и сохранённую provenance.
- Сохранить bounded I/O, память, количество строк и размер JSON.
- Сделать сгенерированный OpenAPI точным описанием runtime router.

## Нецели

- React/Vite-приложение и любая другая реализация фронтенда.
- `terminate backend` и другие пишущие или опасные операции.
- Пользовательские аннотации, сохранённые настройки и серверная сессия UI.
- Отдельные маршруты для share-link, темы, языка, Grafana и локальных
  настроек.
- Выдуманные значения для несобираемых полей, включая PSS.
- Мультиинстансный HTTP router или выбор другого storage root.

## Текущее состояние

Сгенерированный OpenAPI содержит 16 операций `/v1`. Для UI уже реализованы:

| Маршрут | Текущее состояние |
| --- | --- |
| `GET /v1/ui/catalog` | Девять views, metrics, columns и presets; состав колонок неполон относительно v5 |
| `GET /v1/views/summary` | Population, status, notable boolean и collection status |
| `GET /v1/timeline/heatmap` | Bounded top-K, aligned buckets, quality и ranking bounds |
| `GET /v1/frame/{view}` | Точный frame, pagination, sort, простой `q`, sparks и 14 числовых classifications |
| `GET /v1/timeline/overview` | Event digest, preview, DB-health summary и coverage |
| `GET /v1/timeline/events` | Типизированные события и cursor |
| `GET /v1/timeline/health` | Диагностический health score БД; это не качество данных |
| `GET /v1/incidents` | Кластеры, members, findings и встроенный каталог линз |

Три отсутствующих маршрута уже описаны прежней спецификацией:

- `GET /v1/ui/context`;
- `GET /v1/entity/{view}/{entity}`;
- `GET /v1/storage`.

Два обязательных маршрута прежняя карта макета не учла:

- `GET /v1/timeline/spine` для `load / nproc` и PSI;
- `GET /v1/data/quality` для freshness, coverage, gaps и состояния producer.

## Целевой реестр маршрутов

После реализации OpenAPI содержит 21 операцию `/v1`: текущие 16 и пять новых.

| Приоритет | Маршрут | Назначение |
| --- | --- | --- |
| P0 | `GET /v1/ui/context` | Инстанс, hostname, версия, базы, роль, репликация и CPU topology в точке |
| P0 | `GET /v1/entity/{view}/{entity}` | Полная строка, доказанные связи и bounded history |
| P0 | `GET /v1/timeline/spine` | Выровненные ряды `load_per_cpu` и `psi_io_some` |
| P0 | `GET /v1/data/quality` | Качество коллекции и сохранённых данных отдельно от DB health |
| P1 | `GET /v1/storage` | Размеры store, filesystem headroom, retention и прогноз |

Новые маршруты не заменяют существующие:

- события остаются в `/v1/timeline/events`;
- DB health остаётся в `/v1/timeline/health`;
- incidents и каталог линз остаются в `/v1/incidents`;
- exact table rows остаются в `/v1/frame/{view}`.

## Общие HTTP-инварианты

- Все маршруты read-only и используют `GET`.
- Время передаётся signed decimal Unix microseconds UTC.
- `from` включительно, `to` исключительно.
- Неизвестные и повторные query-параметры отклоняются до storage I/O.
- Wide integers и timestamps сериализуются decimal strings там, где число
  может выйти за точный диапазон JSON.
- `null` означает отсутствие наблюдения, но рядом присутствует машинная
  причина.
- Наблюдённый ноль остаётся числовым нулём.
- Cursor непрозрачен и связан с route schema revision, projection revision,
  точным snapshot, нормализованными параметрами и последним ключом.
- Ошибки используют существующий машинный JSON-контракт `ApiError`.
- Partial data возвращается как успешный ответ с `quality.status=partial`,
  если пределы и причины доказаны.
- Повреждение, invalid identity, stale cursor и превышение hard bound не
  маскируются partial-ответом.

## `GET /v1/ui/context`

### Параметры

| Параметр | Контракт |
| --- | --- |
| `at` | Обязательный timestamp; выбирается последний согласованный context snapshot `<= at` |

Повторные и любые другие параметры запрещены.

### Ответ

```json
{
  "snapshot_ts_us": "1730000000000000",
  "instance": {
    "hostname": "orders-db",
    "pg_version_num": 170000,
    "pg_system_identifier": "7300000000000000000",
    "role": "primary"
  },
  "host": {
    "logical_cpu_count": 16,
    "kernel_version": "6.8.0",
    "boot_id": "opaque-value"
  },
  "databases": [
    {
      "entity": "AQID",
      "oid": 16384,
      "name": "orders",
      "visibility": "full"
    }
  ],
  "replication": {
    "instance": {
      "timeline_id": 1,
      "streaming_replicas": 2,
      "replay_lag_us": null,
      "replay_lag_reason": "primary"
    },
    "replicas": [
      {
        "entity": "BAUG",
        "pid": 4810,
        "application_name": "standby-a",
        "state": "streaming",
        "sync_state": "async",
        "replay_lag_us": 400000
      }
    ]
  },
  "quality": {
    "status": "complete",
    "gaps": [],
    "gated": [],
    "active_tail": false
  }
}
```

Источники фактов:

- `instance_metadata`;
- `pg_stat_database` для полного видимого списка баз в snapshot;
- `replication_instance`;
- `replication_replicas`;
- `os_topology`.

Имя базы не извлекается из активного view. `entity` является typed identity
token и используется как фильтр database-scoped frame. Если
`pg_system_identifier` недоступен по привилегиям, поле равно `null`, а не
синтетическому идентификатору; соседнее
`pg_system_identifier_reason=permission` объясняет отсутствие.

`role` вычисляется только из `replication_instance.is_in_recovery`:
`primary` или `standby`. Отсутствующий replication snapshot даёт
`role=null` с записью в `quality.gated`.

## `GET /v1/timeline/spine`

Маршрут обслуживает непрерывные host-level сигналы верхней ленты. Raw registry
section не является публичным UI-контрактом.

### Параметры

| Параметр | Контракт |
| --- | --- |
| `from` | Обязательное начало |
| `to` | Обязательный исключающий конец |
| `buckets` | `1..=512`, default `288` |

Максимальный диапазон равен 24 часам.

### Метрики

Ответ всегда использует фиксированные коды:

| Code | Формула | Unit |
| --- | --- | --- |
| `load_per_cpu` | `os_loadavg.load1 / count(os_topology.cpu_id)` | `ratio` |
| `psi_io_some` | `os_psi(resource=io).some_avg10` | `percent` |

`load_per_cpu` отсутствует, если topology не согласована с bucket. Деление на
предполагаемое число CPU запрещено. Отсутствующий PSI даёт `null` и
`not_collected`, а не ноль.

### Ответ

```json
{
  "grid": {
    "from_us": "1729996400000000",
    "to_us": "1730000000000000",
    "bucket_count": 288
  },
  "series": [
    {
      "code": "load_per_cpu",
      "unit": "ratio",
      "aggregation": "max",
      "values": [0.32, null, 1.14],
      "value_statuses": [
        { "status": "available", "reason": null },
        { "status": "unavailable", "reason": "producer_gap" },
        { "status": "available", "reason": null }
      ]
    },
    {
      "code": "psi_io_some",
      "unit": "percent",
      "aggregation": "max",
      "values": [1.2, null, 34.0],
      "value_statuses": [
        { "status": "available", "reason": null },
        { "status": "unavailable", "reason": "producer_gap" },
        { "status": "available", "reason": null }
      ]
    }
  ],
  "quality": {
    "status": "partial",
    "snapshots": 42,
    "gaps": [
      {
        "from_us": "1729998000000000",
        "to_us": "1729998180000000",
        "reason": "producer_gap"
      }
    ],
    "gated": [],
    "resource_limited": [],
    "active_tail": false
  }
}
```

Сетка обеих серий идентична. Bucket с несколькими значениями использует
`max`, чтобы короткий пик не исчезал. Bucket без значения остаётся `null`;
`value_statuses` имеет ту же длину, что и `values`, и задаёт его причину.
События и incidents в этот payload не копируются: UI совмещает их по UTC с
`/v1/timeline/events` и `/v1/incidents`.

## `GET /v1/data/quality`

Маршрут описывает качество коллекции и хранения. Он не возвращает health score
БД и не использует слова `ok/warning/critical` для состояния PostgreSQL.

### Параметры

| Параметр | Контракт |
| --- | --- |
| `from` | Обязательное начало окна |
| `to` | Обязательный исключающий конец |

Максимальный диапазон равен 24 часам.

### Ответ

```json
{
  "status": "partial",
  "freshness": {
    "data_through_us": "1730000000000000",
    "age_us": "12000000",
    "expected_period_us": "10000000",
    "state": "late"
  },
  "producer": {
    "state": "running",
    "collector_pid": 4812,
    "collector_started_at_us": "1729900000000000",
    "last_status_at_us": "1730000000000000"
  },
  "coverage": {
    "expected_snapshots": 45,
    "observed_snapshots": 42,
    "complete_snapshots": 42
  },
  "gaps": [
    {
      "from_us": "1729998000000000",
      "to_us": "1729998180000000",
      "reason": "producer_restart"
    }
  ],
  "capabilities": [
    {
      "kind": "lens",
      "code": "OS-NET-028",
      "status": "unavailable",
      "reason": "not_collected"
    }
  ],
  "integrity": {
    "status": "complete",
    "readable_segments": 96,
    "corrupt_segments": 0,
    "quarantined_entries": 0,
    "last_catalog_refresh_us": "1730000000000000"
  },
  "quality": {
    "status": "complete",
    "resource_limited": [],
    "active_tail": false
  }
}
```

Закрытые состояния:

- верхний `status`: `fresh`, `late`, `stale`, `unavailable`, `partial`;
- `freshness.state`: `fresh`, `late`, `stale`, `unknown`;
- `producer.state`: `running`, `stopped`, `unknown`;
- capability `status`: `available`, `unavailable`, `partial`;
- integrity `status`: `complete`, `degraded`, `unknown`.

`freshness.age_us` равен `max(0, to - data_through_us)`. Состояние `fresh`
означает возраст не больше `expected_period_us`, `late` — возраст больше
ожидаемого периода, но не больше настроенного `stale_after`, `stale` — возраст
больше `stale_after`, `unknown` — отсутствие одного из необходимых фактов.

Верхний `status` вычисляется детерминированно в таком порядке:

1. `unavailable`, если в окне нет читаемых данных;
2. `stale`, если `freshness.state=stale`;
3. `partial`, если есть gap, неполное покрытие или degraded integrity;
4. `late`, если `freshness.state=late`;
5. `fresh` в остальных доказанных случаях.

Недоступная capability перечисляется отдельно и сама по себе не переводит весь
набор данных в `partial`.

`producer.state=stopped` разрешён только при сохранённом terminal status или
доказанном heartbeat contract. Возраст данных сам по себе доказывает
`stale`, но не доказывает остановку collector. При отсутствии heartbeat
возвращается `unknown`.

Причина gap публикуется только при сохранённом evidence. Иначе используется
`unknown`; API не превращает соседство deploy event в причинность.

`capabilities` объединяет availability projection inputs и incident lens
requirements без запуска полного anomaly scan. Полный per-window результат
анализа по-прежнему принадлежит `/v1/incidents`.

## `GET /v1/entity/{view}/{entity}`

Маршрут имеет два взаимоисключающих режима. `view` проверяется по projection
catalog. `entity` декодируется как base64url typed identity и обязан иметь
ожидаемую `identity_revision`.

Каждый token из frame обязан разрешаться point/detail-запросом того же view и
snapshot. Для событий token включает revision, устойчивую event identity и
достаточную привязку к snapshot; process-local номера section/row без revision
публичным token не являются.

### Point/detail

Параметры:

- обязательный `at`;
- необязательный `include=related`.

Ответ:

```json
{
  "mode": "point",
  "view": "statements",
  "entity": "AQID",
  "snapshot_ts_us": "1730000000000000",
  "fields": [
    {
      "code": "query",
      "value": "SELECT ...",
      "status": "available",
      "reason": null
    },
    {
      "code": "pss",
      "value": null,
      "status": "not_collected",
      "reason": "not_collected"
    }
  ],
  "related": [
    {
      "relation": "statement_plan",
      "view": "plans",
      "entity": "BAUG",
      "provenance": {
        "kind": "field_equality",
        "fields": ["queryid", "dbid", "userid"]
      }
    }
  ],
  "quality": {
    "status": "complete",
    "gaps": [],
    "gated": []
  }
}
```

Point/detail возвращает все catalog columns, включая `lazy=true`. Поля не
упаковываются в positional array: detail должен сохранять status и reason
каждого значения.

`related` содержит только доказанную связь. Совпадение PID без process start,
текста relation без OID или queryid без database/user scope не создаёт связь.

### History

Параметры:

- обязательные `from`, `to`, `columns`;
- необязательные `limit`, `cursor`;
- `columns` является comma-separated allowlist кодов catalog;
- `limit` равен `1..=2000`, default `500`;
- диапазон одного запроса не больше 6 часов;
- ответ читает не больше 32 PGM-сегментов.

Ответ:

```json
{
  "mode": "history",
  "view": "statements",
  "entity": "AQID",
  "columns": ["total", "calls"],
  "snapshots": [
    {
      "ts_us": "1730000000000000",
      "values": [28.4, 840],
      "statuses": ["available", "available"],
      "reasons": [null, null]
    }
  ],
  "page": {
    "next": null
  },
  "quality": {
    "status": "complete",
    "gaps": [],
    "gated": []
  }
}
```

`values`, `statuses` и `reasons` имеют одинаковую длину, равную длине
`columns`. История поддерживается только для views со стабильной
межснапшотной identity.
Catalog получает capabilities:

```json
{
  "detail": true,
  "history": true,
  "related": true
}
```

Для ephemeral event row или другого view без устойчивой identity
`history=false`; запрос истории возвращает `400 invalid_query_constraint` с
`constraint=history_supported`.

## `GET /v1/storage`

Маршрут не принимает параметров и описывает storage root, который уже
обслуживает процесс.

```json
{
  "used_bytes": {
    "pgm": 0,
    "ovf": 0,
    "journal": 0,
    "quarantine": 0,
    "other": 0
  },
  "filesystem": {
    "total_bytes": 0,
    "available_bytes": 0,
    "used_fraction": 0.0
  },
  "retention": {
    "mode": "auto_percent",
    "configured_limit": 80,
    "effective_limit_bytes": 0,
    "status": "known"
  },
  "forecast": {
    "write_rate_bytes_per_day": 0,
    "window_us": "86400000000",
    "full_in_days": null,
    "full_in_days_reason": "non_positive_rate"
  },
  "integrity": {
    "readable_segments": 0,
    "orphan_overviews": 0,
    "quarantined_entries": 0
  },
  "quality": {
    "status": "complete",
    "gated": []
  }
}
```

`used_bytes` строится по bounded layout inventory. `filesystem` использует
filesystem API для самого storage root. `other` включает только учтённые
регулярные файлы, не распознанные как PGM, OVF, journal или quarantine.

`write_rate_bytes_per_day` вычисляется по положительному росту запечатанных
PGM/OVF за bounded окно. При недостатке точек он равен `null`.
`full_in_days` равен `null`, если rate отсутствует, не положителен или
retention удалит данные раньше заполнения.

Retention config должен быть доступен web-процессу через сохранённый
producer-status contract. Если старые данные его не содержат, `status=unknown`
и числовые поля равны `null`; поле `reason` содержит машинный код
`producer_status_unavailable`.

## Расширение `GET /v1/ui/catalog`

### Capabilities и причины

`ViewSpec` получает `capabilities`. `InputSpec`, `MetricSpec` и `ColumnSpec`
сохраняют закрытое `availability` и получают необязательный
`unavailable_reason`.
Причина является машинным кодом, а не локализованной строкой.

Минимальные причины:

- `not_collected`;
- `missing_extension`;
- `permission`;
- `unsupported_type`;
- `missing_provenance`;
- `resource_limited`;
- `not_applicable`.

### Недостающие проекции v5

Собираемые registry-поля должны быть представлены в catalog и frame:

| View | Обязательные дополнения |
| --- | --- |
| `activity` | `backend_type`; replication preset только с доказанным join к `replication_replicas` |
| `plans` | `shared_hit`, `shared_read`, `first_seen`, `last_seen` |
| `tables` | `size`, `io_hit_pct`, `xid_age`, `mxid_age`, `size` preset |
| `indexes` | `size`, `io_hit_pct`, `last_idx_scan`, `size` preset |
| `vacuum` | relation label через `(datid, relid)`, `elapsed` при доказанном start evidence |
| `processes` | `threads`; `cgroup` preset только при сохранённой cgroup mapping; PSS остаётся `not_collected` |
| `locks` | `depth`, `root_pid`, `blocked_by`, `granted`, lock mode/type и wait/hold semantics |
| `events` | стабильные enum codes для severity/category и lazy typed detail |

Поля, которые уже собираются, нельзя оставлять `not_collected` только потому,
что projection catalog их ещё не описывает.

PSS остаётся `not_collected`, пока collector не добавит bounded
`smaps_rollup`. API возвращает `null` с причиной.

## Расширение `GET /v1/views/summary`

`notable: bool` недостаточен для цветного состояния неактивных вкладок.
Каждый view summary получает:

```json
{
  "notable": true,
  "notable_level": "warning",
  "notable_count": 2
}
```

Закрытые уровни: `none`, `info`, `warning`, `critical`.

`notable_level` и `notable_count` должны быть сохранены в `UiSummary`; API не
сканирует PGM девяти views ради tab bar. Уровень выводится из серверной
классификации, а не из цвета или названия view.

## Расширение `GET /v1/frame/{view}`

### Выбор колонок

Добавляется необязательный `columns` как comma-separated allowlist из `1..=32`
уникальных кодов. Пустой элемент и повтор кода запрещены. `preset` и `columns`
взаимоисключающие. Sort column обязана входить в materialized projection, но
может не отображаться, если это явно отражено в `columns` metadata ответа.

Это закрывает действие `columns` в proposal v5 без клиентского запроса raw
sections.

### Фильтр

`q` получает bounded grammar:

```text
expr        := term *(SP term)
term        := glob | column "=" glob
glob        := bare | quoted
bare        := 1*(UTF-8 character except SP, DQUOTE and BACKSLASH)
quoted      := DQUOTE *(escaped | UTF-8 character except DQUOTE and BACKSLASH) DQUOTE
escaped     := BACKSLASH (DQUOTE | BACKSLASH | "*" | "?")
column      := catalog column code
```

Условия соединяются `AND`. Glob применяется case-insensitive только к text
columns; numeric/bool/timestamp используют полное typed equality. Lazy column
не участвует в frame filter. Неэкранированные `*` и `?` являются wildcard,
экранированные — литералами. Декодированный `q` остаётся не больше 256 bytes и
содержит не больше 16 terms.

Cursor связывается с canonical parsed filter, а не с исходным расположением
пробелов.

### Фильтр базы

`database` принимает opaque token из `/v1/ui/context.databases[].entity`.
Фильтрация по display name не является идентичностью: одинаковое имя после
пересоздания базы не должно продолжать старую entity history. Token проверяется
до PGM I/O и разрешён только для database-scoped view.

### Причины `null`

Positional `cells` дополняются выровненным `cell_statuses`:

```json
{
  "cells": [null, 28.4],
  "cell_statuses": [
    { "status": "unavailable", "reason": "permission" },
    { "status": "available", "reason": null }
  ]
}
```

`null` без status запрещён. У наблюдённого нуля status равен `available`.

### Классификации

Текущие 14 числовых bindings сохраняются. Для v5 добавляются:

- доказуемые числовые bindings для отображаемых колонок, чьи operands уже
  собираются;
- категориальные classifications для `state`, `wait_event`, lock state,
  log severity/category и replication state;
- `not_classified` с точной причиной для недоказанного predecessor, reset,
  gap, permission и missing config.

Клиент не должен угадывать severity из текста. Provisional thresholds не
добавляются только ради совпадения с mock-цветом.

## Расширение `GET /v1/incidents`

Текущего incident DTO недостаточно для focus bar, counters и evidence graph.
Каждый incident получает:

- `peak_ts_us`;
- `level`: `info`, `warning`, `critical`;
- `category_code`;
- `summary_code`;
- `finding_count`;
- `coincident_count`;
- `relations[]`.

Каждый finding дополнительно получает `confidence_cap` и `slug`.

`relations[]` имеет форму:

```json
{
  "from_finding": 0,
  "to_finding": 1,
  "kind": "proven",
  "provenance": {
    "contract": "statement_plan",
    "fields": ["queryid", "dbid", "userid"]
  }
}
```

`kind=proven` разрешён только при сохранённом join evidence. Временное
совпадение не создаёт relation; UI показывает его как coincident по роли
finding.

Incident `level` задаёт отдельная детерминированная server policy с revision.
Confidence не является severity и не преобразуется в level напрямую.
`summary_code` и `category_code` являются языконейтральными; локализованный
заголовок принадлежит UI.

Встроенный `catalog` остаётся нормативным источником списка линз. Отдельный
`/v1/lenses` не добавляется.

## Карта proposal v5

| Поверхность | Контракт |
| --- | --- |
| Инстанс | `/v1/ui/context` |
| Фильтр базы | `/v1/ui/context` + `database` token в frame |
| Роль и репликация | `/v1/ui/context` |
| Data-quality chip и stale banner | `/v1/data/quality` |
| DB warning/critical counters | `/v1/incidents` |
| Load/PSI spine | `/v1/timeline/spine` |
| Event markers и legend codes | `/v1/timeline/events` |
| Incident markers и focus | `/v1/incidents` |
| Tab counts и notable level | `/v1/views/summary` |
| Heatmap | `/v1/timeline/heatmap` |
| Presets, sort, filter, columns | `/v1/ui/catalog` + `/v1/frame/{view}` |
| Row detail и related drills | `/v1/entity/{view}/{entity}?at=...` |
| Entity history | `/v1/entity/{view}/{entity}?from=...&to=...` |
| Disk/retention popup | `/v1/storage` |
| Baseline heatmap | клиентское сравнение buckets одного heatmap response |
| Replay/live/cursor/zoom | клиентское состояние поверх timestamps и frame neighbors |
| Share-link | клиентская сериализация URL-state |
| AI context | клиентская сборка уже redacted API payload |
| Snapshot JSON | экспорт полученных frame/detail/context payload |
| Grafana link | клиентская конфигурация и URL-state |
| Theme/timezone | локальные настройки клиента |

Отдельные API не добавляются для последних семи клиентских действий.

## Producer и storage prerequisites

Новые маршруты используют уже собираемые секции, но требуют трёх backend
дополнений:

1. `UiSummary` сохраняет `notable_level` и `notable_count`.
2. Producer status сохраняет heartbeat/terminal state и фактическую retention
   config рядом с root или в versioned service section.
3. Timeline index получает bounded host signal block либо эквивалентный
   адресуемый индекс для `load_per_cpu` и `psi_io_some`.

Endpoint не сканирует все raw PGM за 24 часа без структурного read budget.
Если host signal block ещё не создан, сначала реализуется producer/reader
контракт, затем HTTP consumer.

## Ресурсные пределы

| Ответ | Целевой размер | Жёсткий максимум |
| --- | ---: | ---: |
| UI context | 64 КиБ | 256 КиБ |
| Timeline spine | 32 КиБ | 256 КиБ |
| Data quality | 64 КиБ | 512 КиБ |
| Entity point | 128 КиБ | 512 КиБ |
| Entity history page | 512 КиБ | 2 МиБ |
| Storage | 16 КиБ | 64 КиБ |

Дополнительные пределы:

- context читает не больше одного PGM выбранного снимка;
- spine читает только адресуемые host signal blocks и 0 raw PGM;
- data quality читает descriptors, coverage/status metadata и 0 section
  bodies;
- entity point читает не больше одного PGM выбранного снимка;
- entity history читает PGM последовательно, не больше 32 сегментов и
  удерживает не больше одного decoded segment вне кеша;
- storage inventory ограничен существующими layout bounds;
- serialized size проверяется до публикации ответа.

## Ошибки

Добавляется один предметный код для отсутствующей сущности:

| Code | Когда |
| --- | --- |
| `entity_not_found` | Identity корректна, но строки нет в выбранном snapshot |

Остальные ошибки следуют текущему `ApiError`:

- malformed entity token возвращает `invalid_query_parameter` с
  `expected=entity_token`;
- history для view без устойчивой identity возвращает
  `invalid_query_constraint` с `constraint=history_supported`;
- неизвестная колонка возвращает `invalid_query_parameter`;
- взаимоисключающие режимы и `preset` вместе с `columns` возвращают
  `invalid_query_constraint`;
- слишком широкий диапазон и превышение inventory bound возвращают
  `query_limit_exceeded` с точным `resource`, `limit` и, если известно,
  `observed`;
- `invalid_cursor`, `cursor_query_mismatch`, `cursor_expired`,
  `store_read_failed` и `internal_error` переиспользуются без новых синонимов.

`unknown_source` удаляется из целевого web UI API и не должен появляться в
новых тестах или OpenAPI.

## OpenAPI и router

Rust DTO остаются единственным источником wire-схем:

- каждый новый handler имеет `#[utoipa::path]`;
- successful response использует именованный `ToSchema` DTO;
- все error statuses ссылаются на `ApiError`;
- `src/api_docs.rs` подключает route один раз через `OpenApiRouter`;
- operation ID уникален и стабилен;
- exporter относит новые пути и schemas к доменам `ui` или `timeline`;
- `/openapi.json`, runtime router и многофайловый
  `bins/pg_kronika-web/openapi/openapi.yaml` описывают одинаковые операции.

После реализации выполняется:

```sh
make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
```

Первую команду запускает разработчик для обновления committed артефактов.
Вторая команда после повторной генерации доказывает отсутствие drift.

## Тестирование

### Contract tests

- OpenAPI содержит ровно 21 `/v1` operation.
- Все пять новых операций имеют named success schema, явные statuses и tags.
- Неизвестный и повторный параметр каждого маршрута отклоняется до I/O.
- Ни один UI route не принимает `source`.
- В OpenAPI нет `/v1/sources` и `unknown_source`.

### Integration tests

- Context возвращает базы независимо от активного view.
- Primary и standby не получают выдуманных replication fields.
- Spine выравнивает обе серии по одной сетке и различает `null`/`0`.
- Spine не читает raw PGM.
- Data quality различает stale data и доказанный stopped producer.
- Неизвестная причина gap остаётся `unknown`.
- Entity point возвращает lazy fields и per-field reason.
- Related entity появляется только при доказанной provenance.
- History cursor плиточно покрывает все snapshots без дублей и пропусков.
- Ephemeral entity history отклоняется стабильным code.
- Storage accounting не считает один файл в двух категориях.
- Forecast не появляется при нулевом или недоказанном rate.
- Frame fielded filter, custom columns и pagination имеют стабильный matched.
- Incident relation не строится из одного совпадения времени.

### Qualification

- Spine: 24 часа при `N=96` и `N=1440`, bytes/read/decode/RSS.
- Entity history: 32 максимальных PGM, 2 МиБ JSON, отмена запроса.
- Data quality: максимальное число gaps/capabilities в 512 КиБ.
- Storage: максимальный разрешённый layout inventory.
- OpenAPI reverse assembly равен `/openapi.json`.
- Demo API smoke вызывает по одному характерному запросу каждого нового
  маршрута.

## Порядок реализации

1. Удалить HTTP `source` из старой UI API-спеки, error inventory и
   незавершённых frontend assumptions.
2. Расширить shared projection catalog и `UiSummary`.
3. Добавить producer status/retention metadata и host signal index.
4. Реализовать `/v1/ui/context` и `/v1/data/quality`.
5. Реализовать `/v1/timeline/spine`.
6. Реализовать entity point, затем related provenance, затем bounded history.
7. Реализовать `/v1/storage`.
8. Расширить frame и incidents DTO.
9. Подключить все handlers к router/OpenAPI и обновить generated tree.
10. Расширить demo smoke и qualification.

Каждый шаг должен оставлять runtime router, `/openapi.json` и generated
OpenAPI согласованными.

## Критерии приёмки

- Все пять новых URL отвечают реальными bounded данными или явным состоянием
  availability, а не заглушкой.
- Все 21 операции `/v1` присутствуют в Swagger и runtime router.
- Proposal v5 не требует raw section API для штатного read-only сценария.
- В HTTP-контракте UI отсутствует `source`.
- Data quality и DB health являются разными контрактами.
- Каждое отсутствующее значение имеет машинную причину.
- Каждая related/incident связь имеет сохранённую provenance.
- Catalog и frame покрывают собираемые поля proposal v5; несобираемый PSS
  честно остаётся `not_collected`.
- Тесты, OpenAPI drift check, demo smoke и структурные resource qualifications
  проходят.
