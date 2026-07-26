# Разработка и проверка PgKronika

Этот документ перечисляет локальные проверки и объясняет, как они соотносятся
с GitHub Actions. Правила написания BDD-сценариев находятся в
[`bdd-testing-guide.md`](bdd-testing-guide.md).

## Выбор проверки

| Изменение | Минимальная проверка |
| --- | --- |
| Только Markdown | `git diff --check` и `cargo +1.96.0 fmt --all --check` |
| Один Rust-пакет | форматирование, Clippy и тесты изменённого пакета |
| Общий контракт или несколько пакетов | полный Rust-набор и `xtask check-deps` |
| Сбор PostgreSQL, веб-API или BDD | полный Rust-набор, тест помощника образа и нужный BDD-набор |
| Зависимости, Nix или BDD-образ | полный BDD-прогон |

Все команды запускаются из корня репозитория.

## Целевая платформа

В [`.cargo/config.toml`](../.cargo/config.toml) по умолчанию выбрана выпускная
цель `x86_64-unknown-linux-musl`. Её нельзя использовать для запуска тестов на
macOS, а на Linux для неё нужен отдельный набор стандартной библиотеки и
компоновщик.

Тестовые бинарники нужно собирать для платформы текущего компьютера:

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"
cargo +1.96.0 test -p kronika-registry --target "$HOST"
```

Полный `cargo test --workspace` рассчитан на Linux: `kronika-source-os`
проверяет живые `/proc` и mount namespace. На macOS запускайте тесты
переносимых изменённых пакетов с `--target "$HOST"`; полный набор выполнит
Linux CI.

Полный Clippy, напротив, проверяет Linux-ветки `kronika-source-os` и
`kronika-source-log`. Используйте выпускную цель musl; для неё нужны
установленные `x86_64-unknown-linux-musl` и `musl-gcc`. Clippy для Darwin
проверяет платформенные заглушки вместо рабочего Linux-кода и не заменяет эту
проверку.

GitHub Actions задаёт `CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu`, поэтому
команды CI не наследуют выпускную цель musl.

## Полный Rust-набор на Linux

```sh
HOST="$(rustc +1.96.0 -vV | sed -n 's/^host: //p')"

git diff --check
cargo +1.96.0 fmt --all --check
cargo +1.96.0 clippy --workspace --all-targets \
  --target x86_64-unknown-linux-musl -- -D warnings
cargo +1.96.0 test --workspace --target "$HOST"
cargo +1.96.0 run -p xtask --target "$HOST" -- check-deps
bash scripts/test-bdd-image.sh
```

На macOS оставьте те же команды, но замените `cargo test --workspace` на
перечень изменённых переносимых пакетов. Команды Clippy и `xtask` сохраняются
без изменений.

`xtask` строит граф зависимостей рабочего пространства и проверяет разрешённые
границы бинарников из [`architecture.md`](architecture.md).
`test-bdd-image.sh` проверяет вычисление ключа, контексты и команды
Docker/Nix-образа без запуска PostgreSQL.

## Локальный BDD

Требования:

- запущенный Docker;
- Docker Buildx, если точного образа зависимостей ещё нет локально или в
  реестре;
- доступ к GHCR для необязательного чтения публичного образа зависимостей.

Nix на хосте не нужен: основной путь запускает Nix внутри образа сборщика.

Полная матрица PostgreSQL 15–18:

```sh
DEBUG=1 make test-bdd
```

Сценарии по выражению тегов Cucumber:

```sh
DEBUG=1 make test-bdd TAGS='@pg_log'
```

Без `TAGS` выполняются все сценарии. Значение проверяется до сборки образа.
`DEBUG=1` добавляет подробный вывод Cucumber.

Сценарии перезапуска и восстановления используют настоящий
`pg_kronika-web`, включённый в BDD-образ:

```sh
DEBUG=1 make test-bdd TAGS=@timeline_web_lifecycle
DEBUG=1 make test-bdd TAGS='@timeline_web_lifecycle and @pg15'
```

Первый вариант проверяет PostgreSQL 15–18, второй оставляет только PG15 для
целевой диагностики. Harness ждёт явного сообщения готовности после открытия
порта, завершает процесс сигналом и управляет публикацией через Unix-сокет
между синхронизацией временного OVF и атомарным переименованием. Фиксированных
задержек и циклов повторных запросов в этих проверках нет.

`make test-bdd` выполняет три действия:

1. находит или собирает точный образ зависимостей;
2. всегда компилирует текущий код и собирает локальный рабочий образ;
3. запускает `kronika-bdd` с нужным выражением тегов.

Рабочий образ по умолчанию имеет локальный тег `pgkronika-bdd:local` и не
публикуется.

## Прямое управление BDD-образом

```sh
export BDD_BUILDER_PULL=1

