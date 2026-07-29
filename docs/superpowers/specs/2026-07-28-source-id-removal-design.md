# PgKronika — полное удаление `source_id`

Версия: 0.1  
Дата: 2026-07-28  
Статус: согласованный проект

## 1. Цель

PgKronika удаляет глобальную сущность `source_id` из конфигурации, форматов PGM
и OVF, хранилища, аналитики и HTTP API.

Один каталог данных представляет один поток наблюдений одного PostgreSQL.
Для выбора этого потока оператор задаёт каталог данных и параметры подключения.
Отдельный идентификатор источника не настраивается и не вычисляется
автоматически.

## 2. Причина изменения

Текущее поле `source_id` не имеет единственного реализованного источника истины.
Документация формата описывает его как идентификатор
`{cluster_id}/{pg_system_identifier}`, а коллектор фактически принимает
произвольное число из `KRONIKA_SOURCE_ID` и использует `0` как отсутствие
значения.

Поле создаёт ложную поддержку нескольких PostgreSQL в одном каталоге данных:

- writer допускает и объединяет нулевые значения;
- reader группирует и фильтрует units по числу;
- OVF включает число в lineage, ключи фактов и идентичности сущностей;
- HTTP требует `source` почти во всех запросах и возвращает списки источников;
- demo не задаёт переменную и поэтому публикует `source_id=0`.

При этом физическое хранилище, активный journal и жизненный цикл collector
рассчитаны на единственного владельца каталога. Число не доказывает
происхождение данных и не делает смешанный каталог корректным.

## 3. Инварианты

После изменения действуют следующие требования:

1. Один data root принадлежит одному collector и одному наблюдаемому PostgreSQL.
2. В одном data root нельзя смешивать PGM, journals или OVF разных PostgreSQL.
3. Collector не принимает и не создаёт глобальный идентификатор источника.
4. Reader рассматривает все units опубликованного snapshot как один временной
   ряд.
5. HTTP API работает с единственным data root без выбора источника.
6. Локальные идентичности фактов выводятся из содержимого, физического
   происхождения и версий контрактов, а не из операторского числа.
7. Старые PGM, journals и OVF не читаются и не преобразуются.

Если в будущем понадобится объединять несколько установок PgKronika, федерация
получит собственную идентичность узла на внешнем уровне. Она не должна
возвращать `source_id` в локальный формат.

## 4. Границы терминологии

Удаляется поле `source_id`, обозначающее PostgreSQL или collector.

`source_type_id` сейчас обозначает физический тип PGM-секции, а не PostgreSQL.
Чтобы исключить повторную двусмысленность, оно переименовывается в
`section_type_id`. Числовой `type_id` реестра секций и стабильные
идентификаторы реальных сущностей PostgreSQL сохраняются.

Имена `SourceDescriptor` и `source_file_len` в OVF относятся к точному входному
PGM-файлу. Они не задают пространство нескольких источников и могут сохраниться.

## 5. Конфигурация collector

Из collector удаляются:

- переменная окружения `KRONIKA_SOURCE_ID`;
- поле `Config::source_id`;
- передача числа в writer;
- поле `source_id` в диагностических событиях flush и seal;
- описание параметра в английской и русской документации.

Отсутствие `KRONIKA_SOURCE_ID` не является ошибкой и не требует значения по
умолчанию. Неизвестная переменная окружения не влияет на запуск collector.

## 6. Формат PGM

### 6.1 Каталог

`source_id: u64` удаляется из `Catalog` и `CatalogView`. Метаданные каталога
уменьшаются с 40 до 32 байт:

```text
metadata: 32 B
  min_ts          i64
  max_ts          i64
  entry_count     u32
  format_version  u32
  crc32c          u32
  window_count    u32
```

CRC вычисляется по новому 32-байтовому представлению с обнулённым полем
`crc32c`.

### 6.2 Journal и seal

Journal parts больше не переносят `source_id` через вложенный каталог. Seal:

- не вычисляет общий идентификатор;
- не допускает нулевой sentinel;
- не проверяет конфликт идентификаторов между parts;
- записывает новый каталог без этого поля.

Публичные функции writer больше не принимают `source_id`.

### 6.3 Версия

Числовая версия PGM изменяется вместе с layout. Decoder поддерживает только
текущий layout. Отдельный compatibility decoder и миграция не добавляются.

## 7. Snapshot и query

Из `CatalogSummary`, `UnitMeta`, `SegmentDescriptor`, live-state и связанных
структур удаляется `source_id`.

Snapshot:

- сохраняет единый порядок units по времени и locator;
- не группирует units по источнику;
- не фильтрует units по источнику;
- не проверяет совпадение источника между sealed и live units;
- не публикует source summaries.

Функции `source_summaries` и связанные лимиты и ошибки удаляются. Section,
diff, batch и latest queries выбирают данные только по времени, имени секции,
cursor и действующим ограничениям работы.

## 8. Формат OVF и аналитические идентичности

### 8.1 Header

`pgm_source_id: u64` удаляется из `HeaderIdentity`. Фиксированный OVF header
уменьшается на 8 байт. Смещения, длина header и CRC пересчитываются для нового
layout.

OVF admission связывает sidecar с PGM по следующим данным:

- версии формата и аналитических контрактов;
- `source_min_ts_us` и `source_max_ts_us`;
- `source_file_len`;
- content-derived `SourceDescriptor`;
- `FactKey`;
- `SegmentLineageId`.

