# Полнота сбора секций: точный статус `N/M` без повторного прохода

Дата: 2026-07-30.

Статус: решения согласованы, ветка rebased на `main` с PR #146; письменная
спецификация готова к review перед планом реализации.

## Цель

Для `statements`, `plans`, `tables` и `indexes` ручка
`GET /v1/views/summary` должна возвращать:

- фактическое число записанных строк `collected`;
- фактическое число строк источника `source_total`, если collector смог его
  посчитать;
- результат чтения `read_state`;
- видимость источника `visibility`.

Статус `tables` и `indexes` описывает сумму по всем подключённым базам данных.
Будущий web-клиент сможет показать `500/4.8K`, `1.1K/1.9K` или одно число для
полного снимка. Оценки, нижние границы и приблизительные totals в публичный
контракт не входят.

Два свойства имеют одинаковый приоритет:

1. Collector создаёт минимальную дополнительную нагрузку на PostgreSQL.
2. Web читает статус быстро, без последовательного сканирования PGM-секций.

## Что уже есть

Формат полноты состоит из двух секций:

- `SnapshotCoverageV1` (`type_id=1_038_001`) хранит
  `section_type_id`, `collected`, `source_total`, `read_state` и `visibility`;
- `CollectionCoverageV1` (`type_id=1_023_001`) объясняет top-N и ошибки,
  хранит `unknown_total`, `max_n`, `order_by`, `cutoff_value` и `reason`.

Collector уже пишет `SnapshotCoverageV1` для `statements` и обоих вариантов
`plans`. Для `tables` и `indexes` collector сейчас пишет только
`CollectionCoverageV1` и только при усечении или ошибке.

Reader умеет разбирать обе coverage-секции для аналитических факторов, но этот
путь не обслуживает статусы UI-view. `UiSummaryBlock` revision 1 хранит только
времена снимков, population, notable и статус индекса.

`GET /v1/views/summary` уже работает только поверх OVF `UiSummary`. Ручка не
декодирует исходные PGM-секции, и это ограничение сохраняется.

## Подтверждённая проблема нагрузки

Текущие SQL-запросы повторно обходят источники:

- `statements_query` отдельно обращается к `pg_stat_statements` для каждой оси
  candidate selection, финальной выборки и `count(*)`;
- vadv `pg_store_plans` вызывается для top-N и ещё раз для `count(*)`;
- ossc `pg_store_plans` вызывается для top-N и ещё раз для `count(*)`;
- запросы `tables` и `indexes` повторно читают statistics view из candidate
  selection, финальной выборки и `count(*)`.

`pg_stat_statements` хранит тексты запросов во внешнем файле. Вызов
`pg_stat_statements(false)` пропускает тексты, но отдельный count всё равно
добавляет проход по hash table.

OSSC `pg_store_plans` 1.x не имеет аргумента `showtext`. Его SRF материализует
полный набор, загружает файл планов и только после этого отдаёт строки внешнему
SQL. Поэтому приём `LIMIT N+1` не устраняет основной полный проход и не даёт
достаточного выигрыша, чтобы отказаться от фактического `M`.

## Решение: один materialized source

Каждый запрос строит один `MATERIALIZED` CTE `source`. Точный total вычисляется
оконной функцией над тем же набором:

```sql
WITH source AS MATERIALIZED (
  SELECT s.*, count(*) OVER ()::int8 AS source_total
  FROM <source> s
)
SELECT ...
FROM source s
...
```

Candidate selection и финальная выборка обращаются только к `source`.
Скалярных подзапросов `(SELECT count(*) FROM <source>)` не остаётся.
`MATERIALIZED` запрещает planner размножить вызов SRF при нескольких ссылках на
CTE.

Пустой `source` даёт фактический total `0`: отсутствие первой строки результата
трактуется как успешный пустой набор только после успешного выполнения запроса.

### Statements

`source` один раз материализует `pg_stat_statements` вместе с текстами запросов и
`source_total`. Обе оси top-N (`total_exec_time`/`total_time` и `calls`) читают
этот CTE.

Такой запрос загружает тексты один раз. Отдельный
`pg_stat_statements(false)` для count не нужен: он сохранил бы второй проход,
хотя точный total уже получается в единственной материализации.

### Plans vadv

`source` материализует `pg_store_plans(false)` и считает
`count(*) OVER ()`. Тексты выбранных планов по-прежнему загружаются отдельно
через `pg_store_plans_get_plan` под существующими лимитами времени и памяти.

### Plans ossc

`source` материализует `pg_store_plans` один раз. `count(*) OVER ()` возвращает
точное число записей из этой материализации. Внешние `ORDER BY total_time` и
`LIMIT` выбирают top-N, не вызывая SRF повторно.

