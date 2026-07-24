# kronika-reader

[English version](README.md)

`kronika-reader` проверяет и декодирует локальные PGM units, строит snapshot из
готовых файлов и живых parts, затем выполняет ограниченные logical queries для
`pg_kronika-web`.

## Units и snapshots

`PgmUnit<R: ReadAt>` объединяет decode готового `File` и in-memory active part.
Он сначала открывает концевой каталог, проверяет format version и границы,
читает тело по требованию, сверяет CRC и только затем вызывает registry codec.
`Segment` — удобная оболочка для готового файла.

`LocalDirSnapshot` последовательно получает список sealed units и сканирует
`active.parts` через `kronika-store`; эти операции не образуют единый атомарный
снимок. Сначала идут sealed units, затем live parts. Live part скрывается только
при точном совпадении каталога с sealed unit; пересечения по времени
недостаточно. Store warnings и damage regions доступны вызывающему коду.

После создания snapshot writer может запечатать или сбросить `active.parts`.
Изменившаяся ссылка возвращает `ReadError::StaleSnapshot`. Query helpers
ограниченно повторяют refresh, после чего нестабильный unit отражается gap.

`LiveBuilder`, `LiveView` и seal reconciliation предоставляют ограниченные
примитивы для overview fold и handoff. `pg_kronika-web` пока не публикует этот
live timeline: production-запросы по-прежнему обращаются к `LocalDirSnapshot`.

## Logical queries

`logical_section(name)` объединяет версии layout с общим именем. Запрос:

1. выбирает `source_id` и пересекающийся временной диапазон;
2. декодирует только нужные entries и словари;
3. объединяет колонки версий и разрешает строки;
4. сортирует по registry contract;
5. возвращает gaps и непрозрачный cursor.

`section` и `sections` принимают row limit и всегда соблюдают жёсткий потолок
10 000 000 materialized cells. Варианты `*_with_limits` позволяют adapter
задать меньший общий бюджет. `QueryError::ResultTooLarge` возникает до
удержания следующей лишней строки.

Cursor фиксирует последний ключ и источник. Неверный или взятый у другого
source cursor отклоняется, а не используется как offset.

## Gauge и counter semantics

`gauge_section` группирует gauge samples по identity. `diff_section`
обрабатывает cumulative columns через `kronika-analytics`, сохраняя точный
целочисленный delta и реальный интервал.

Состояния без данных различаются:

- `FirstPoint` — начало ряда или первая точка после разрыва;
- `Reset` — уменьшение cumulative value или продвижение reset metadata;
- `Gap` — отсутствие покрытия между samples;
- `NotCollected` — выключенный или неизвестный collection gate;
- `Anomaly` — неверное время или несовместимый scalar.

Неизменившийся измеренный счётчик даёт нулевые delta и rate. Diff не соединяет
ряд через no-data и не экстраполирует пропущенное время.

## Файлы фактов overview

`source_scope_id`, `SourceDescriptor`, `section_body_id` и
`dictionary_context_id` выводят типизированные идентификаторы содержимого из
точных метаданных PGM и сохранённых значений. `PgmUnit::read_overview_section`
читает секцию по позиции в каталоге и проверяет её CRC.
`PgmUnit::resolve_overview_dictionary` читает только `dict.strings` и
`dict.blobs`, сохраняет запрошенные ID и возвращает счётчики прочитанных и
декодированных данных.

`FactFile::build` записывает канонический контейнер PGKOVF. `FactFile::admit`
проверяет весь контейнер: физическую разметку, контрольные суммы, суммарные
пределы, логическое содержимое блоков, происхождение данных и ссылки на строки.
`FactFileReader` сначала читает заголовок и каталог, затем проверяет CRC только
у выбранных тел блоков. `FactReadStats` сообщает число чтений и объём данных.

Все конструкторы и декодеры PGKOVF применяют абсолютные пределы `LIMIT` до
крупных аллокаций. `FactStore` загружает и проверяет версионированные файлы
фактов для отдельных сегментов. При отсутствии или отклонении файла крейт
ограниченно извлекает факты из PGM, после чего store публикует их по content
key. Ошибка сохранения остаётся видна вместе со свежими извлечёнными фактами.

## Границы и отказы

Каталог ограничен 64 MiB. Registry принимает не более 8 MiB, 65 536 rows и 16
Parquet row groups на секцию. Декод словаря использует те же row/row-group
guards. Ошибки разделяют I/O, framing, unsupported format, bounds, CRC/codec,
storage и staleness.

Крейт не владеет HTTP status mapping, remote storage, anomaly budget или
поведением PostgreSQL. Канонический public surface — [`src/lib.rs`](src/lib.rs).
