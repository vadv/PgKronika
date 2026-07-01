# BDD-тесты PgKronika: договорённость и план чистки

Этот документ фиксирует, как писать BDD-тесты в PgKronika, и задаёт план
приведения существующих тестов к этому стандарту. Цель — проверять поведение
коллектора против PostgreSQL, а не внутреннюю согласованность самого коллектора.

## 1. Диагноз: конкретные признаки

CI-прогон `28544244557` показал две независимые проблемы.

### 1a. Ассерты-пустышки: коллектор проверяется сам с собой

Единственная проверка дерева ожиданий (`check_wait_tree`) сейчас сводится к
этому:

```rust
let waiter = rows.iter().find(|r| !r.blocked_by.is_empty())?;   // любая строка с ребром
let blocker_pid = waiter.blocked_by[0];
ensure!(rows.iter().any(|r| r.pid == blocker_pid && r.blocked_by.is_empty()));
ensure!(waiter.has_awaited_lock);
```

Проверка не привязана к backend'ам, которые создал сценарий. Она не проверяет
тип lock (`transactionid`), режим lock и то, что блокер именно тот, который нужен
сценарию. `has_awaited_lock` проверяется как булев флаг, а не как значение с
проверяемым смыслом. Поэтому вывод, похожий на дерево, может пройти даже при
неверных данных.

Такой тест доказывает только то, что коллектор согласован сам с собой. Он не
доказывает соответствие текущему состоянию PostgreSQL. Та же проблема повторяется
по репозиторию: для метрики есть один крупный `Then` вроде `each cluster seals X
rows`, а сетап и проверка спрятаны в Rust. `.feature` при этом не объясняет
сценарий.

### 1b. Хрупкий и слепой harness

7 из 17 сценариев упали на шаге `Given the matrix is booted`:

```text
Step panicked. Captured output: initdb for postgres 17 failed: exit status: 1
```

Причины относятся к harness, а не к метрикам.

1. **Вывод `initdb` выброшен в `/dev/null`.** В `cluster.rs::run_initdb`
   используется `.stdout(Stdio::null()).stderr(Stdio::null())`. Ошибка
   сводится к `exit status: 1` без строки с причиной. After-hook дампит только
   `server.log` уже существующего кластера, поэтому при падении `initdb` он не
   помогает: кластера ещё нет.
2. **Матрица перезагружается на каждый сценарий.** `Given the matrix is booted`
   поднимает все мажорные версии заново в каждом из 17 сценариев. Это создаёт
   десятки конкурирующих запусков `initdb`, увеличивает contention и делает CI
   флейковым.

Сам locks-сценарий прошёл (`When session H holds a row lock...`). Красный
результат дал инфраструктурный флейк, а не логика сценария. Но зелёный результат
тоже остаётся слабым сигналом, пока данные и причина их появления не наблюдаемы.

## 2. Договорённость: как писать BDD-тесты

Ниже базовые правила. Нарушение любого из них — блокер ревью.

1. **Вход должен быть видимым.** SQL, создающий доменное состояние для сценария
   (DDL, seed data, долгие транзакции, блокирующий statement), лежит в `.feature`
   как docstring. В Rust остаётся механика harness: открыть сессию, получить
   `pg_backend_pid()`, дождаться wait state, выполнить cleanup.
2. **Ассерт проверяет конкретные значения, привязанные к сетапу.** Ожидаемая
   строка задаётся видимой таблицей в `.feature` с реальными значениями.
   Плейсхолдеры вроде `[H]` разрешаются в фактические backend PID или OID
   сессий. Нельзя проверять «какое-то дерево», «секция присутствует», «метки
   разрешаются» или булев `has_X` вместо самого значения.
3. **Оракулом является PostgreSQL, но тип оракула выбирается по метрике.**
   Сценарий должен явно сказать, что именно сверяется: точное значение,
   преобразованное значение, подмножество, top-N/capped выборка, timestamp window
   или schema-only contract. Нельзя требовать exact raw query там, где коллектор
   делает version mapping, truncation, top-N, nullable преобразование или
   timestamp normalization.
4. **Состояние должно быть удержано до конца assertion.** Для lock/session
   сценариев waiter/blocker остаются живыми до завершения snapshot и oracle
   checks. Cleanup выполняется после assertion, даже при падении теста.
5. **Провал даёт контекст.** При любом падении нужен дамп: декодированная секция
   таблицей, результат oracle query, stdout/stderr `initdb`, `postgres`
   `server.log`, stderr коллектора. В Telegram/CI сообщение можно обрезать, но
   полный лог должен быть доступен как artifact.