Если ossc скрывает identity чужих строк, `source_total` всё равно равен
фактическому числу строк SRF. Collector исключает строки без identity из
`collected`, выставляет `read_state=permission` и
`visibility=restricted`, но сохраняет точный `N/M`.

### Tables и indexes

Каждый запрос к базе данных материализует соответствующий statistics view один
раз, считает `count(*) OVER ()` и использует CTE для всех осей top-N.

Collector передаёт во все запросы одного цикла общий `cycle_ts_us`:

```sql
$2::int8 AS ts_us
```

Локальный `statement_timestamp()` больше не создаёт отдельный snapshot для
каждой базы данных. Строки всех баз одного цикла получают общий ключ, поэтому
`UiSummary` складывает их population, а coverage описывает тот же snapshot.
Значения строк и counts остаются фактическими; общим становится только
идентификатор цикла сбора.

## Coverage collector

Collector формирует `SnapshotCoverageV1` для всех четырёх view. Marker
записывается для каждой начатой попытки, если collector уже определил физический
`section_type_id`, включая попытки с permission error и read failure:

| View | `section_type_id` | Область counts |
| --- | --- | --- |
| `statements` | активная версия `1_002_xxx` | instance-wide SRF |
| `plans` | `1_003_001` или `1_004_001` | instance-wide SRF |
| `tables` | активная версия `1_013_xxx` | сумма по всем базам |
| `indexes` | активная версия `1_014_xxx` | сумма по всем базам |

`SourceCoverage` остаётся внутренним накопителем. Он определяет состояние
попытки:

| Условие | `read_state` | `visibility` | Публичный `source_total` |
| --- | --- | --- | --- |
| `collected == source_total`, ошибок нет | `complete` | `full` | точное число |
| `collected < source_total`, ошибок нет | `source_limit` | `full` | точное число |
| ossc вернул masked rows | `permission` | `restricted` | точное число |
| SQL permission error не позволил прочитать источник | `permission` | `restricted` | `null` |
| timeout или другая ошибка не позволили прочитать источник | `read_failure` | `unknown` | `null` |
| collector потерял строки или вышел за форматный предел | `collector_limit_or_loss` | `unknown` | `null` |

Если часть баз не прочитана, `collected` остаётся фактической суммой записанных
строк успешных баз. Известная часть total может сохраниться во внутреннем
coverage, но API не выдаёт её как `source_total`.

Если одна попытка содержит несколько причин, состояние выбирается по приоритету:
`collector_limit_or_loss`, `read_failure`, `permission`, `source_limit`,
`complete`. `visibility=restricted` применяется только когда все известные
ограничения вызваны правами; при read failure или collector loss видимость
считается `unknown`.

`SnapshotCoverageV1` и `CollectionCoverageV1` используют `u32`. Перед
преобразованием collector проверяет переполнение. Насыщенное значение
`u32::MAX` нельзя публиковать как точный total: попытка получает
`collector_limit_or_loss`, а API возвращает `source_total=null`.

Reader объединяет `SnapshotCoverageV1` и `CollectionCoverageV1` по
`(section_type_id, ts)`. `unknown_total=true` запрещает публиковать total.
Совпадающие записи дополняют друг друга, а не считаются дубликатами только при
побайтовом равенстве внутренних представлений.

## UiSummary revision 2

`UiSummaryBlock` revision 2 добавляет к каждому `ViewSummary` collection status
на общей таблице времён:

- presence mask для collection status;
- `collected`;
- nullable exact `source_total`;
- `read_state`;
- `visibility`.

Coverage физической секции сопоставляется с view через первый `WebInput`:

- `pg_stat_statements` -> `statements`;
- `pg_store_plans_ossc` или `pg_store_plans_vadv` -> `plans`;
- `pg_stat_user_tables` -> `tables`;
- `pg_stat_user_indexes` -> `indexes`.

Revision 1 продолжает декодироваться. Для старого блока collection status
отсутствует. OVF — производный индекс, поэтому новые или перестроенные блоки
записываются только в revision 2; миграция PGM не требуется.

## Быстрый путь чтения

`GET /v1/views/summary` читает только:

1. metadata локального OVF directory;
2. адресуемый `UiSummaryBlock` для подходящего временного диапазона.

Ручка не читает:

- PGM body;
- dictionary;
- исходные `snapshot_coverage` или `collection_coverage`;
- `EntitySeries`;
- строки `statements`, `plans`, `tables` или `indexes`.

Стоимость запроса не зависит от числа строк PostgreSQL-источников. Размер
`UiSummary` остаётся ограничен существующими bounds на views, timestamps и
decoded bytes. Поиск по нескольким сегментам использует только маленькие
summary-блоки и существующую directory metadata, а не последовательный scan
PGM-файлов.

