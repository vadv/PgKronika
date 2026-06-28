# kronika-bdd

[English version](README.md)

`kronika-bdd` запускает интеграционные BDD-сценарии для PostgreSQL. Nix даёт
PostgreSQL 15, 16 и 17; программа поднимает их параллельно и подключается через
`tokio-postgres`. Набор сценариев проверяет запуск серверов, вызывает
коллектор `source-pg` на каждой версии и запускает `pg_kronika-collector`, пока
тот не запишет запечатанный сегмент.

## Что запускается

`features/smoke.feature` проверяет запуск серверов:

```gherkin
Scenario: every version is reachable
  Given the PostgreSQL matrix is booted
  Then every version answers a version query
```

Для каждой основной версии PostgreSQL программа:

- создаёт временный каталог данных;
- запускает `initdb` с методом аутентификации `trust`, локалью `C` и
  `--no-sync`;
- запускает `postgres` на свободном локальном TCP-порту;
- ждёт, пока сервер начнёт принимать TCP-подключения;
- выполняет `SHOW server_version`;
- проверяет, что версия начинается с ожидаемого основного номера.

`features/collector.feature` вызывает коллектор `source-pg` на запущенных
версиях PostgreSQL:

```gherkin
Scenario: every version yields a valid bgwriter/checkpointer snapshot
  Given the PostgreSQL matrix is booted
  Then every version reports valid bgwriter/checkpointer stats
```

Основная версия выбирает точный коллектор и `type_id`: PG 15-16 вызывают
`collect_bgwriter` (`1_006_001`), PG 17 вызывает `collect_checkpointer`
(`1_006_002`). Для каждой версии сценарий проверяет, что:

- `ts` строки — это `clock_timestamp()` сервера, близкий к времени процесса,
  который запускает тесты;
- счётчики неотрицательны, а `stats_reset` представления не позже этого `ts`.

Так сценарий ловит SQL, который больше не совпадает с каталогом, или выбор
неверного типа.

Этот же файл запускает исполняемый файл коллектора:

```gherkin
Scenario: every version seals a readable segment with its version's sections
  Given the PostgreSQL matrix is booted
  Then every version is collected into a sealed segment with its version's sections
```

Для каждой версии программа запускает `pg_kronika-collector` (путь из
`KRONIKA_COLLECTOR_BIN`), ждёт строку `ready`, посылает `SIGUSR2` и читает
строку `sealed <path>`. Затем она открывает сегмент через `kronika-reader` и
типизированно декодирует точные секции для этой основной версии: семейство
bgwriter (`1_006_001` или `1_006_002`) и контекст сбросов (`1_020_001` или
`1_020_002`). Сценарий проверяет, что `ts` каждой строки попадает в диапазон
сегмента, а типизированные значения сохранились после записи и чтения.

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
- `server_version` mismatch: процесс ответил, но не той основной версией
  PostgreSQL.
- `collect type 1_006_...`, `collect type 1_020_...` или `postgres NN: ...` из
  сценария коллектора: запрос не совпал с каталогом сервера либо декодированная
  секция не прошла проверку. Сообщение называет тип, колонку или основную
  версию PostgreSQL.
