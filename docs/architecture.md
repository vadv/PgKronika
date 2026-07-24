# Текущая архитектура PgKronika

Документ описывает код в `main`, а не целевую схему будущих релизов. Источники
истины для состава workspace — корневой `Cargo.toml` и
`cargo metadata --no-deps`; для разрешённых зависимостей бинарников —
`xtask/src/main.rs`.

## Процессы

Сейчас работают два пользовательских процесса:

- `pg_kronika-collector` подключается к одному экземпляру PostgreSQL, читает
  PostgreSQL и локальную Linux-систему, пишет `active.parts` и запечатывает
  `.pgm`;
- `pg_kronika-web` читает локальный каталог PGM, отдаёт UI, JSON API,
  Prometheus metrics и bounded analytics. К PostgreSQL он не подключается.

`pg_kronika-archiver` и `pg_kronika-dump` остаются заглушками. Они печатают
сообщение и завершаются с кодом 2. Удалённые хранилища, MCP, retention и
алертинг в workspace отсутствуют.

Коллектор не открывает порт. Связь между процессами идёт только через каталог
данных:

```text
PostgreSQL ─┐
/proc,/sys ─┼─> collector ─> active.parts ─> *.pgm ─> web ─> UI/JSON
stderr log ─┘                         └───────────────> live read
```

## Путь записи

1. `kronika-source-pg` выполняет промаркированные SQL-запросы и преобразует
   версии PostgreSQL/расширений в typed rows. `kronika-source-os` читает
   ограниченные файлы `/proc`, `/sys`, filesystem и cgroup.
   `kronika-source-log` ограниченно читает stderr-журнал и выдаёт typed events
   вместе с gap accounting.
2. `kronika-registry` владеет `type_id`, схемой, sort/identity keys, классами
   колонок, collection gates и Parquet-кодеками. Внутренний proc-macro
   `kronika-derive` генерирует контракт и кодек из одной структуры.
3. Коллектор заканчивает все async-запросы до создания `SectionBuffers` и
   `Interner`. Так не-`Send` writer state не пересекает await, а все
   материализованные результаты остаются под source/cardinality caps.
4. `kronika-writer` кодирует окно в самодостаточную PGM part и добавляет кадр
   `PGMP` в `active.parts`. Append синхронизирует кадр до возврата.
5. Ротация по размеру, возрасту, signal или пределу журнала вызывает `seal`.
   Writer копирует parts в соседний temporary file, пишет концевой каталог,
   синхронизирует файл и публикует его без перезаписи существующего пути.

При обычной ротации имя готового файла — timestamp первого окна в
микросекундах. При восстановлении имя берётся из минимального `catalog.min_ts`
сохранённых частей. Открытый журнал переживает рестарт: оборванный последний
кадр обрезается, корректные кадры запечатываются до нового подключения к
PostgreSQL. Нетерминальная порча остаётся в диагностике. Если восстановленный
журнал невозможно запечатать, коллектор логирует ошибку и сбрасывает его, чтобы
продолжить новый сбор; это явная потеря сохранённых окон, а не бесконечный
startup failure.

## Путь чтения

1. `kronika-store::LocalDir` сначала потоково сканирует `active.parts`, затем
   перечисляет `.pgm`, читая только хвостовой каталог. Эти операции выполняются
   последовательно и не образуют атомарный снимок sealed-файлов и журнала.
   Весь журнал в память не загружается.
2. `kronika-reader::LocalDirSnapshot` открывает полученные sealed units и live
   parts. Точный live part скрывается, если его каталог совпал с уже
   опубликованным sealed unit; одно пересечение по времени для dedup
   недостаточно.
3. `PgmUnit` проверяет framing, catalog и section CRC до передачи Parquet в
   registry codec. Logical query объединяет версии одного section name,
   фильтрует окно, сортирует по контракту и возвращает cursor/gaps.
4. Cumulative columns проходят через `kronika-analytics::diff`. Gauge и diff
   series сохраняют reset, coverage gap, first point, invalid interval и
   `NotCollected` как разные состояния без данных.
5. Web anomaly adapter сканирует окна pure-функциями
   `kronika-analytics::anomaly`, затем incident adapter группирует эпизоды по
   времени и node identity. Активные diagnostic lenses читают typed
   counter-дельты внутри incident-окна, возвращают bounded findings, а
   неподключённые вопросы остаются в dormant catalog. Как читать неполный ответ
   и каталог линз — в
   [incident-analysis.md](incident-analysis.md).

`kronika-analytics::overview` предоставляет типы, редукции, health-оценку и
oracle interface. `kronika-reader::overview` извлекает факты из PGM, хранит
версионированные файлы фактов для отдельных сегментов, выполняет bounded live
fold и сверяет seal handoff. Web пока не публикует live view в production
timeline и не предоставляет overview HTTP endpoints.

