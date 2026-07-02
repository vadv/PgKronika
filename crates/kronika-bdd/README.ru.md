# kronika-bdd

[English version](README.md)

`kronika-bdd` запускает интеграционные BDD-сценарии для PostgreSQL. Nix даёт
матрицу PostgreSQL (с 15 по 18); программа поднимает её один раз на весь
прогон, а каждый сценарий открывает собственную базу с уникальным именем на
одном из кластеров, управляет сессиями через `tokio-postgres`, снимает
запечатанный сегмент `pg_kronika-collector` и проверяет записанные строки.

## Что запускается

Каждый сценарий следует `docs/bdd-testing-guide.md`: SQL подготовки виден в
`.feature` как docstring, ожидаемые значения конкретны и привязаны к этой
подготовке, а оракулом выступает сам PostgreSQL.

`features/smoke.feature` — единственный сценарий на всю матрицу:

```gherkin
Scenario: every booted major reports a matching server_version_num
  Given the PostgreSQL matrix is booted
  Then each cluster's declared major matches the result of:
    """
    SELECT current_setting('server_version_num')::int / 10000
    """
```

Фичи метрик (activity, archiver, bgwriter/checkpointer, database, io, locks,
prepared xacts, progress vacuum, statements, user tables и indexes, wal,
replication instance, connection pool) используют общий словарь шагов:

- `Given a fresh database on PostgreSQL NN` — изолированная база сценария;
- `Given a database seeded with:` и `Given a second database seeded with:` —
  подготовка docstring-SQL;
- `Given session "X" runs:` / `… runs and holds its transaction open:` /
  `… runs and blocks:` — именованные сессии, чьи backend PID разрешают
  плейсхолдеры `[X]` в таблицах ожиданий;
- `When the collector snapshots the segment` — запускает
  `pg_kronika-collector` (путь из `KRONIKA_COLLECTOR_BIN`) до запечатанного
  сегмента;
- `Then section 1_XXX_YYY has exactly one row:` / `has a row for session "X":`
  / `has a row with <col> = <val>:` — таблицы ожиданий против декодированной
  секции;
- `Then section 1_XXX_YYY <column> matches the <kind> oracle:` — независимое
  SQL-чтение, сравнение по виду: `exact`, `transformed`, `subset`, `floor`
  (нижняя граница), `ceiling` (верхняя);
- `Given the window floor for section 1_XXX_YYY <column> is captured as:` в
  паре с `Then section 1_XXX_YYY <column> matches the window oracle up to:` —
  монотонный счётчик зажимается между двумя чтениями оракула вокруг снапшота;
- `Then section 1_XXX_YYY is absent from the segment` — защита для метрик с
  разделёнными layout.

Фраза шага без зарегистрированного определения роняет прогон
(`fail_on_skipped`), а каждый упавший ассерт печатает декодированную секцию,
значения оракула, `server.log` и stderr коллектора.

## Быстрая проверка на локальной машине

Эта команда запускает только модульные тесты, безопасные для локальной машины.
PostgreSQL она не поднимает:

```sh
cargo test -p kronika-bdd
```

Используйте её для изменений в разборе `KRONIKA_PG_MATRIX` и коде исполнителя.
Это не полный прогон для PostgreSQL 15, 16 и 17.

## Полный локальный запуск через Docker

Нужен Docker с Buildx. Nix запускается внутри Docker. Образ сборки хранит уже
собранные зависимости Rust и PostgreSQL.

Из корня репозитория:

```sh
export BDD_IMAGE_PREFIX=ghcr.io/vadv/pgkronika
export BDD_BUILDER_PULL=1

./scripts/bdd-image.sh build-builder
./scripts/bdd-image.sh build-runtime
./scripts/bdd-image.sh run
```

`build-builder` берёт `pgkronika-bdd-builder` из реестра Docker, если там уже
есть образ с тем же ключом зависимостей. Если такого образа нет, команда
собирает его локально. `build-runtime` собирает `image.tar`, загружает
`pgkronika-bdd:latest` в Docker и оставляет tar-файл в рабочей копии.

Первая сборка образа после изменения зависимостей всё равно дорогая. Дальше
правки только в исходном коде переиспользуют тот же образ сборки.

Чтобы опубликовать образ сборщика с машины, у которой есть право на публикацию:

```sh
export BDD_BUILDER_PUSH=1
./scripts/bdd-image.sh build-builder
```

`BDD_CACHE_FROM` и `BDD_CACHE_TO` можно задать отдельно для BuildKit-кэша в
реестре Docker. По умолчанию достаточно образа сборки: так мы не публикуем один
и тот же большой `/nix/store` два раза.

## Полный локальный запуск через Nix

Если Nix уже установлен на локальной машине:

```sh
nix build .#image --out-link result-bdd-image
./result-bdd-image | docker load
docker run --rm pgkronika-bdd:latest
```

После проверки можно удалить `result-bdd-image`.

## Как это работает в CI

В GitHub Actions есть два BDD-задания:

- `bdd image` собирает или берёт из реестра образ сборщика, затем собирает образ
  запуска;
- `bdd matrix` запускает уже готовый образ.

Для PR из этого же репозитория образ сборщика лежит в GHCR. Тег образа сборщика
зависит от ключа зависимостей и платформы, поэтому правка `src/` не пересобирает
слой с Rust/PostgreSQL-зависимостями. Образ запуска всё ещё получает тег по
содержимому; если такой образ уже есть, задание пропускает сборку до очистки
диска.

PR из форка не публикуют образы в GHCR. Они собирают образ сборщика локально и
передают образ запуска в `bdd matrix` как временный файл.

Тот же скрипт можно использовать в GitLab CI:

```yaml
bdd:
  image: docker:29
  services:
    - docker:29-dind
  variables:
    DOCKER_TLS_CERTDIR: ""
    BDD_IMAGE_PREFIX: "$CI_REGISTRY_IMAGE"
    BDD_PLATFORM: linux/amd64
  before_script:
    - docker login -u "$CI_REGISTRY_USER" -p "$CI_REGISTRY_PASSWORD" "$CI_REGISTRY"
    - docker buildx create --use
  script:
    - platform_slug=$(./scripts/bdd-image.sh platform-slug)
    - export BDD_BUILDER_PULL=1 BDD_BUILDER_PUSH=1
    - export BDD_RUNTIME_IMAGE="${BDD_IMAGE_PREFIX}/pgkronika-bdd:${platform_slug}-sha-$(./scripts/bdd-image.sh image-key | cut -c1-16)"
    - ./scripts/bdd-image.sh build-builder
    - BDD_RUNTIME_PUSH=1 ./scripts/bdd-image.sh build-runtime
    - ./scripts/bdd-image.sh run "$BDD_RUNTIME_IMAGE"
```

## Полезные ошибки

- `KRONIKA_PG_MATRIX is not set`: исполнитель запустили вне образа Docker и не
  передали пути к исполняемым файлам PostgreSQL.
- `postgres ... not ready`: сервер не стартовал или не начал принимать TCP за
  30 секунд. В ошибку добавляется `server.log`.
- расхождение в smoke: кластер ответил, но `server_version_num / 10000` — не
  тот major, который объявила матрица.
- упавшие ассерты печатают декодированную таблицу секции, значения оракула,
  `server.log` и stderr коллектора; сообщение называет секцию и колонку.