6. **Один `Then` — один проверяемый контракт.** Шаг может проверять одну строку
   или одну таблицу, если failure report показывает column-level diff. Он не
   должен смешивать разные факты вроде boot, snapshot, dictionary, labels и
   semantic assertions.

## 3. Типы оракулов

Каждый сценарий выбирает один из этих типов и называет его в шаге или таблице
ожиданий.

- **Exact oracle.** Секция должна совпасть с raw PostgreSQL query после
  нормализации типов. Подходит для простых catalog/stat rows.
- **Transformed oracle.** PostgreSQL query возвращает raw values, а ожидаемая
  таблица явно показывает преобразование коллектора: `NULL`, unit conversion,
  `unix usec`, version layout, enum/string mapping.
- **Subset oracle.** Секция должна содержать конкретные строки, но может иметь
  дополнительные строки. Подходит для instance-wide sources и шумных системных
  представлений.
- **Top-N/capped oracle.** Сценарий проверяет выбранные строки, лимит, порядок,
  truncation/coverage marker и отсутствие скрытого overflow.
- **Window/tolerance oracle.** Для timestamps и cumulative counters проверяется
  допустимое окно или monotonic relation, а не точное равенство.
- **Schema-only oracle.** Используется только для codec/layout contract, когда
  live PostgreSQL state не нужен. Такой сценарий не считается behavioral BDD для
  метрики.

Oracle SQL не должен быть копией collector SQL. Он должен быть независимой
проверкой смысла: более простой raw query, точечный catalog lookup или
проверяемая проекция PostgreSQL state.

## 4. Скелет сценария: эталон для locks

```gherkin
@pg14 @lock @serial
Scenario: row-lock wait is captured as W -> H with the transactionid edge
  Given a database seeded with:
    """
    CREATE TABLE t(id int primary key, v int);
    INSERT INTO t VALUES (1, 0);
    """
  And session "H" runs and holds its transaction open:
    """
    BEGIN;
    UPDATE t SET v = 1 WHERE id = 1;
    """
  And session "W" runs and blocks:
    """
    UPDATE t SET v = 2 WHERE id = 1;
    """
  When the collector snapshots the segment
  Then section 1_011 has exactly one row for session "W":
    | blocked_by    | [H]           |
    | lock_locktype | transactionid |
    | lock_mode     | ShareLock     |
    | lock_granted  | false         |
  And section 1_011 has a root row for session "H" with blocked_by = []
  And section 1_011 blocked_by matches the exact oracle:
    """
    SELECT pid, pg_blocking_pids(pid)
    FROM pg_stat_activity
    WHERE wait_event_type = 'Lock'
    """
```

`"H"`, `"W"` и `[H]` обозначают реальные backend PID, полученные через
`pg_backend_pid()` при открытии сессий. Шаг `and blocks` ждёт состояния
`wait_event_type = 'Lock'` с ограниченным таймаутом. После всех assertions
harness отменяет/завершает statement W и откатывает транзакцию H.

## 5. Переиспользуемый harness

- **Именованные сессии.** `World` хранит map постоянных подключений. Шаги
  `session "X" runs ...` выполняют SQL, запоминают `pg_backend_pid()` и
  регистрируют cleanup guard. Вариант `and blocks` запускает statement в
  отдельной задаче и ждёт нужного wait state без fixed sleep.
- **Снимок.** `When the collector snapshots the segment` — общий шаг. Он не
  должен скрывать подготовку состояния.
- **Ассерт строки.** `Then section <type_id> has [exactly one] row for session
  "X": <table>` декодирует секцию, находит строку по PID сессии `X`, сверяет
  каждую именованную колонку и разрешает плейсхолдеры `[Name]`.
- **Ассерт таблицы.** Для метрик без PID используется явный key: `datid`,
  `relid`, `indexrelid`, `queryid`, `slot_name`, etc. Key должен быть виден в
  `.feature`.
- **Оракул.** `Then section <type_id> <column> matches the <oracle-kind> oracle:
  <SQL docstring>` выполняет сырой запрос и сверяет результат с секцией по
  объявленному типу oracle.
- **Дамп при провале.** Единый helper печатает decoded rows, expected rows,
  oracle rows и логи подпроцессов. Короткое сообщение содержит tail; полный лог
  сохраняется как artifact.

Generic steps допустимы только как транспорт. Смысл сценария должен оставаться в
`.feature`: SQL-вход, ожидаемые значения, oracle-kind и ключ строки.

## 6. Изоляция, матрица и теги

- **Матрица поднимается один раз на прогон или один раз на worker.** Повторный
  `initdb` на каждый сценарий запрещён.
