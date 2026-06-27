# kronika-bdd

[English version](README.md)

`kronika-bdd` — исполнитель BDD-сценариев для интеграционных проверок
PostgreSQL. Nix даёт PostgreSQL 15, 16 и 17, программа поднимает их параллельно
и подключается к ним через `tokio-postgres`. Она проверяет саму тестовую
инфраструктуру и прогоняет коллектор `source-pg` вживую против каждой версии.

## Что запускается

`features/smoke.feature` проверяет саму инфраструктуру:

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

`features/collector.feature` прогоняет коллектор `source-pg` против живой
матрицы:

```gherkin
Scenario: every version yields a plausible bgwriter/checkpointer snapshot
  Given the PostgreSQL matrix is booted
  Then every version reports plausible bgwriter/checkpointer stats
```

Для каждой версии она вызывает `collect_bgwriter_checkpointer` (тип реестра
`1_006_001`) и проверяет, что:

- строка несёт ту метку времени, которую передал вызывающий код;
- счётчики неотрицательны, а `bgwriter_stats_reset` — реальный момент не позже
  времени сбора;
- заполненные и `NULL`-колонки соответствуют версии: PG17+ заполняет колонки
  рестарт-точек и сброса checkpointer и убирает `buffers_backend`, а более
  ранние версии — наоборот.

Это живая проверка выбора версии в коллекторе: запрос, переставший
соответствовать каталогу сервера, падает здесь, а не в продакшене.

## Быстрая проверка на локальной машине

Эта команда запускает только модульные тесты, безопасные для локальной машины.
PostgreSQL она не поднимает:

```sh
cargo test -p kronika-bdd
```

Используйте её для изменений в разборе `KRONIKA_PG_MATRIX` и коде исполнителя.
Это не полный прогон для PostgreSQL 15, 16 и 17.

## Полный локальный запуск через Docker

Это тот же путь, что в CI, но в безопасном варианте для рабочей копии. Nix на
локальной машине не нужен: он запускается внутри закреплённого образа
`nixos/nix`.

Из корня репозитория:

```sh
export NIX_BUILD_IMAGE='docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894'

docker run --rm \
  -v "$PWD":/work:ro \
  -e NIX_CONFIG='experimental-features = nix-command flakes' \
  "$NIX_BUILD_IMAGE" \
  sh -ceu '
    mkdir -p /tmp/src
    tar --exclude=.git --exclude=target --exclude=result --exclude=.direnv \
      -C /work -cf - . | tar -C /tmp/src -xf -
    cd /tmp/src
    nix build .#image --out-link /tmp/img
    /tmp/img
  ' > image.tar

docker load -i image.tar
docker run --rm pgkronika-bdd:latest
```

Первая команда собирает tar-файл с образом. Вторая загружает его в Docker.
Третья запускает проверку PostgreSQL 15, 16 и 17.

`image.tar` — только локальный файл; после проверки его можно удалить.

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

- `bdd image` один раз собирает образ Docker через Nix;
- `bdd matrix` запускает уже готовый образ.

Для PR из этого же репозитория `bdd image` публикует образ в GHCR под тегом,
основанным на хэше содержимого. Если такой тег уже есть, дорогая сборка
пропускается. Для PR из форка tar-файл с образом передаётся через GitHub
Actions как временный файл, без публикации в GHCR.

Хэш содержимого включает файлы flake, `Cargo.lock`, `Cargo.toml`,
закреплённую версию Rust, исходники `kronika-bdd` и Gherkin-файлы. Изменение
любого из этих входов даёт новый тег образа.

## Полезные ошибки

- `KRONIKA_PG_MATRIX is not set`: исполнитель запустили вне образа Docker и не
  передали пути к исполняемым файлам PostgreSQL.
- `postgres ... not ready`: сервер не стартовал или не начал принимать TCP за
  30 секунд. В ошибку добавляется `server.log`.
- `server_version` mismatch: процесс ответил, но не той основной версией
  PostgreSQL.
- `collect type 1_006_001 ...` или `postgres NN: ...` из сценария коллектора:
  запрос не совпал с каталогом сервера либо снимок оказался неправдоподобным.
  Сообщение называет колонку или выбор версии, который не сошёлся.