Версионированный файл фактов остаётся primary cache. После успешных extraction,
canonical encoding и повторной admission-проверки recoverable publication
failure может оставить `Arc<SegmentFacts>` в process-local LRU, ограниченном
одновременно segment-hours и canonical bytes. Durable lookup всегда выполняется
раньше fallback lookup; ключ включает полный `FactKey` и sealed lineage.
Oversized entry возвращается текущему запросу без сохранения.

Web держит в общей структуре metadata snapshot, а не декодированные section
bodies. Каждый handler клонирует snapshot metadata и выполняет синхронное
чтение на своей копии. Фоновый refresh публикует новую копию раз в секунду.

## Пакеты и ответственность

| Пакет | Владеет | Не владеет |
| --- | --- | --- |
| `kronika-format` | Byte layout PGM/PGMP, catalog, CRC, dictionary model | Parquet schema, storage policy, domain meaning |
| `kronika-derive` | Генерация внутреннего `Section` impl | Публичный extension API |
| `kronika-registry` | Type contracts, codecs, version/gate semantics | Сбор, storage, запросы |
| `kronika-writer` | Буферы, interner, journal, seal | Источники и чтение |
| `kronika-store` | Read-only local listing и journal scan | Decode секций и remote backends |
| `kronika-reader` | Decode, snapshot, pagination, logical/gauge/diff query, overview extraction и local fact store | HTTP и PostgreSQL |
| `kronika-analytics` | Pure diff, anomaly и overview contract kernels | PGM extraction, persistence, HTTP и PostgreSQL behavior |
| `kronika-source-pg` | PostgreSQL SQL и mapping | Writer, filesystem и HTTP |
| `kronika-source-os` | Linux parsing и bounded reads | Scheduling и segment state |
| `kronika-source-log` | Tail state, stderr parser, typed log rows | Registry codec и writer |
| `pg_kronika-collector` | Lifecycle, scheduling, budgets, coverage, rotation | Read API и remote upload |
| `pg_kronika-web` | HTTP/auth/readiness, response contracts, incident domain | Collection и root-cause attribution |
| `kronika-bdd` | Live matrix harness и domain assertions | Production runtime |
| `xtask` | Dependency allow lists | Release packaging |

## Проверяемые границы зависимостей

`cargo run -p xtask -- check-deps` строит workspace graph из Cargo metadata и
проверяет транзитивные allow lists:

```text
collector -> format, derive, registry, writer, source-pg, source-os, source-log
web       -> format, derive, registry, store, reader, analytics
archiver  -> format, store
dump      -> format, derive, registry, store, reader, analytics
```

Поэтому PostgreSQL client и `/proc` readers не попадают в web, а reader/store —
в collector. Allow lists для заглушек фиксируют будущую границу, но не означают,
что их функции уже реализованы.

## Формат и data quality

Контейнерная версия сейчас одна: `FORMAT_VERSION = 1`. Reader отклоняет другую
версию; обещания читать все будущие или прошлые версии нет. Несовместимые схемы
данных получают новый `type_id`, а logical section объединяет зарегистрированные
версии по имени.

Основные hard limits:

- section body — не более 8 MiB, 65 536 rows и 16 Parquet row groups;
- local catalog — не более 64 MiB;
- default part — не более 64 MiB, journal — 1 GiB;
- ordinary reader query — не более 10 000 000 materialized cells;
- web diff — не более 262 144 rows за окно;
- incident adapter — request-wide ceilings на units, sections, cells, points,
  identity bytes, positions, score work и episodes;
- одновременно выполняется один тяжёлый anomaly/incident request.

Source-specific top-N и byte caps проверяются до interning/encoding. Когда
источник прочитан частично, coverage rows, gaps и skipped reasons сохраняют
причину. Отсутствие, reset и выключенная настройка сбора не кодируются нулём.

## Security boundary

Коллектор имеет доступ к PostgreSQL, `/proc`, cgroup и при настройке к журналу.
Он не содержит HTTP server или storage credentials. Web имеет доступ только к
каталогу сегментов и сети; в нём нет PostgreSQL client.

PGM применяет CRC для случайной порчи, но не подпись и не шифрование. SQL,
планы, имена объектов, аргументы процессов и log text считаются чувствительными
данными. Доступ к каталогу задаётся filesystem permissions. Web не реализует
TLS; встроенный Basic Auth нужно использовать только через защищённый транспорт
или на loopback. Probes и `/metrics` остаются публичными.

## Версионирование и зрелость

Все packages имеют workspace version `0.0.0`, не публикуются на crates.io и
собираются атомарно. Release archive и compatibility policy ещё не выпущены.
CI проверяет Rust на GNU target и BDD PostgreSQL 15–18 в Docker/Nix на
`linux/amd64`; repository default для release build —
`x86_64-unknown-linux-musl`.

Исторические specs в `docs/superpowers/` и ранние design notes фиксируют
решения на момент отдельных PR. Они не заменяют current manifests, crate
contracts и этот документ.
