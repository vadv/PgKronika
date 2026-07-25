# pg_kronika-web

[English version](README.md)

`pg_kronika-web` открывает локальный каталог PGM через встроенный UI, JSON API
и Prometheus endpoint. Он читает готовые сегменты и корректные кадры
`active.parts` через `LocalDirSnapshot`, поддерживает привязанный к источникам
timeline-индекс, обновляет опубликованный store view раз в секунду и не
подключается к PostgreSQL. Один сохраняемый writer сворачивает journal deltas,
продвигает точно совпавшие sealed segments и атомарно публикует неизменяемые
представления дескрипторов и live-данных. Тела sealed-сегментов загружаются
только для допущенных timeline-запросов.

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
| `KRONIKA_WEB_OVERVIEW_GC_MAX_ENTRIES` | `100000` | Максимальное число записей в полном сканировании кэша фактов; достижение предела запрещает удаление. |
| `KRONIKA_WEB_OVERVIEW_GC_GRACE_GENERATIONS` | `2` | Число разных авторитетных поколений GC до допуска неактуального готового файла к удалению. |
| `KRONIKA_WEB_OVERVIEW_GC_WALL_GRACE_S` | `120` | Минимальное время после первого авторитетного обнаружения неактуального файла, секунды. |
| `KRONIKA_WEB_OVERVIEW_GC_ARTIFACT_GRACE_S` | `600` | Минимальный возраст распознанного временного или карантинного файла перед удалением, секунды. |
| `KRONIKA_WEB_OVERVIEW_CACHE_MAX_LOGICAL_BYTES` | не задан | Необязательный предел суммы логических `st_size` учтённых файлов в пространстве имён кэша. |
| `KRONIKA_WEB_OVERVIEW_CACHE_MAX_FILES` | не задан | Необязательный предел числа учтённых файлов в пространстве имён кэша. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_BYTES` | `67108864` | Logical-byte budget serialized response cache overview/health. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_ENTRIES` | `4096` | Лимит entries в serialized response cache overview/health. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_VIEWS` | `64` | Максимальное число event views, закреплённых для продолжения cursor. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_BYTES` | `536870912` | Logical-byte budget закреплённых cursor event views. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_TTL_S` | `300` | Время жизни cursor и закреплённого view в секундах. |
| `KRONIKA_WEB_OVERVIEW_MAX_SELECTED_SEGMENTS` | `1024` | Действующий лимит sealed-сегментов в одном timeline-запросе; допустимый диапазон `1..=4096`. |

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
| Сканирование кэша фактов | 100 000 записей | 1 000 000 записей |
| Grace для неактуального готового файла | 2 разных авторитетных поколения GC и 120 s | Оба значения ненулевые; поколений должно быть не меньше 2 |
| Grace служебных файлов публикации | 600 s | Ненулевое значение |
| Допуск в постоянный кэш фактов | По умолчанию нет предела байтов и файлов | Необязательные ненулевые пределы логических байтов и числа файлов |
| Serialized response cache overview/health | 4 096 entries, logical charge 64 MiB | Оба настраиваемых budget ненулевые и помещаются в `usize`. |
| Закреплённые event views для cursors | 64 views, logical charge 512 MiB, TTL 300 s | Все budgets ненулевые; число и байты помещаются в `usize`. |
| Выбранные sealed-сегменты в одном timeline-запросе | 1 024 | Настраивается от 1 до абсолютного предела v1: 4 096 |
| Период timeline query | — | 31 сутки |
| Материализованный timeline query | — | Cloned-observation charge 64 MiB; 1 048 576 observations/count inputs, 262 144 clipped coverage spans, 65 536 joint keys, 1 024 signal keys |
| Страница events | 100 элементов | 1 000 элементов |
| Notable preview | 100 элементов | Фиксируется notable policy v1 |
| Health line | — | 2 000 points |

Числовые параметры `KRONIKA_WEB_OVERVIEW_*` задаются беззнаковыми десятичными
целыми. Обязательные бюджеты и интервалы должны быть ненулевыми; оба предела
постоянного кэша можно не задавать. Значения байтов, записей и представлений,
которые преобразуются в размер процесса, должны помещаться в платформенный
`usize`. Fallback дополнительно отклоняет значения больше 744 segment-hours
или 268435456 bytes. Лимит выбранных сегментов должен находиться в диапазоне
`1..=4096`. Неверное значение завершает запуск до открытия listener.

## Работа постоянного кэша фактов

Выделяйте каждому web-процессу отдельный каталог кэша, если развёртывание не
предусматривает один процесс с правом записи. Первый `FactStore`, захвативший
корневую блокировку, удерживает её всё время своей работы. Другие процессы с
тем же каталогом могут читать готовые факты, но публикация и GC возвращают
конфликт; новые факты остаются в ограниченном локальном fallback процесса.

Web запрашивает GC после каждых 60 успешных публикаций timeline. Grace по
поколениям продвигается только при разных полных авторитетных сканированиях GC,
а не при обычных обновлениях. При настройках по умолчанию с первого
сканирования, которое не нашло готовый файл в актуальном наборе, также должно
пройти 120 секунд; поэтому удаление может потребовать ещё одного сканирования.
Недоступный запечатанный сегмент, ошибка сканирования или достижение лимита
запрещают удаление и не продвигают grace. GC работает только внутри
`overview/v1`, проверяет идентичность готового файла и не касается исходных
PGM, `active.parts`, блокировок, символьных ссылок и посторонних файлов.

Необязательные пределы постоянного кэша считают логические размеры и число
файлов. Это не свободное место и не физическая квота файловой системы. Если
полное сканирование не может допустить публикацию без превышения заданного
предела, ответ всё равно использует ограниченный fallback в памяти. При
`ENOSPC` или исчерпании настроенной квоты выполняется не больше одного
авторитетного прохода GC и одной повторной публикации.

Задержка повторной записи не блокирует чтение готовых фактов. Даже если новых
фактов нет, обновление запускает одну созревшую проверку восстановления. После
ошибок доступа и файловой системы только для чтения первая проверка выполняется
через пять минут. При нехватке места и временных ошибках I/O действует
экспоненциальная задержка с индивидуальным для каждого экземпляра разбросом и
пределом пять минут. Ошибки пути, структуры кэша и идентичности, а также
неклассифицированные ошибки I/O сообщаются без включения общей задержки.

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
увеличивает его, чтобы ответ не превысил 2 000 points. До проверки response
cache, регистрации response flight, допуска аналитики и закрепления нового
cursor первый запрос страницы строит план пересекающихся sealed-дескрипторов
для канонического набора источников. Превышение действующего лимита даёт `400`
с `code=query_limit_exceeded` и
`params.resource=selected_segments`. Events применяет один общий лимит ко всему
дедуплицированному набору источников. Данные live-журнала не считаются
sealed-сегментами и ограничиваются отдельно. Events по умолчанию возвращает
100 фактов и никогда больше 1 000. Неверный cursor или cursor от другого query
даёт `400`. Истёкший или оставшийся после restart cursor даёт `410` с
`code=cursor_expired`; вытесненный или иначе отсутствующий закреплённый view —
`410` с `code=view_gone`. Ошибка capacity registry даёт `503` с
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
`404`, неверные параметры — `400`, а существующие ограничения входных данных
и материализации — `413`. Превышение лимита выбранных sealed-сегментов
считается ошибкой формы запроса и возвращает `400`. Полный контракт описан в
[OpenAPI](openapi.json) и
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
- При обновлении timeline публикуются полученные из каталогов
  sealed-дескрипторы и одно ограниченное поколение live-данных; тела секций
  sealed-сегментов при этом не декодируются. Допущенный запрос загружает только
  факты из выбранного плана source/range. Холодная сборка разделяется по
  полному lineage-qualified `FactBuildKey`, продолжается после отмены запроса
  и проходит через глобальный FIFO-диспетчер: четыре worker, очередь из
  64 элементов, не более четырёх загрузок от одного запроса и общие веса для
  PGM, декодированной памяти, CPU, файловых дескрипторов, чтения, записи и
  публикации. Отказ по capacity даёт `503` с
  `code=overview_capacity_unavailable` и `Retry-After: 1`.
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
- Одновременно выполняется одна тяжёлая проекция ответа anomalies, incidents
  или некэшированного timeline. Одинаковые промахи response cache разделяют
  один response flight, а попадания в кэш не занимают слот. Другой отдельный
  тяжёлый запрос получает `503` с `code=analytic_capacity_unavailable` и
  `Retry-After: 1`, а не ждёт в очереди.

Warnings сканирования и повреждённые диапазоны журнала остаются в reader и
влияют на gaps/completeness. Они не превращаются в успешные строки.

## Метрики timeline

`/metrics` публикует монотонные счётчики работы timeline:
`kronika_web_overview_durable_hits_total`,
`kronika_web_overview_fallback_hits_total`,
`kronika_web_overview_rebuilt_total`,
`kronika_web_overview_promotions_total`,
`kronika_web_overview_persistence_failures_total`,
`kronika_web_overview_sealed_failures_total`. Счётчики загрузки фактов
увеличиваются, когда допущенный запрос загружает выбранные факты; первая
публикация содержит только дескрипторы. Продвижение представления показывают
`kronika_web_store_view_generation`,
`kronika_web_overview_view_generation`,
`kronika_web_overview_data_through_us` и
`kronika_web_overview_refresh_errors_total`.

Состояние постоянной записи показывают
`kronika_web_overview_persist_{mode,failures,retry_after_seconds,probe_in_flight}`,
взаимоисключающие gauges с закрытым набором значений labels
`kronika_web_overview_persist_reason{reason}` и
`kronika_web_overview_persist_failure_class{class}`, а также
`kronika_web_overview_persist_probe_{attempts,failures,skipped}_total`. GC
публикует gauges полноты сканирования, разрешения удаления, превышения квоты,
ожидающих файлов и числа просмотренных записей; gauges файлов, логических и
выделенных байтов для закрытых классов
`kind={committed,temporary,quarantine,lock,foreign}`; счётчики пропусков,
удалённых файлов и отвязанных логических/выделенных байтов. «Отвязанные
выделенные байты» — значение `st_blocks` открытого inode перед unlink; открытый
дескриптор или другая жёсткая ссылка могут сохранить эти блоки на диске.

Давление cursor registry видно в
`kronika_web_timeline_cursor_views`, `kronika_web_timeline_cursor_bytes` и
`kronika_web_timeline_cursor_pins_total`,
`kronika_web_timeline_cursor_resolves_total`,
`kronika_web_timeline_cursor_evictions_total`,
`kronika_web_timeline_cursor_expired_total` и
`kronika_web_timeline_cursor_capacity_rejections_total`. Активность response
cache и single-flight отражают
`kronika_web_timeline_response_cache_{hits,misses,evictions}_total`,
`kronika_web_timeline_response_cache_{entries,bytes}` и
`kronika_web_timeline_singleflight_{leaders,joins}_total`. Политику выбранных
сегментов показывают `kronika_web_timeline_selected_segments_limit` и
`kronika_web_timeline_query_limit_rejections_total{resource="selected_segments"}`.
Перегрузку холодной загрузки фактов считает
`kronika_web_overview_cold_work_rejections_total{reason="capacity"}`. Наборы
этих labels фиксированы; labels HTTP-запросов используют matched route
templates, а не raw URI.

## Завершение и отказы

`SIGTERM` и `SIGINT` запускают graceful HTTP shutdown. Ошибка refresh
записывается в лог, а последний опубликованный view остаётся доступен; после
заданного порога `/readyz` становится stale. Если store scan успешен, а timeline
build завершается ошибкой, web публикует свежую metadata вместе с последним
пригодным timeline и не показывает частично собранный timeline. Неверная
environment configuration, ошибка первого открытия store/overview или
недоступная энтропия ОС для аутентификации cursor завершают процесс до bind.

У бинарника нет CLI-флагов. MCP, удалённые хранилища, retention исходных
сегментов и доставка алертов не реализованы.
