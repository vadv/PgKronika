# Запуск тестов PgKronika

Этот документ описывает, какие проверки запускать локально и как устроен BDD
путь в CI. Правила написания BDD-сценариев вынесены в
[bdd-testing-guide.md](bdd-testing-guide.md).

## Что запускать

| Задача | Команда |
| --- | --- |
| Быстрая проверка одного крейта | `cargo test -p <crate>` |
| Полный локальный Rust gate | см. [Полный локальный gate](#полный-локальный-gate) |
| Проверка BDD image helper без PostgreSQL | `scripts/test-bdd-image.sh` |
| Полный BDD через Docker/Nix | `DEBUG=1 make test-bdd` |
| BDD по тегу через Docker/Nix | `DEBUG=1 make test-bdd TAGS=@pg_log` |
| Полный BDD image run | `./scripts/bdd-image.sh build-builder && ./scripts/bdd-image.sh build-runtime && ./scripts/bdd-image.sh run` |
| Архитектурные зависимости | `cargo run -p xtask -- check-deps` |

Для документационных правок обычно достаточно `git diff --check` и
`cargo fmt --all -- --check`. Для Rust-кода запускайте полный gate.

## Требования

- Rust toolchain берётся из [`rust-toolchain.toml`](../rust-toolchain.toml).
- Для BDD через `make test-bdd` нужен Docker daemon. Docker Buildx нужен только
  при отсутствии exact dependency builder.
- Nix на хосте не нужен для Docker-пути: Nix запускается внутри builder image.
- Nix на хосте нужен только для ручного варианта `nix build .#image`.
- Публичный exact dependency builder читается анонимно. Права на push нужны
  только для первой публикации отсутствующего exact builder; runtime image не
  публикуется.

## Полный локальный gate

Запускайте из корня репозитория:

```sh
git diff --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
DEBUG=1 make test-bdd
```

`check-deps` проверяет границы workspace: collector не должен зависеть от
storage-backend кода, web не должен тянуть PostgreSQL sources, а служебные
крейты не должны создавать циклы ответственности.

## Локальный BDD

Полный запуск BDD:

```sh
DEBUG=1 make test-bdd
```

Запуск одного набора сценариев по тегу:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` необязателен. Если он не задан, runner не передаёт `--tags` и запускает
весь BDD suite. Если `TAGS` задан, значение валидируется как Cucumber tag
expression и передаётся в Cucumber как `--tags`. `DEBUG=1` включает verbose
Cucumber output (`-vvv`) и передаётся в контейнер как переменная окружения.

Этот путь использует тот же dependency builder и тот же Nix build, что и CI.
Runtime получает только локальный тег:

```text
pgkronika-bdd:local
```

Каждый запуск заново передаёт текущие source/features в builder, компилирует
first-party код и собирает runtime. Exact source rerun на чистом runner не
переиспользует compiler output. Dependency Cargo artifacts, PostgreSQL 15–18 и
`pg_store_plans` не пересобираются, если exact builder найден.

## Модель кэша BDD

Единственный публикуемый cache image — exact dependency builder. Его тег
содержит platform и dependency key; mutable branch aliases отсутствуют.

Dependency key меняется при изменении:

- `Cargo.toml` и `Cargo.lock`;
- crate/binary `Cargo.toml`;
- `flake.nix` и `flake.lock`;
- `rust-toolchain.toml`;
- `Dockerfile.bdd-builder`.

Source и BDD features не входят ни в один registry cache key. Они только
передаются в локальную runtime assembly:

- Rust source files в `crates/*/src/**` и `bins/*/src/**`;
- BDD feature files в `crates/kronika-bdd/features/**`.

Host-only файлы также не меняют dependency key:

- `Makefile`;
- `scripts/test-bdd-local.sh`;
- runner/helper shell changes, если они не меняют builder context;
- README и docs.

Практический эффект:

- изменения dependency contract создают новый exact builder tag;
- обычные изменения Rust source и features сохраняют builder tag, но всегда
  пересобирают ephemeral local runtime;
- документация и runner-only файлы не меняют builder tag, но BDD job всё равно
  выполняет source build: final runtime cache намеренно отсутствует.

Полезные параметры:

```sh
BDD_BUILDER_PULL=1          # попытаться взять exact builder из registry
BDD_BUILDER_PUSH=1          # опубликовать только отсутствующий exact builder
BDD_RUNTIME_IMAGE=...       # задать ephemeral local runtime tag вручную
BDD_OUTPUT_TAR=...          # сохранить runtime tarball в выбранный путь
BDD_IMAGE_PREFIX=...        # registry prefix для builder images
```

## Полный BDD image run

Полный запуск матрицы через Docker/Nix:

```sh
export BDD_BUILDER_PULL=1

./scripts/bdd-image.sh build-builder
./scripts/bdd-image.sh build-runtime
./scripts/bdd-image.sh run
```

`build-builder` строит или берёт exact dependency builder. `build-runtime`
всегда компилирует переданные source внутри него, собирает runtime и тегирует
его локально. По умолчанию локальный тег — `pgkronika-bdd:local`; в GitHub
Actions используется `pgkronika-bdd:run-<run-id>-<attempt>`. `run` запускает
этот image без фильтрации сценариев.

Если Nix установлен на хосте, можно собрать image напрямую:

```sh
nix build .#image --out-link result-bdd-image
./result-bdd-image | docker load
docker run --rm pgkronika-bdd:latest
```

Docker-путь остаётся основным локальным путём, потому что он совпадает с CI и не
требует Nix на хосте.

## GitHub Actions

В репозитории есть один workflow: [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

Задания:

- `fmt + clippy`: `cargo fmt --all --check` и
  `cargo clippy --workspace --all-targets`;
- `dependency rules`: `cargo run -p xtask -- check-deps` и
  `bash scripts/test-bdd-image.sh`;
- `test`: `cargo test --workspace`;
- `coverage`: `cargo llvm-cov --workspace`;
- `bdd matrix`: вычисляет dependency key, анонимно проверяет exact builder,
  собирает его только при miss, затем всегда собирает ephemeral local runtime
  и запускает PostgreSQL matrix.

Exact builder публично читается без login. Trusted run из этого же репозитория
при miss логинится и публикует единственный exact tag. PR из форка не получает
GHCR credentials и при miss строит builder только локально, без push.

Обычный Rust source change не меняет builder tag и не строит dependency Cargo
artifacts/PG closure. Source build при этом запускается всегда. Между чистыми
hosted runners нет compiler cache, поэтому нельзя обещать компиляцию только
затронутых first-party crates.

## GitLab CI

В репозитории нет `.gitlab-ci.yml` и нет активного GitLab CI pipeline. Для
GitLab используйте тот же путь `scripts/bdd-image.sh`: Docker executor с Buildx,
registry login, `BDD_IMAGE_PREFIX="$CI_REGISTRY_IMAGE"`, затем
`build-builder`, `build-runtime`, `run`.

Минимальный пример:

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
    - export BDD_BUILDER_PULL=1 BDD_BUILDER_PUSH=1
    - export BDD_RUNTIME_IMAGE="pgkronika-bdd:run-${CI_PIPELINE_ID}-${CI_JOB_ID}"
    - ./scripts/bdd-image.sh build-builder
    - ./scripts/bdd-image.sh build-runtime
    - ./scripts/bdd-image.sh run "$BDD_RUNTIME_IMAGE"
```

## Частые ошибки

- `TAGS must contain at least one Cucumber tag`: проверьте tag expression,
  например `TAGS=@pg_log`. Не задавайте `TAGS`, если нужен полный BDD run.
- `Docker daemon is not reachable`: start Docker or set `BDD_DOCKER`.
- `docker: 'buildx' is not a docker command`: exact builder отсутствует;
  install/enable Buildx для его локальной сборки.
- `KRONIKA_PG_MATRIX is not set`: the BDD binary was run outside the Nix image.
  Use `make test-bdd`, `scripts/bdd-image.sh run`, or provide the matrix
  manually.
- GitHub job завершается без runner и без steps: это проблема инфраструктуры
  Actions или billing до запуска команд репозитория. Повторите запуск после
  восстановления доступа к Actions.
