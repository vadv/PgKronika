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
| Tagged BDD через Docker/Nix | `DEBUG=1 make test-bdd TAGS=@pg_log` |
| Полный BDD image run | `./scripts/bdd-image.sh build-builder && ./scripts/bdd-image.sh build-runtime && ./scripts/bdd-image.sh run` |
| Архитектурные зависимости | `cargo run -p xtask -- check-deps` |

Для документационных правок обычно достаточно `git diff --check` и
`cargo fmt --all -- --check`. Для Rust-кода запускайте полный gate.

## Требования

- Rust toolchain берётся из [`rust-toolchain.toml`](../rust-toolchain.toml).
- Для BDD через `make test-bdd` нужен Docker daemon и Docker Buildx.
- Nix на хосте не нужен для Docker-пути: Nix запускается внутри builder image.
- Nix на хосте нужен только для ручного варианта `nix build .#image`.
- Для публикации BDD cache в registry нужны права на push; локальный запуск
  работает без них.

## Полный локальный gate

Запускайте из корня репозитория:

```sh
git diff --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```

`check-deps` проверяет границы workspace: collector не должен зависеть от
storage-backend кода, web не должен тянуть PostgreSQL sources, а служебные
крейты не должны создавать циклы ответственности.

## Локальный BDD по тегу

Запуск одного набора сценариев:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` обязателен. Значение передаётся в Cucumber как `--tags`, поэтому можно
использовать обычные tag expressions. `DEBUG=1` включает verbose Cucumber output
(`-vvv`) и передаётся в контейнер как переменная окружения.

Этот путь использует Docker/Buildx и тот же Nix image, что и CI. Runner сам
вычисляет content-keyed тег runtime image:

```text
pgkronika-bdd:<platform>-sha-<runtime-key>
```

Если такой local image уже есть, он переиспользуется. Повторный запуск после
правки host-only файлов обычно не собирает образ.

## Модель кэша BDD

BDD image path разделяет dependency builder и runtime image.

Dependency key меняется при изменении:

- `Cargo.toml` и `Cargo.lock`;
- crate/binary `Cargo.toml`;
- `flake.nix` и `flake.lock`;
- `rust-toolchain.toml`;
- `Dockerfile.bdd-builder`.

Runtime key меняется при изменении dependency inputs, а также:

- Rust source files в `crates/*/src/**` и `bins/*/src/**`;
- BDD feature files в `crates/kronika-bdd/features/**`.

Host-only файлы не меняют dependency key и runtime key:

- `Makefile`;
- `scripts/test-bdd-local.sh`;
- runner/helper shell changes, если они не входят в runtime source;
- README и docs.

Практический эффект:

- изменения зависимостей пересобирают builder image и runtime image;
- обычные изменения Rust source пересобирают runtime image, но не dependency
  builder;
- изменения документации и runner-only файлов переиспользуют существующий
  runtime image.

Полезные параметры:

```sh
BDD_BUILDER_PULL=1          # попытаться взять exact builder из registry
BDD_RUNTIME_REUSE_LOCAL=0   # принудительно пересобрать runtime image
BDD_RUNTIME_IMAGE=...       # задать runtime image tag вручную
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

`build-builder` строит или берёт dependency builder. `build-runtime` собирает
runtime image и тегирует его content-keyed именем, если `BDD_RUNTIME_IMAGE` не
задан. `run` запускает этот image без фильтрации сценариев.

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
- `bdd metadata`: вычисляет dependency/runtime keys и image refs;
- `bdd matrix`: берёт готовый runtime image из GHCR или собирает его через
  `scripts/bdd-image.sh`, затем запускает PostgreSQL matrix.

PR из этого же репозитория могут читать и обновлять GHCR cache. PR из форков
используют те же keys и build path, но не публикуют cache обратно в registry.

`bdd matrix` не пересобирает dependency builder от обычных Rust source changes.
Такие изменения меняют runtime key и приводят только к runtime build. Если exact
runtime image уже есть в GHCR, job пропускает build.

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
    - platform_slug=$(./scripts/bdd-image.sh platform-slug)
    - export BDD_BUILDER_PULL=1 BDD_BUILDER_PUSH=1
    - export BDD_RUNTIME_IMAGE="${BDD_IMAGE_PREFIX}/pgkronika-bdd:${platform_slug}-sha-$(./scripts/bdd-image.sh image-key | cut -c1-16)"
    - ./scripts/bdd-image.sh build-builder
    - BDD_RUNTIME_PUSH=1 ./scripts/bdd-image.sh build-runtime
    - ./scripts/bdd-image.sh run "$BDD_RUNTIME_IMAGE"
```

## Частые ошибки

- `TAGS is required`: pass a Cucumber tag expression, for example
  `TAGS=@pg_log`.
- `Docker daemon is not reachable`: start Docker or set `BDD_DOCKER`.
- `Docker Buildx is required`: install/enable Buildx for the Docker daemon.
- `KRONIKA_PG_MATRIX is not set`: the BDD binary was run outside the Nix image.
  Use `make test-bdd`, `scripts/bdd-image.sh run`, or provide the matrix
  manually.
- GitHub job завершается без runner и без steps: это проблема инфраструктуры
  Actions или billing до запуска команд репозитория. Повторите запуск после
  восстановления доступа к Actions.
