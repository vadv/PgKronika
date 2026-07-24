# pg_kronika-web

[English version](README.md)

`pg_kronika-web` открывает локальный каталог PGM через встроенный UI, JSON API
и Prometheus endpoint. Он читает готовые сегменты и корректные кадры
`active.parts` через `LocalDirSnapshot`, поддерживает привязанный к источникам
timeline-индекс, обновляет опубликованный store view раз в секунду и не
подключается к PostgreSQL. Один сохраняемый writer сворачивает journal deltas,
продвигает точно совпавшие sealed segments и атомарно публикует неизменяемые
timeline views.

## Настройки

| Переменная | Дефолт | Назначение |
| --- | ---: | --- |
| `KRONIKA_WEB_DIR` | обязательна | Каталог `.pgm` и необязательного `active.parts`. |
| `KRONIKA_WEB_ADDR` | обязательна | Адрес прослушивания в формате `host:port`. |
| `KRONIKA_WEB_BASIC_AUTH` | не задан | `user:password`; без него UI и `/v1/*` открыты. |
| `KRONIKA_WEB_STALE_AFTER_S` | `10` | `/readyz` возвращает `503`, если успешный refresh старше этого времени. |
| `KRONIKA_WEB_LOG` | `info` | Filter directive для `tracing-subscriber`. |
| `KRONIKA_WEB_OVERVIEW_CACHE_DIR` | `<KRONIKA_WEB_DIR>/.pgkronika-overview-cache` | Durable cache timeline-фактов для отдельных сегментов. |
| `KRONIKA_WEB_OVERVIEW_NAMESPACE` | байты canonical store path | Стабильная identity store/deployment в ключах timeline-фактов. |
| `KRONIKA_WEB_OVERVIEW_FALLBACK_SEGMENT_HOURS` | `24` | Общий лимит segment-hours, сохраняемых после восстанавливаемой ошибки durable publication. |
| `KRONIKA_WEB_OVERVIEW_FALLBACK_BYTES` | `67108864` | Byte budget canonical facts для process-local fallback. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_BYTES` | `67108864` | Logical-byte budget serialized response cache overview/health. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_ENTRIES` | `4096` | Лимит entries в serialized response cache overview/health. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_VIEWS` | `64` | Максимальное число event views, закреплённых для продолжения cursor. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_BYTES` | `536870912` | Logical-byte budget закреплённых cursor event views. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_TTL_S` | `300` | Время жизни cursor и закреплённого view в секундах. |

```sh
KRONIKA_WEB_DIR=/var/lib/pg_kronika \
KRONIKA_WEB_ADDR=127.0.0.1:8688 \
KRONIKA_WEB_BASIC_AUTH='operator:change-me' \
pg_kronika-web
```

TLS не встроен: слушайте loopback или используйте TLS reverse proxy. Basic
Auth закрывает UI и `/v1/*`; `/healthz`, `/readyz` и `/metrics` всегда
публичны. Credentials не выводятся в ошибке конфигурации и debug, но Basic Auth
не шифрует соединение.

Дефолты и ограничения timeline resource policy:

| Ресурс | Дефолт | Ограничение или ceiling |
| --- | ---: | ---: |
| Fallback после восстанавливаемой ошибки durable publication | 24 segment-hours, 64 MiB | 744 hours, 256 MiB |
| Serialized response cache overview/health | 4 096 entries, logical charge 64 MiB | Оба настраиваемых budget ненулевые и помещаются в `usize`. |
| Закреплённые event views для cursors | 64 views, logical charge 512 MiB, TTL 300 s | Все budgets ненулевые; число и байты помещаются в `usize`. |
| Период timeline query | — | 31 сутки |
| Материализованный timeline query | — | Cloned-observation charge 64 MiB; 1 048 576 observations/count inputs, 262 144 clipped coverage spans, 65 536 joint keys, 1 024 signal keys |
| Страница events | 100 элементов | 1 000 элементов |
| Notable preview | 100 элементов | Фиксируется notable policy v1 |
| Health line | — | 2 000 points |

Все семь числовых budget-переменных `KRONIKA_WEB_OVERVIEW_*` принимают
ненулевые беззнаковые десятичные целые числа. Byte-, entry- и view-budgets
должны помещаться в платформенный `usize`. Fallback дополнительно отклоняет
значения больше 744 segment-hours или 268435456 bytes. Неверное значение
останавливает startup до bind listener.

## Endpoints

Для знакомства с хранилищем сначала вызовите `/v1/sources`, `/v1/sections` и
`/v1/segments`. Эти методы показывают, какие данные доступны, до чтения строк и
запуска анализа.

| Endpoint | Параметры | Что получит оператор |
| --- | --- | --- |
| `GET /healthz` | нет | Подтверждает, что HTTP-процесс работает. |
| `GET /readyz` | нет | Показывает системе мониторинга, успевает ли сервер обновлять снимок каталога, и сообщает возраст последнего успешного обновления. |
| `GET /metrics` | нет | Отдаёт метрики Prometheus об ошибках чтения, возрасте данных, HTTP-запросах, RSS и открытых файловых дескрипторах. |
| `GET /v1/version` | нет | Сообщает версию JSON API и версию формата PGM, которые обслуживает эта сборка. |
| `GET /v1/sources` | нет | Перечисляет источники коллектора в хранилище: для каждого указаны первая и последняя временные отметки и число сегментов. |
| `GET /v1/sections` | нет | Показывает доступные наборы данных, их семантику, ключ сортировки и объединённый список зарегистрированных колонок. |
| `GET /v1/segments` | `source`, `from`, `to` | Показывает сегменты, пересекающие выбранный период, и число строк в каждой секции. Метод читает только метаданные каталога, не тела секций. |
| `GET /v1/section/{name}` | `source`, `from`, `to`; необязательные `limit`, `cursor` | Возвращает строки выбранного набора данных в порядке времени. В `gaps` указаны отсутствующие или нечитаемые интервалы, а `next_cursor` появляется, если осталась следующая страница. |
| `GET /v1/sections/batch` | `source`, `from`, `to`, список `names` через запятую; необязательный `limit` | Возвращает такие же страницы строк сразу для нескольких наборов данных, по ключу с именем секции, за один проход по пересекающимся сегментам. |
| `GET /v1/section/{name}/diff` | `source`, `from`, `to` | Преобразует накопительные счётчики в изменения и скорости в секунду для каждого объекта. Точка содержит `delta`, `rate` и `dt_micros` либо причину `nodata`, если корректную скорость вычислить нельзя. |
| `GET /v1/sections/batch/diff` | `source`, `from`, `to`, список `names` через запятую | Возвращает такой же расчёт изменений сразу для нескольких наборов данных, по ключу с именем секции, за один проход по сегментам. |
| `GET /v1/timeline/overview` | ровно один `source`, `from`, `to` | Возвращает привязанный к источнику event digest, ограниченный notable preview, health summary, coverage, freshness, completeness, exactness, count semantics и известную потерю. |
| `GET /v1/timeline/events` | один или несколько повторяемых `source`, `from`, `to`; необязательные `limit`, `cursor`, `min_severity`, `kind` | Возвращает стабильную страницу типизированных важных event facts и непрозрачный cursor, если остались события. |
| `GET /v1/timeline/health` | ровно один `source`, `from`, `to`; необязательный `step` как целое число микросекунд | Возвращает не более 2 000 health points по policy, coverage и effective step. |
| `GET /v1/anomalies` | `source`, `from`, `to`; необязательные `window`, `step`, `threshold`, `eps_rel`, `limit`, `section` | Находит интервалы, в которых скорости счётчиков или текущие значения метрик необычно изменились за выбранный период. Ответ называет ряд, метрику, интервал, направление и показатели пика; упорядочивает эпизоды по `abs(peak.m)`; даёт счётчики проверки для каждой секции и список пропущенных секций. |
| `GET /v1/incidents` | `source`, `from`, `to`; необязательные `window`, `step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section` | Объединяет близкие по времени аномальные эпизоды в кандидаты на инциденты. Когда входных данных достаточно, возвращает findings и машинные evidence; также сообщает покрытие, качество данных, состояние каталога и пропущенную работу. |
| `GET /` | нет | Открывает встроенный браузерный UI над тем же локальным снимком данных. |

`source` — беззнаковый id из ответа `/v1/sources`. `from` и `to` — знаковые
временные отметки Unix в микросекундах. Параметры длительности принимают
`250ms`, `90s`, `15m`, `2h` или секунды без суффикса. Методы чтения строк по
умолчанию возвращают 1 000 строк и ограничивают `limit` значением 10 000.
Содержимое cursor непрозрачно для клиента: передавайте его в следующий запрос
без изменений.

Период timeline `from`/`to` полуоткрытый и не может превышать 31 сутки.
Overview и health отклоняют отсутствующий или повторный `source`; events
канонизирует повторяемый набор источников сортировкой и дедупликацией. Timeline
health принимает в `step` целое число микросекунд и при необходимости
увеличивает его, чтобы ответ не превысил 2 000 points. Events по умолчанию
возвращает 100 фактов и никогда больше 1 000. Неверный cursor или cursor от
другого query даёт `400`. Истёкший или оставшийся после restart cursor даёт
`410` с `code=cursor_expired`; вытесненный или иначе отсутствующий закреплённый
view — `410` с `code=view_gone`. Ошибка capacity registry даёт `503` с
`code=cursor_capacity_unavailable` и без `Retry-After`.

```sh
curl -u operator:change-me \
  'http://127.0.0.1:8688/v1/segments?source=1&from=0&to=9223372036854775807'
```

Success/data API не зависит от языка. `Accept-Language` не меняет ответы, а
`/v1` не отправляет `Content-Language` и языковой `Vary`. Строки из PostgreSQL,
ОС и пользовательского ввода остаются буквальными; продуктовые подписи и
объяснения принадлежат UI.

Каждая application error в `/v1` имеет единственную форму RFC 9457 Problem
Details с media type `application/problem+json` и ровно пятью полями: `type`,
`status`, `code`, типизированный `params` и непрозрачный `instance`.
Человекочитаемых `title` и `detail` нет. Problem response получает
`Cache-Control: no-store`, а один server-generated correlation token помещается
в `instance` и `X-Request-ID`. Заголовки `WWW-Authenticate`, `Allow` и
`Retry-After` сохраняются там, где их требует HTTP. Неизвестная секция даёт
`404`, неверные параметры — `400`, превышение ограничения — `413`. Полный
контракт описан в [OpenAPI](openapi.json) и
[нормативной спецификации](../../docs/superpowers/specs/2026-07-21-i18n-machine-api-contract.md).

## Контракты чтения и анализа

- Запрос строк читает только пересекающиеся сегменты, проверяет CRC формата PGM
  и секций до декодирования, сводит зарегистрированные версии layout под одним
  логическим именем секции и сортирует по ключу реестра. Совпадающая запись из
  готового сегмента и `active.parts` попадает в ответ один раз.
- Timeline-факты изолированы по source. Overview preview и events pages
  используют одну typed-проекцию `EventFact`: semantic `event_id`,
  provenance-bound `event_instance_id`, поля источника и времени, notable- и
  evidence-классы, quality flags, typed payload, supporting evidence и
  приложенную потерю. Точный порядок pagination — `(sort_ts_us, event_id,
  event_instance_id)`.
- Event counts используют checked arithmetic. Суммы severity и category,
  SQLSTATE buckets top/other/missing и joint buckets top/other независимо
  сходятся с числом retained error occurrences; retained groups и физические
  observation rows считаются отдельно. Retained exactness, source completeness,
  physical-count semantics, freshness и известная потеря остаются независимыми
  полями ответа.
- Lineage-qualified durable fact files всегда проверяются до ограниченного
  process-local fallback. Заполнить fallback может только восстанавливаемая
  ошибка publication. Exact response cache overview/health ограничен числом
  записей и байтами. Event cursors закрепляют точный неизменяемый view в
  registry с ограничениями по числу, байтам и TTL и связывают canonical source
  set, query, policy и последнюю позицию сортировки с process-local случайным
  ключом ОС.
- Ответ diff отличает измеренный ноль от отсутствующего результата. Точка без
  корректной скорости содержит один из кодов ответа: `reset`, `gap`,
  `first_point`, `anomaly` или `not_collected`. В этом API `anomaly` означает,
  что время не продвинулось вперёд или типы чисел не совпали.
- Поиск аномалий сравнивает каждое текущее окно с остальными пригодными точками
  выбранного периода. Первым идёт эпизод с наибольшим `abs(peak.m)`.
  Объект `sections` считает проверенные и непроверенные положения окна, а
  `nodata_points` даёт только общую сумму: ответ anomalies не разбивает её на
  resets, gaps и интервалы выключенного сбора. Положение окна, пересекающее
  разрыв временного ряда, учитывается в `not_evaluated.discontinuity`.
  Пропущенные данные не заменяются нулями.
- Группировка инцидентов подробнее показывает неполные входные данные:
  `data_quality` отдельно считает `resets`, `gaps` и `not_collected`,
  `coverage_by_section` перечисляет интервалы без покрытия, а `skipped`
  объясняет работу, отброшенную из-за лимита. Период запроса ограничен 24 часами;
  также ограничены units, sections, materialized cells, series points, identity
  bytes, scoring work и episodes.
- Lock-evidence между секциями требует явного сохранённого producer токена
  общего наблюдения и точного совпадения `(snapshot timestamp, PID,
  backend_start)`. Равные timestamps не доказывают связь. Текущие activity- и
  lock-коллекторы выполняют разные statements, поэтому
  `cross_section_entity_join` остаётся недоступным, пока producer не сохранит
  такой токен.
- Продуктовые объяснения неполного результата используют закрытую схему
  `{ "kind": "...", "params": { ... } }`. Lens ids, enum values, formulas,
  units и evidence остаются стабильными машинными данными; в incident catalog
  нет локализованных title и question.
- Одновременно выполняется один тяжёлый запрос anomalies, incidents или
  некэшированного timeline. Одинаковые timeline misses разделяют один
  single-flight build, а cache hits слот не занимают. Другой отдельный тяжёлый
  запрос получает `503` с `code=analytic_capacity_unavailable` и
  `Retry-After: 1`, а не ждёт в очереди.

Warnings сканирования и повреждённые диапазоны журнала остаются в reader и
влияют на gaps/completeness. Они не превращаются в успешные строки.

## Метрики timeline

`/metrics` публикует gauges/counters для timeline:
`kronika_web_overview_durable_hits_total`,
`kronika_web_overview_fallback_hits_total`,
`kronika_web_overview_rebuilt_total`,
`kronika_web_overview_promotions_total`,
`kronika_web_overview_persistence_failures_total`,
`kronika_web_overview_sealed_failures_total`,
`kronika_web_store_view_generation`,
`kronika_web_overview_view_generation`,
`kronika_web_overview_data_through_us` и
`kronika_web_overview_refresh_errors_total`. Давление cursor registry видно в
`kronika_web_timeline_cursor_views`, `kronika_web_timeline_cursor_bytes` и
`kronika_web_timeline_cursor_pins_total`,
`kronika_web_timeline_cursor_resolves_total`,
`kronika_web_timeline_cursor_evictions_total`,
`kronika_web_timeline_cursor_expired_total` и
`kronika_web_timeline_cursor_capacity_rejections_total`. Активность response
cache и single-flight отражают
`kronika_web_timeline_response_cache_{hits,misses,evictions}_total`,
`kronika_web_timeline_response_cache_{entries,bytes}` и
`kronika_web_timeline_singleflight_{leaders,joins}_total`. Labels HTTP-запросов
используют фиксированные matched route templates, а не raw URI.

## Завершение и отказы

`SIGTERM` и `SIGINT` запускают graceful HTTP shutdown. Ошибка refresh
записывается в лог, а последний опубликованный view остаётся доступен; после
заданного порога `/readyz` становится stale. Если store scan успешен, а timeline
build завершается ошибкой, web публикует свежую metadata вместе с последним
пригодным timeline и не показывает частично собранный timeline. Неверная
environment configuration, ошибка первого открытия store/overview или
недоступная энтропия ОС для аутентификации cursor завершают процесс до bind.

У бинарника нет CLI-флагов. MCP, удалённые хранилища, retention и доставка
алертов не реализованы.