- **Сценарий изолирует своё состояние.** По умолчанию используется отдельная
  база или схема с уникальным именем. Для instance-wide метрик сценарий должен
  явно описать, какие shared side effects допустимы.
- **Cleanup обязателен.** Открытые транзакции, блокирующие statements, temp
  databases/schemas, roles, extensions и background tasks закрываются в after
  hook.
- **Параллельность явная.** Сценарии, которые держат locks, меняют instance-wide
  state или зависят от timing, получают `@serial`.
- **Версии явные.** Сценарии используют теги `@pg10`, `@pg14`, `@pg16`, `@pg18`
  или version guards, если контракт зависит от версии PostgreSQL.
- **Дорогие сценарии помечаются.** Используются `@slow`, `@matrix`,
  `@requires_extension`, `@lock`, чтобы CI мог управлять набором.

## 7. Harness: надёжность и наблюдаемость

1. **Захватывать вывод `initdb` и `postgres`.** `run_initdb` пишет stdout+stderr
   в файл или буфер. При провале этот вывод попадает в сообщение об ошибке.
2. **Не выбрасывать stderr коллектора.** Ошибки collector process должны быть
   частью failure report.
3. **Логи в CI включены всегда.** `DEBUG=1` может расширять детализацию, но
   минимальный набор логов должен быть доступен в любом CI падении.
4. **Ограничивать объём логов в сообщении.** В failure message выводится
   полезный tail и путь к artifact, а не мегабайты raw output.
5. **`log_statement=all` применять осознанно.** Для CI это полезно, но нужно
   ограничить размер логов и не печатать секреты/DSN.
6. **Падать с контекстом.** Ошибка уровня `open sealed segment` без секции,
   oracle rows и логов недостаточна.

## 8. Запрещённые анти-паттерны

- мега-шаг `seals X rows`, который прячет сетап и ассерт;
- невидимый domain SQL сетапа в Rust;
- self-verifying checks и «правдоподобность» вместо конкретных значений;
- булев `has_X` вместо проверяемого значения;
- копирование collector SQL в oracle SQL без независимой проверки смысла;
- перезагрузка матрицы на каждый сценарий;
- выброшенный вывод подпроцессов (`Stdio::null()` у `initdb`/`postgres`);
- generic step text, который скрывает доменную причину сценария;
- сценарии, зависящие от порядка выполнения других сценариев.

## 9. План чистки: эпик качества тестов

Каждая фаза — отдельный PR.

- **Фаза 0 — фундамент.** Добавить harness: именованные сессии, шаг снимка,
  generic-ассерт строки, oracle-kind, единый дамп при провале, cleanup guards,
  захват вывода `initdb` и запуск boot-матрицы один раз на прогон. В качестве
  эталона перевести на новый стиль 1-2 простые метрики: `archiver` как singleton
  и `database` как версионную метрику.
- **Фаза 1 — locks как эталон сложного случая.** Переписать BDD дерева ожиданий:
  конкретные W/H/locktype/mode, удержание состояния до assertions, oracle
  `pg_blocking_pids`, coverage для `pid=0`, multi-blocker и truncation.
- **Фаза 2 — остальные метрики.** Переводить метрики по одной на PR, каждую в
  новый стиль: SQL-вход, таблица ожиданий, выбранный тип oracle и дамп при
  провале. Очередь: `activity`, `bgwriter`, `wal`, `io`, `prepared_xacts`,
  `progress_vacuum`, `user_tables`, `user_indexes`, `statements`,
  `replication_instance`.

## 10. Definition of Done для любого BDD-сценария

Каждый новый или изменённый сценарий обязан:

- [ ] показывать domain SQL сетапа в `.feature` как docstring;
- [ ] утверждать конкретные значения колонок, привязанные к сетапу: реальные
      PID/OID, locktype, mode и другие проверяемые значения;
- [ ] указывать тип oracle: exact, transformed, subset, top-N/capped,
      window/tolerance или schema-only;
- [ ] сверять секцию с независимым PostgreSQL oracle там, где это behavioral
      scenario, а не только codec/layout test;
- [ ] удерживать состояние до конца snapshot и assertions, если сценарий зависит
      от live locks/session state;
- [ ] иметь cleanup для сессий, транзакций, temp DB/schema, extensions и
      background tasks;
- [ ] при провале дампить секцию таблицей, expected rows, oracle rows и логи
      `initdb`/`postgres`/коллектора;
- [ ] состоять из шагов, где каждый `Then` проверяет один контракт и даёт
      column-level diff;
- [ ] использовать version/slow/serial/extension tags, если они нужны сценарию;
- [ ] не перезагружать матрицу ради одного сценария.

Без этого чек-листа метрика не считается покрытой BDD.