Отдельная проверка числового источника отсутствует.

### 8.2 Ключ фактов и lineage

`FactKey` выводится из:

- точного PGM descriptor;
- вида fact-файла;
- версии схемы фактов;
- версии extractor semantics;
- версии registry contract.

`SegmentLineageId` для sealed unit выводится из PGM descriptor. Для live unit
он выводится из journal generation и descriptor первого part. Числовая
квалификация не используется.

### 8.3 Факты и серии

Из сериализованных OVF-блоков и in-memory моделей удаляется `source_id`:

- coverage и event facts;
- metric series descriptors;
- entity и alignment derivation;
- reset boundaries;
- manifests и live fold;
- web summary и entity series, где поле могло участвовать транзитивно.

`source_type_id` переименовывается в `section_type_id`. Идентичность series
включает `section_type_id`, factor, entity и реальный discriminator.

Изменение wire-layout OVF сопровождается изменением его внутренних version
constants. Reader принимает только новый layout; старые sidecars не
перестраиваются на месте.

## 9. Аналитика

Overview, events, health, anomalies и incidents работают над единственным
выбранным временным диапазоном.

Удаляются:

- разбиение диапазона по источникам;
- source-set hash;
- source-qualified entity scope;
- source в evidence, incident keys, ETag и cache keys;
- проверки одинакового источника у границ метрик и фактов.

Связь incident entities использует доказанную идентичность сущности. Например,
узел PostgreSQL связывается по непустому `node_self_id`, а не по паре
`(source_id, node_self_id)`.

## 10. HTTP API

### 10.1 Маршруты и параметры

Маршрут `GET /v1/sources` удаляется.

Параметр `source` удаляется из всех endpoint:

- `/v1/timeline/overview`;
- `/v1/timeline/events`;
- `/v1/timeline/health`;
- `/v1/timeline/heatmap`;
- `/v1/anomalies`;
- `/v1/incidents`;
- `/v1/ui/catalog`;
- `/v1/views/summary`;
- `/v1/sections`;
- `/v1/segments`;
- `/v1/section/{name}`;
- `/v1/section/{name}/diff`;
- `/v1/sections/batch`;
- `/v1/sections/batch/diff`.

Переданный `source` считается неизвестным query-параметром. Специальная ошибка
`unknown_source` удаляется.

### 10.2 Ответы

Из всех JSON-ответов удаляются:

- `source_id`;
- `sources`;
- `available_sources`;
- per-source массивы freshness, status и loss.

Timeline metadata содержит единичные свойства data root:

- `data_through_us`;
- `store_data_through_us`;
- `tail_pending`;
- `status`;
- `freshness`;
- `loss`.

`freshness` и `loss` являются объектами без идентификатора, а не одноэлементными
массивами. Вложенные поля `source_status` и `source_completeness`
переименовываются в `status` и `completeness`.

Event, anomaly и incident DTO не содержат глобальный идентификатор. Cursor и
ETag не кодируют source или source-set hash.

Breaking change отражается в `response_schema_version`. Путь `/v1` сохраняется:
он обозначает семейство API, а не совместимость конкретной формы ответа.

## 11. CLI и документация

`pg_kronika-dump` не печатает `source_id` для PGM или OVF. Определение типа
файла по имени и требование `--rows` для содержимого web-индекса сохраняются.

Обновляются:

- README collector, reader, format и web на двух языках;
- примеры запросов;
- BDD harness и fixtures;
- активные спецификации, на которые ссылается пользовательская документация.

Исторические планы не являются контрактом текущей версии. Их не переписывают,
если они не используются как действующая справка.

## 12. Совместимость и данные

Совместимость со старыми файлами отсутствует намеренно:

- старые PGM и journals не принимаются новым reader;
- старые OVF не принимаются и не мигрируют;
- смешанный каталог старых и новых файлов не поддерживается;
- demo-data после обновления создаётся заново.

Поскольку потребителей и production-архива нет, migration command, fallback и
reserved zero fields не добавляются.

## 13. Ошибки и наблюдаемость

Ошибки конфликта или отсутствия source удаляются. Повреждение layout, CRC,
версии или descriptor продолжает давать действующую bounded ошибку формата.

Метрики и логи не получают константную label `source_id=0`. Остальные labels
сохраняют действующие ограничения кардинальности.

## 14. Критерии приёмки

Изменение принято, когда выполнены все условия:

1. Production-код не содержит глобального поля или параметра `source_id`;
   локальные имена, создающие ту же двусмысленность, переименованы.
2. Collector не читает `KRONIKA_SOURCE_ID`.
3. Публичный HTTP router не содержит `/v1/sources`.
4. `source` отклоняется как неизвестный query-параметр.
5. JSON schemas и ответы не содержат `source_id` или списков источников.
6. Новый PGM catalog занимает 32 байта metadata и проходит CRC/property tests.
7. OVF header и блоки не сериализуют глобальный идентификатор.
8. Reader, writer, analytics, web, dump и BDD tests используют single-root
   модель.
9. Golden и property tests доказывают отказ от старого layout.
10. Workspace проходит format, lint и test проверки CI.

Дополнительная статическая проверка выполняет поиск `source_id`,
`KRONIKA_SOURCE_ID`, `/v1/sources` и `QueryParameter::Source` в production-коде
и действующей документации. Каждый оставшийся результат должен относиться к
историческому документу либо быть устранён.
