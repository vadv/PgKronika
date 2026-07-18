# Запуск тестов PgKronika

Правила BDD-сценариев описаны в [bdd-testing-guide.md](bdd-testing-guide.md).
Этот документ задаёт локальные команды и cache contract CI.

## Команды

| Задача | Команда |
| --- | --- |
| Тест одного crate | `cargo test -p <crate>` |
| Архитектурные зависимости | `cargo run -p xtask -- check-deps` |
| BDD cache/key tests без Docker и Nix | `bash scripts/test-bdd-image.sh` |
| Полный BDD | `DEBUG=1 make test-bdd` |
| BDD по тегу | `DEBUG=1 make test-bdd TAGS=@pg_log` |

Полный Rust gate:

```sh
git diff --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```

Toolchain берётся из [`rust-toolchain.toml`](../rust-toolchain.toml). Release и
CI Rust builds используют `x86_64-unknown-linux-musl`; нужен `musl-gcc`. Для
BDD нужны Docker daemon и Buildx. Nix работает внутри immutable dependency
image и на хосте не требуется; Nix derivations получают musl compiler из
pinned `nixpkgs`, а не из пакетов GitHub runner.

## Локальный BDD

```sh
DEBUG=1 make test-bdd
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` — однострочное Cucumber tag expression. Без него запускается весь
suite. Runner анонимно разрешает public dependency и PostgreSQL base tags в
`repo@sha256:...`, вычисляет app key, переиспользует exact local runtime либо
строит один source/app layer.

Ручной эквивалент:

```sh
resolved=$(./scripts/bdd-image.sh resolve-dependencies)
export BDD_DEPENDENCY_DIGEST_REF=$(printf '%s\n' "$resolved" | sed -n 's/^dependency_digest_ref=//p')
export BDD_PG_BASE_DIGEST_REF=$(printf '%s\n' "$resolved" | sed -n 's/^pg_digest_ref=//p')
./scripts/bdd-image.sh build-runtime
./scripts/bdd-image.sh run
```

## Cache schema 2

Pipeline разделён на три immutable объекта.

### Dependency image

Dependency key включает:

- schema, platform, Cargo target и feature set;
- `Cargo.lock`, workspace и все member manifests;
- `.cargo` config и `rust-toolchain.toml`;
- `flake.nix`, `flake.lock`, `Dockerfile.bdd-builder` и builder helper;
- PostgreSQL 15–18 и pinned `pg_store_plans` inputs;
- имена Cargo target entry points.

Содержимое Rust `src/**` не хешируется и не копируется. Builder context создаёт
dummy entry points с той же target topology, поэтому Crane вычисляет тот же
`cargoArtifacts`, что и source build, без зависимости от source bodies.

Dependency image содержит только `bddCargoArtifacts`. Из того же полного
contract trusted publisher отдельно строит exact `bddPgMatrix`:
`postgresql_15_plans`…`postgresql_18_plans`, обе pinned реализации
`pg_store_plans`, `fakeNss` и `/bin/sh`. Поэтому source builder не скачивает
PG closure второй раз. Mutable branch/main tags и fallback отсутствуют.

### PostgreSQL base

PG base содержит только runtime closure PG15–18 и helpers. Stable paths:

```text
/opt/pgkronika/pg/15
/opt/pgkronika/pg/16
/opt/pgkronika/pg/17
/opt/pgkronika/pg/18
```

Consumer использует только resolved digest. Tag служит locator, не trust
boundary.

### Source/app layer

App key связывает dependency digest, PG base digest, source/static/features и
`Dockerfile.bdd-app`. `bddAppLayer` содержит только BDD/collector binaries и
feature files. Перед build выполняется `nix build .#bddAppLayer --dry-run`;
любой planned/fetched/built `postgresql_*`, `postgresql-and-plugins`,
`pg_store_plans` или `bddPgMatrix` завершает job ошибкой. Финальный image — PG
base плюс один tar layer.

Exact runtime manifest проверяется до disk cleanup, Buildx setup и source
build. Hit сразу переходит к pull и PG15–18 matrix.

## Trust и публикация

- Pull request и fork — read-only consumers с `packages: read`; registry login
  и publish отсутствуют.
- Dependency и PG base публикуются single-flight только trusted push или
  maintainer `workflow_dispatch`. В PR #77 push исходной ветки является
  bootstrap path; PR job остаётся read-only и при гонке сообщает typed miss.
- Final runtime публикуется только после успешной matrix в trusted push или
  maintainer dispatch; PR job никогда не получает write credentials.
- Images имеют full keys, resolved digests, OCI source/revision labels и build
  provenance attestations. После publish workflow проверяет anonymous manifest
  access.
- Отсутствующий dependency digest — fail-closed. Fallback к mutable branch
  image запрещён.

## Telemetry

`$GITHUB_STEP_SUMMARY` содержит:

- schema, полные dependency/source/app keys;
- dependency, PG base и final digests;
- exact hit/miss;
- cold dependency и source build durations;
- app-layer bytes;
- `PostgreSQL derivations planned/fetched/built: 0` для source-only path.

Проверка ключей и security boundary:

```sh
./scripts/bdd-image.sh keys-json
bash scripts/test-bdd-image.sh
```

## GitHub Actions jobs

- `fmt + clippy`, `test`, `coverage`, `dependency rules`: устанавливают
  declared musl target/linker и запускают Rust gates.
- `bdd metadata`: вычисляет полный contract.
- `bdd immutable dependencies`: trusted cold publisher или exact digest hit.
- `bdd matrix`: read-only consumer, source-only plan gate и PG15–18 run.
- `bdd trusted runtime publish`: публикует app image после успешной matrix.

## Ошибки

- `immutable dependency image is absent`: maintainer должен запустить trusted
  publisher для текущего dependency key.
- `Source-only Nix plan contains PostgreSQL work`: app graph получил
  запрещённую ссылку на PG closure; исправьте graph, не ослабляйте gate.
- `can't find crate for core`: musl target не установлен; CI устанавливает его
  явно, локально используйте `rustup target add x86_64-unknown-linux-musl`.
- `Docker daemon is not reachable` / `Docker Buildx is required`: запустите
  daemon и установите Buildx.
- `KRONIKA_PG_MATRIX is not set`: запускайте через BDD runtime image.

GitLab workflow в репозитории отсутствует. Перенос обязан сохранить full-key,
digest-pinning, trusted publisher и read-only consumer; branch cache tags
возвращать нельзя.