## API

Существующее поле `population` сохраняется. Оно описывает число строк
индексированного UI-view и не подменяет collection status.

Каждый элемент `views` получает nullable поле `collection`:

```json
{
  "view": "statements",
  "snapshot_ts_us": "1000000",
  "population": 500,
  "status": "complete",
  "notable": true,
  "collection": {
    "collected": 500,
    "source_total": 4800,
    "read_state": "source_limit",
    "visibility": "full"
  }
}
```

`source_total` nullable и содержит только фактическое полное число:

```json
{
  "collected": 320,
  "source_total": null,
  "read_state": "read_failure",
  "visibility": "unknown"
}
```

Поля `total_quality`, lower bound и оценочный total в этот API не добавляются.
Для revision 1, старого PGM без достаточного provenance и недоступного view
`collection=null`.

Строковые значения:

- `read_state`: `complete`, `source_limit`, `permission`, `read_failure`,
  `collector_limit_or_loss`;
- `visibility`: `full`, `restricted`, `unknown`.

## Будущий web-клиент

Форматирование чисел не входит в этот PR. Следующий consumer сможет отображать:

- `N`, если `read_state=complete` и `N == M`;
- `N/M`, если оба фактических числа доступны;
- только `N` и индикатор ошибки, если `source_total=null`.

Компактные формы `4.8K` и `1.1K` принадлежат web-клиенту. API возвращает целые
числа без округления.

## Инварианты и ошибки

- `collected` и ненулевой `source_total` не превышают форматные bounds.
- При `read_state=complete` выполняется `collected == source_total`.
- При `read_state=source_limit` выполняется `collected < source_total`.
- Если collection status присутствует, для того же view и cycle timestamp
  выполняется `population == collection.collected`.
- `source_total` присутствует в API только при доказанном точном count.
- Ошибка одной базы не превращает известную часть total в полный total.
- Два противоречащих coverage-факта для одного `(section_type_id, ts)` означают
  повреждённый источник и не публикуются.
- Неизвестная revision `UiSummary` отклоняется; revision 1 и 2 читаются явно.
- Ошибка collection status не превращает успешный bounded summary response в
  transport error, если состояние можно вернуть как `collection` с
  `read_state`.

## Тестирование

### SQL и source-pg

- Каждый запрос содержит `MATERIALIZED` и `count(*) OVER ()`.
- SQL не содержит скалярного `count(*)` по исходному SRF или view.
- Каждый исходный SRF или statistics view встречается в запросе один раз.
- Две оси statements/tables/indexes читают только CTE `source`.
- vadv использует `pg_store_plans(false)`.
- ossc использует один `pg_store_plans` и возвращает exact total.
- tables/indexes используют переданный `cycle_ts_us`.

### Collector

- Несколько баз одного цикла получают одинаковый timestamp.
- Counts tables/indexes суммируются по всем успешным базам.
- Complete, source limit, permission, read failure и overflow дают
  согласованные `SnapshotCoverageV1` и `CollectionCoverageV1`.
- Masked ossc rows входят в exact `source_total`, но не в `collected`.

### Reader и OVF

- `UiSummary` revision 2 round-trip сохраняет collection status.
- Revision 1 декодируется с `collection=None`.
- Coverage четырёх физических секций попадает в правильные view.
- Несогласованные counts, states и дубликаты отклоняются.
- Bounds учитывают память новых masks и значений.

### API и бюджет чтения

- `/v1/views/summary` отдаёт `N/M`-данные для всех четырёх view.
- При ошибке total равен `null`, а `collected` остаётся фактическим.
- OpenAPI описывает nullable `collection` и `source_total`.
- Consumer-test доказывает отсутствие PGM body reads, dictionary reads и
  `EntitySeries` reads при запросе summary.
- Cold request читает только directory metadata и bounded `UiSummary` blocks.

## Не цели

- Frontend и форматирование `K/M`.
- Оценки, `N+`, lower bounds и approximate totals в web API.
- Новая версия PGM coverage-секций.
- Фильтр collection status по одной базе данных.
- Полный scan PGM при обработке web-запроса.

## Порядок реализации

1. Перестроить SQL четырёх source families на один materialized source и exact
   window count.
2. Ввести общий `cycle_ts_us` для tables/indexes и coverage всех четырёх view.
3. Научить reader канонически объединять две coverage-секции.
4. Добавить collection status в `UiSummary` revision 2 с чтением revision 1.
5. Расширить `/v1/views/summary` и OpenAPI.
6. Закрепить отсутствие PGM scan и фактические counts consumer- и
   qualification-тестами.
