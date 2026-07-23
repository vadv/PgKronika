# PgKronika

[English version](README.md)

PgKronika сохраняет диагностическую историю экземпляра PostgreSQL в локальных
неизменяемых файлах PGM. Коллектор читает статистику PostgreSQL, данные Linux
из `/proc` и cgroup, а при настройке — stderr-журнал PostgreSQL. Отдельный
web-процесс показывает записанные строки, разницы счётчиков, эпизоды аномалий и
кластеры инцидентов через локальный интерфейс и JSON API.

Проект активно развивается. Коллектор, локальное хранилище сегментов, reader и
web API реализованы и проверяются BDD-матрицей PostgreSQL 15–18. Готовой
поставки, управления retention, удалённой архивации, `pg_kronika-dump`, MCP,
алертинга и определения первопричины пока нет.

## Путь данных

```text
PostgreSQL 15–18       Linux /proc, /sys, cgroups       stderr log
        \                     |                           /
         kronika-source-pg / kronika-source-os / kronika-source-log
                              |
                 kronika-registry + kronika-derive
                              |
                kronika-writer -> active.parts -> *.pgm
                              |
                 kronika-store -> kronika-reader
                              |
             kronika-analytics -> pg_kronika-web
                    diff, anomaly       JSON/UI, incidents
```

Коллектор работает на сервере базы данных и не открывает сетевой порт. Он пишет
добавочный журнал `active.parts`, а затем запечатывает его кадры в
самодостаточные файлы `.pgm`. Web-процесс читает из того же каталога готовые
файлы и корректные части живого журнала, не подключаясь к PostgreSQL.

PgKronika полезна для сохранения подробных высококардинальных данных рядом с
базой: сессий, запросов, планов, статистики отношений, репликации, давления ОС,
счётчиков процессов и cgroup, типизированных событий журнала. Она не заменяет
систему метрик и алертинга и не выдаёт корреляцию за причину инцидента.

## Поддержка и требования

| Область | Текущий контракт |
| --- | --- |
| PostgreSQL | BDD-матрица запускается на версиях 15, 16, 17 и 18. |
| Платформа | Linux. CI и BDD image сейчас проверяют `x86_64`. |
| Rust | Rust 1.96.0, зафиксированный в [`rust-toolchain.toml`](rust-toolchain.toml). |
| Сборка для разработки | GNU target; команда ниже совпадает с target в CI. |
| Release-сборка | По умолчанию репозиторий использует `x86_64-unknown-linux-musl`; нужны `musl-gcc` и Rust target. Готового release bundle пока нет. |
| Доступ к PostgreSQL | Login-роль с `CONNECT` к нужным базам. `pg_monitor` или более узкие эквивалентные права открывают статистику чужих сессий. |
| Расширения | Необязательны. `pg_stat_statements` и поддерживаемый вариант `pg_store_plans` обнаруживаются в базе установки; без них основной сбор продолжает работать. |

Коллектор перечисляет доступные нетемплейтные базы и держит не более 20
подключений для per-database источников. База в DSN задаёт начальное соединение,
а не режим сбора одной базы. Права и поведение расширений описаны в
[контракте сбора PostgreSQL](docs/type-registry/postgresql-collection.md).

## Кратчайший путь от сборки до данных

Команды ниже создают сегмент, открывают его web-процессом и возвращают первый
полезный результат API. Запускайте их из корня репозитория на Linux.

Соберите два реализованных пользовательских бинарника:

```sh
rustup target add x86_64-unknown-linux-gnu --toolchain 1.96.0
cargo +1.96.0 build --locked --target x86_64-unknown-linux-gnu \
  -p pg_kronika-collector -p pg_kronika-web
mkdir -p var/segments
```

Запустите коллектор, подставив DSN доступной PostgreSQL 15–18 и отдельной роли.
`KRONIKA_SEGMENT_MAX_BYTES=0` запечатывает каждое окно сбора, поэтому первый
результат появляется сразу.

```sh
KRONIKA_PG_DSN='host=127.0.0.1 port=5432 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR="$PWD/var/segments" \
KRONIKA_SEGMENT_MAX_BYTES=0 \
./target/x86_64-unknown-linux-gnu/debug/pg_kronika-collector
```

После успешного окна процесс печатает `ready`, затем
`sealed <path> reason=tick`. Ошибка отдельного цикла попадает в stderr, а демон
продолжает работу и повторяет сбор позже.

Во втором терминале откройте каталог на loopback-адресе:

```sh
KRONIKA_WEB_DIR="$PWD/var/segments" \
KRONIKA_WEB_ADDR=127.0.0.1:8688 \
./target/x86_64-unknown-linux-gnu/debug/pg_kronika-web
```

Получите список источников и доступных секций:

```sh
curl -sS http://127.0.0.1:8688/v1/sources
curl -sS http://127.0.0.1:8688/v1/sections
```

Встроенный интерфейс доступен на `http://127.0.0.1:8688/`. TLS в сервер не
встроен. Без `KRONIKA_WEB_BASIC_AUTH` интерфейс и `/v1/*` открыты;
`/healthz`, `/readyz` и `/metrics` остаются публичными даже при включённом
Basic Auth.

## Карта workspace

Все пакеты внутренние, имеют общую версию и не публикуются на crates.io.

