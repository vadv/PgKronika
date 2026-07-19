# kronika-bdd

[English version](README.md)

`kronika-bdd` — integration-test runner поведения collector и web на
PostgreSQL 15, 16, 17 и 18. Nix поставляет server binaries и поддерживаемые
forks `pg_store_plans`; Docker запускает тот же image локально и в GitHub
Actions.

Runner не входит в production. Обычный `cargo test --workspace` не запускает
PostgreSQL на хосте.

## Жизненный цикл сценария

Матрица PostgreSQL поднимается один раз на процесс. Сценарии выполняются
последовательно, создают отдельную базу, открывают named `tokio-postgres`
sessions, воспроизводят состояние из feature, запускают collector до готового
сегмента и сравнивают decoded rows с явным ожиданием или независимым oracle
PostgreSQL. Cleanup закрывает sessions и удаляет состояние сценария.

Пропущенный Cucumber step считается ошибкой. Failure report содержит нужную
секцию, oracle values, collector output и PostgreSQL logs. Matrix smoke
дополнительно сверяет объявленный major с `server_version_num`.

## Команды

Unit tests runner без PostgreSQL:

```sh
cargo test -p kronika-bdd
```

Полная Docker/Nix matrix из корня:

```sh
DEBUG=1 make test-bdd
```

Один tag expression:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` валидируется и передаётся как `--tags`. `DEBUG=1` включает подробный
вывод. Нужны Docker daemon и Buildx; Nix на хосте не требуется. Cache и CI
описаны в [`../../docs/testing.md`](../../docs/testing.md).

## Environment runner

Nix image задаёт:

- `KRONIKA_PG_MATRIX` — список `major=bin_dir` через `;`;
- `KRONIKA_COLLECTOR_BIN` — путь к collector;
- `KRONIKA_FEATURES` — каталог features.

Запуск binary вне этого окружения обычно завершается
`KRONIKA_PG_MATRIX is not set`. Используйте `make test-bdd`, если не
разрабатываете image helper.

Правила feature и oracle находятся в
[`../../docs/bdd-testing-guide.md`](../../docs/bdd-testing-guide.md). При
расхождении со старыми design examples каноничны текущие feature files и steps.
