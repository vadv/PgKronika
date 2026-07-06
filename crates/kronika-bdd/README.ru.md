# kronika-bdd

[English version](README.md)

`kronika-bdd` запускает интеграционные BDD-сценарии для PostgreSQL. Nix даёт
матрицу PostgreSQL (с 15 по 18); программа поднимает её один раз на весь
прогон, а каждый сценарий открывает собственную базу с уникальным именем на
одном из кластеров, управляет сессиями через `tokio-postgres`, запускает
`pg_kronika-collector`, пока тот не запишет запечатанный сегмент, и проверяет
записанные строки.

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

Фичи метрик (activity, archiver, bgwriter/checkpointer, database, I/O, locks,
prepared transactions, progress vacuum, statements, user tables и indexes, WAL,
replication instance, connection pool) используют общий словарь шагов:

- `Given a fresh database on PostgreSQL NN` — изолированная база сценария;
- `Given a database seeded with:` и `Given a second database seeded with:` —
  подготовка SQL из docstring;
- `Given session "X" runs:` / `… runs and holds its transaction open:` /
  `… runs and blocks:` — именованные сессии, чьи backend PID разрешают
  плейсхолдеры `[X]` в таблицах ожиданий;
- `When the collector snapshots the segment` — запускает
  `pg_kronika-collector` (путь из `KRONIKA_COLLECTOR_BIN`), пока тот не
  запишет запечатанный сегмент;
- `Then section 1_XXX_YYY has exactly one row:` / `has a row for session "X":`
  / `has a row with <col> = <val>:` — таблицы ожиданий против декодированной
  секции;
- `Then section 1_XXX_YYY <column> matches the <kind> oracle:` — независимое
  SQL-чтение, сравнение по виду: `exact`, `transformed`, `subset`, `floor`
  (нижняя граница), `ceiling` (верхняя);
- `Given the window floor for section 1_XXX_YYY <column> is captured as:` в
  паре с `Then section 1_XXX_YYY <column> is between the captured floor and:` —
  монотонный счётчик проверяется между чтениями оракула до и после снапшота;
- `Then section 1_XXX_YYY is absent from the segment` — проверка отсутствия для
  метрик с разделёнными layout.

Фраза шага без зарегистрированного определения завершает прогон ошибкой
(`fail_on_skipped`), а каждая упавшая проверка печатает декодированную секцию,
значения оракула, `server.log` и stderr коллектора.

## Запуск тестов

Команды локального запуска, требования Docker/Buildx, модель кэша BDD image и
CI описаны в [../../docs/testing.md](../../docs/testing.md).

Модульные тесты runner не поднимают PostgreSQL:

```sh
cargo test -p kronika-bdd
```

Обычный tagged запуск через Docker/Nix из корня репозитория:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` обязателен и передаётся в Cucumber как `--tags`. `DEBUG=1` включает
подробный вывод Cucumber и передаёт `DEBUG` в контейнер.

## Полезные ошибки

- `KRONIKA_PG_MATRIX is not set`: исполнитель запустили вне образа Docker и не
  передали пути к исполняемым файлам PostgreSQL.
- `postgres ... not ready`: сервер не стартовал или не начал принимать TCP за
  30 секунд. В ошибку добавляется `server.log`.
- Ошибка smoke: кластер ответил, но `server_version_num / 10000` — не
  тот major, который объявила матрица.
- Упавшие проверки печатают декодированную таблицу секции, значения оракула,
  `server.log` и stderr коллектора; сообщение называет секцию и колонку.