| Пакет | Ответственность |
| --- | --- |
| [`kronika-format`](crates/kronika-format/) | Кадры PGM, каталог, CRC32C, словари и проверка кадров журнала. |
| [`kronika-derive`](crates/kronika-derive/) | Внутренний derive `Section`, создающий контракт реестра и Parquet-кодек. |
| [`kronika-registry`](crates/kronika-registry/) | Стабильные type id, схемы, семантика колонок, gates, кодеки и linter реестра. |
| [`kronika-writer`](crates/kronika-writer/) | Ограниченные буферы секций, интернер строк, `active.parts` и запечатывание. |
| [`kronika-store`](crates/kronika-store/) | Read-only скан локального каталога сегментов и живого журнала. |
| [`kronika-reader`](crates/kronika-reader/) | Проверенный декод секций, snapshots, pagination, logical sections, gauges и diff. |
| [`kronika-analytics`](crates/kronika-analytics/) | Независимые от источника ядра разностей счётчиков, поиска аномалий и контрактов overview. |
| [`kronika-source-pg`](crates/kronika-source-pg/) | Запросы PostgreSQL и раскладка разных версий в строки реестра. |
| [`kronika-source-os`](crates/kronika-source-os/) | Ограниченное чтение Linux `/proc`, `/sys`, файловых систем, процессов и cgroup. |
| [`kronika-source-log`](crates/kronika-source-log/) | Ограниченный tail stderr, нормализация, типизированные события и gap-учёт. |
| [`pg_kronika-collector`](bins/pg_kronika-collector/) | Жизненный цикл сбора, интервалы, бюджеты, coverage, журнал и ротация. |
| [`pg_kronika-web`](bins/pg_kronika-web/) | Локальный UI, JSON API, auth, readiness, bounded queries, аномалии, кластеризация инцидентов и диагностические выводы (`findings`). |
| [`kronika-bdd`](crates/kronika-bdd/) | Docker/Nix runner интеграционных сценариев для PostgreSQL 15–18. |
| [`xtask`](xtask/) | Проверка границ зависимостей в CI. |
| `pg_kronika-archiver`, `pg_kronika-dump` | Заглушки: печатают ошибку и завершаются с кодом 2. |

Актуальные границы зависимостей и путь данных описаны в
[`docs/architecture.md`](docs/architecture.md). CI проверяет allow list
зависимостей командой `cargo run -p xtask -- check-deps`.

## Контракты, важные при эксплуатации

- **Формат и целостность.** PGM format version 1 использует little-endian
  кадры, CRC32C каждой секции и каталог с собственной CRC. CRC обнаруживает
  случайную порчу, но не защищает от подмены. Ошибочные данные не превращаются
  в строки: reader возвращает typed error или диагностику пропуска.
- **Надёжность записи.** Кадр журнала синхронизируется до возврата из append.
  При запечатывании writer записывает и синхронизирует временный файл, затем
  публикует его без перезаписи существующего сегмента. Оборванный последний
  кадр обрезается при восстановлении; другая порча остаётся в диагностике.
- **Границы ресурсов.** Секция ограничена 65 536 строками, 8 MiB кодированных
  данных и 16 группами строк Parquet. Коллектор ограничивает cardinality,
  словари, время цикла и журнал. У reader есть лимиты строк и materialized
  cells. Web допускает один тяжёлый anomaly/incident request и отвечает `503`,
  если этот слот занят.
- **Качество данных.** Неизменившийся счётчик даёт настоящий нулевой diff.
  Сброс, разрыв покрытия, первая точка, неверный порядок времени и выключенный
  gate дают разные no-data reasons. Diff и anomaly не заменяют их нулями и не
  соединяют ряд через разрыв.
- **Безопасность.** Сегменты могут содержать SQL, планы, имена объектов,
  аргументы процессов и текст журнала. Ограничьте доступ к каталогу и копиям:
  коллектор не шифрует и не редактирует содержимое. Web следует слушать на
  loopback или ставить за TLS reverse proxy; Basic Auth не шифрует транспорт.

Полные лимиты и варианты ошибок находятся в README и rustdoc соответствующих
крейтов.

## Документация и проверка

- Установка и первый запуск: [кратчайший путь](#кратчайший-путь-от-сборки-до-данных)
- Настройки коллектора: [operator guide `pg_kronika-collector`](bins/pg_kronika-collector/README.ru.md)
- JSON API и настройки web: [operator guide `pg_kronika-web`](bins/pg_kronika-web/README.ru.md)
- Подключения и сбор по базам: [`docs/connection-and-multidb.md`](docs/connection-and-multidb.md)
- Контракты типов и качества данных: [`docs/type-registry.md`](docs/type-registry.md)
- Локальные и CI-тесты: [`docs/testing.md`](docs/testing.md)
- Правила и runner BDD: [`docs/bdd-testing-guide.md`](docs/bdd-testing-guide.md) и [`kronika-bdd`](crates/kronika-bdd/)
- Текущая архитектура: [`docs/architecture.md`](docs/architecture.md)
- Контейнер: [`kronika-format`](crates/kronika-format/) и исторический design note [`docs/segment-format.md`](docs/segment-format.md)

Для документационной правки репозиторий задаёт минимальный gate:

```sh
git diff --check
cargo +1.96.0 fmt --all --check
```

Перед изменением Rust или BDD прочитайте [testing.md](docs/testing.md).

## Лицензия

PgKronika распространяется под [MIT License](LICENSE).
