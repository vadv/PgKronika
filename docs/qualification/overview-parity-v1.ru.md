# Проверка overview parity-v1

Артефакт проверки overview строится из исходных данных на одном точном Git
head. Это доказательство контракта из
`docs/superpowers/specs/2026-07-22-overview-index-timeline-api.md`, а не
переносимое обещание производительности.

## Dense-hour fixture

`overview-dense-hour-v1` содержит ровно 720 снимков `pg_stat_database` с шагом
пять секунд, одну строку reset context и полное покрытие исходной популяции для
каждого снимка. Production extraction создаёт канонические блоки counters,
gauges, resets, coverage и event facts. Runner записывает:

- байты источника, fact file и декодированных блоков;
- логический resident size и размер одного pinned набора фактов;
- fixed metric bytes отдельно от переменных event/string bytes;
- точное число рядов, samples, resets, states, coverage и facts;
- fixed metric bytes на сохранённый sample без универсального budget claim.

Disk и resident limits утверждает владелец deployment:

```text
OVERVIEW_DENSE_DISK_BUDGET_BYTES
OVERVIEW_DENSE_RESIDENT_BUDGET_BYTES
```

Если оба значения отсутствуют, артефакт записывает `owner_deferred`: точные
размеры, I/O и performance gates всё равно проверяются, но deployment-budget
verdict не заявляется. Только одно заданное значение считается ошибкой. Если
заданы оба значения, final validation требует, чтобы оба измеренных working set
укладывались в них.

## Режимы и coldness

Runner записывает все девять режимов: `derived-cold`, `restart-warm`,
`process-hot`, `range-cold/facts-warm`, `live`, `concurrent-identical`,
`concurrent-disjoint`, `memory-only` и `oracle-profile`.

Для каждой итерации `derived-cold` использует отдельный отсутствующий корень
кэша и измеряет production-путь построения, включая canonical admission и
долговечную атомарную публикацию. Перед измерением `restart-warm` создаётся один
валидный fact-файл, а в каждой итерации используется новый fact store, поэтому
process-local fallback и декодированный кэш не сохраняются. Runner сохраняет
эти деревья кэша рядом с выходным artifact как подтверждающие данные.

Слово cold здесь означает новый reader или пустое process-local состояние.
Runner не вытесняет page cache ОС и честно пишет `storage_cold=false`.
Storage-cold результат требует отдельной контролируемой процедуры для
конкретного host/filesystem profile.

## Локальный candidate

```bash
cargo run --release -p kronika-reader --example overview_qualification -- \
  --output target/qualification/overview.json
python3 scripts/validate-overview-qualification.py \
  target/qualification/overview.json
```

CI выполняет final structural, I/O и performance validation, затем загружает
raw JSON, JSON проверки и их SHA-256. Сохранённый артефакт для точного release
head проверяется так:

```bash
python3 scripts/validate-overview-qualification.py \
  overview.json --exact-head GIT_SHA --final
```

Итоговое доказательство parity также требует, чтобы все связанные test, BDD,
coverage и qualification jobs принадлежали одной попытке Actions на том же
точном head. Артефакты разных запусков и dirty-tree результаты недействительны.
Статус owner-deferred не утверждает, что размер одобрен для deployment; если
budgets заданы, deployment обязан соблюдать их.