./scripts/bdd-image.sh build-builder
./scripts/bdd-image.sh build-runtime
./scripts/bdd-image.sh check-runtime
./scripts/bdd-image.sh run
```

Полный список команд и переменных:

```sh
./scripts/bdd-image.sh
```

Наиболее полезные переменные:

| Переменная | Назначение |
| --- | --- |
| `BDD_BUILDER_PULL=1` | Проверить точный образ зависимостей в реестре |
| `BDD_BUILDER_PUSH=1` | Опубликовать отсутствующий точный образ |
| `BDD_BUILDER_IMAGE` | Явно задать образ зависимостей |
| `BDD_RUNTIME_IMAGE` | Задать локальный тег рабочего образа |
| `BDD_OUTPUT_TAR` | Сохранить собранный рабочий образ по выбранному пути |
| `BDD_IMAGE_PREFIX` | Изменить префикс реестра образов |
| `BDD_PLATFORM` | Задать платформу Docker, например `linux/amd64` |
| `BDD_DOCKER` | Использовать другую команду вместо `docker` |

## Кэш BDD

Публикуется только точный образ зависимостей. Его ключ зависит от:

- корневых и пакетных `Cargo.toml`, а также `Cargo.lock`;
- `.cargo/**`;
- `flake.nix` и `flake.lock`;
- `rust-toolchain.toml`;
- `Dockerfile.bdd-builder`;
- списка целей Cargo, для которых создаются пустые исходники образа.

Код Rust, статические файлы веб-процесса и feature-файлы входят в контекст
рабочей сборки, но не в ключ образа зависимостей. Поэтому изменение исходников
сохраняет точный образ зависимостей, однако текущий код всё равно компилируется
заново. Изменение README или `docs` не меняет ни ключ зависимостей, ни контекст
рабочего образа.

Проверить фактические списки можно командами:

```sh
./scripts/bdd-image.sh deps-paths
./scripts/bdd-image.sh runtime-paths
./scripts/bdd-image.sh deps-key
```

## GitHub Actions

Рабочий процесс находится в
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml):

| Задание | Что выполняет |
| --- | --- |
| `fmt + clippy` | `cargo fmt --all --check`, затем Clippy всего рабочего пространства |
| `dependency rules` | `xtask check-deps` и модульные проверки BDD-образа |
| `test` | `cargo test --workspace` |
| `coverage` | `cargo llvm-cov --workspace` и публикация `coverage.json` |
| `overview qualification artifact` | Измерение и проверка квалификационного артефакта обзора |
| `bdd matrix` | Сборка текущего кода и запуск BDD на PostgreSQL 15–18 |

Все задания checkout выполняют точный SHA головы PR. Для Rust-команд
используется GNU-цель хоста CI.

BDD-задание сначала ищет публичный точный образ зависимостей. Доверенный запуск
из этого репозитория может опубликовать отсутствующий образ. PR из форка не
получает учётные данные GHCR: при отсутствии образа он собирается только
локально.

## Частые ошибки

- **Тест пытается запустить Linux-musl бинарник на macOS:** команда унаследовала
  выпускную цель из `.cargo/config.toml`. Передайте `--target "$HOST"`, как
  показано выше.

- **`can't find crate for std` для `x86_64-unknown-linux-musl`:** добавьте цель
  в toolchain 1.96 командой
  `rustup +1.96.0 target add x86_64-unknown-linux-musl`.

- **`linker musl-gcc not found`:** установите musl toolchain. Само добавление
  цели Rust не устанавливает системный компоновщик.

- **`TAGS must contain at least one Cucumber tag`:** используйте выражение с
  тегом, например `TAGS='@pg_log'`, либо не задавайте `TAGS`.

- **`Docker daemon is not reachable`:** запустите Docker или задайте рабочую
  команду через `BDD_DOCKER`.

- **`docker: 'buildx' is not a docker command`:** точный образ зависимостей не
  найден и требуется локальная сборка. Установите или включите Buildx.

- **`KRONIKA_PG_MATRIX is not set`:** `kronika-bdd` запущен вне подготовленного
  образа. Используйте `make test-bdd` или `scripts/bdd-image.sh run`.
