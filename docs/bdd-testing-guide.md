# BDD-тесты PgKronika: стандарт и план чистки

Документ задаёт стандарт для BDD-сценариев PgKronika и план перевода старых
проверок на этот стандарт. Цель BDD-слоя - проверять данные коллектора против
наблюдаемого состояния PostgreSQL, а не только внутреннюю согласованность
записанного сегмента.

## 1. Проблемы, которые исправляет стандарт

### 1a. Самопроверяющиеся ассерты

Старая проверка дерева ожиданий (`check_wait_tree`) сводилась к поиску любой
строки с ребром и любой строки с pid блокера:

```rust
let waiter = rows.iter().find(|r| !r.blocked_by.is_empty())?;   // любая строка с ребром
let blocker_pid = waiter.blocked_by[0];
ensure!(rows.iter().any(|r| r.pid == blocker_pid && r.blocked_by.is_empty()));
ensure!(waiter.has_awaited_lock);
```

Такой ассерт не привязан к backend'ам, которые открыл сценарий. Он не проверяет
тип lock (`transactionid`), режим lock и конкретного блокера. Булев
`has_awaited_lock` проверяется как флаг присутствия, а не как значение с
доменным смыслом. Поэтому правдоподобная, но неверная структура может пройти
тест.

Та же форма встречалась в других метриках: один широкий `Then` вроде
`each cluster seals X rows` скрывал и SQL-подготовку, и проверку в Rust.
`.feature` не объяснял, какие входные данные созданы и какие значения должны
оказаться в секции.

### 1b. Недостаточная диагностика harness

Старый harness выбрасывал stdout/stderr `initdb` через `Stdio::null()`. При
ошибке оставался только `exit status: 1`, без причины отказа. After-hook мог
показать только `server.log` уже созданного кластера, поэтому падение `initdb`
оставалось без полезного контекста.

Матрица PostgreSQL также поднималась заново на каждый сценарий. Это увеличивало
число конкурентных запусков `initdb` и делало отказы на boot неотличимыми от
ошибок в доменной проверке.

## 2. Правила BDD-сценария

Нарушение любого правила ниже считается блокером ревью для нового или
изменённого сценария.

1. **Вход виден в `.feature`.** SQL, создающий состояние сценария (DDL, seed
   data, долгие транзакции, блокирующий statement), пишется как docstring.
   В Rust остаётся инфраструктура шага: открыть сессию, получить
   `pg_backend_pid()`, дождаться нужного состояния и зарегистрировать cleanup.
2. **Ассерт проверяет конкретные значения.** Ожидаемая строка задаётся таблицей
   в `.feature`. Плейсхолдеры вроде `[H]` разрешаются в реальные backend PID или
   OID. Нельзя заменять проверку значения на «секция присутствует», «строка
   похожа на дерево» или булев `has_X`.
3. **Оракулом является PostgreSQL.** Сценарий называет тип сравнения: exact,
   transformed, subset, floor, ceiling, top-N/capped или schema-only. Монотонный
   счётчик сверяется оконной парой шагов: floor-чтение до снапшота, ceiling —
   после, записанное значение обязано лежать между ними.
   Exact-сравнение нельзя применять там, где коллектор делает version mapping,
   truncation, top-N, nullable-преобразование или нормализацию timestamp.
4. **Live-состояние удерживается до конца проверки.** Для lock/session
   сценариев waiter/blocker остаются живыми до snapshot и oracle checks.
   Cleanup выполняется после assertion даже при падении.
5. **Провал содержит контекст.** Ошибка должна включать декодированную секцию,
   ожидаемые строки, oracle rows и хвост логов `initdb`/`postgres`/коллектора.
   Большие логи можно обрезать в сообщении, но полный файл должен сохраняться
   рядом с данными прогона.
6. **Один `Then` проверяет один контракт.** Шаг может проверять одну строку или
   одну таблицу, если failure report показывает diff по колонкам. Он не должен
   смешивать boot, snapshot, dictionary, labels и семантические assertions.

## 3. Типы оракулов

Каждый сценарий выбирает тип оракула и называет его в шаге или таблице
ожиданий.

- **Exact oracle.** Значения секции совпадают с результатом PostgreSQL query
  после нормализации типов. Подходит для простых catalog/stat rows.
- **Transformed oracle.** Query возвращает значения в форме, которую хранит
  коллектор: `NULL`, unit conversion, `unix usec`, version layout,
  enum/string mapping. Сравнение остаётся equality, но преобразование явно
  записано в oracle SQL.
- **Subset oracle.** Значения из query должны присутствовать в секции, но секция
  может содержать дополнительные строки. Подходит для instance-wide sources и
  шумных системных представлений.
- **Top-N/capped oracle.** Сценарий проверяет выбранные строки, лимит, порядок,
  truncation/coverage marker и отсутствие скрытого overflow. Реализация
  добавляется вместе с первым сценарием, которому нужен этот тип.
- **Window/tolerance oracle.** Для timestamp и cumulative counters проверяется
  допустимое окно или monotonic relation, а не точное равенство.
- **Ceiling oracle.** PostgreSQL query даёт верхнюю границу; каждое ненулевое
  значение секции должно быть `<=` этой границы. Подходит для `stats_reset <=
  snapshot_time`.
- **Schema-only oracle.** Проверяет codec/layout contract без live PostgreSQL
  state. Такой сценарий не считается behavioral BDD для метрики.

Oracle SQL не должен копировать collector SQL. Он должен проверять смысл через
более простой raw query, точечный catalog lookup или явную проекцию состояния
PostgreSQL.

## 4. Эталон сценария для locks

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

`"H"`, `"W"` и `[H]` обозначают backend PID, полученные через
`pg_backend_pid()`. Шаг `runs and blocks` ждёт `wait_event_type = 'Lock'` с
ограниченным timeout. После assertions cleanup отменяет statement W и
откатывает транзакцию H.

## 5. Переиспользуемый harness

- **Именованные сессии.** `World` хранит постоянные подключения по имени. Шаги
  `session "X" runs ...` выполняют SQL, запоминают `pg_backend_pid()` и
  регистрируют cleanup guard. Вариант `runs and blocks` запускает statement в
  отдельной задаче и ждёт lock wait state без фиксированного sleep.
- **Снимок.** `When the collector snapshots the segment` только запускает
  snapshot коллектора. Подготовка состояния должна быть видна в предыдущих
  шагах.
- **Ассерт строки.** `Then section <type_id> has [exactly one] row for session
  "X": <table>` декодирует секцию, находит строку по PID сессии `X`, сверяет
  именованные колонки и разрешает `[Name]` placeholders.
- **Ассерт по ключу.** Для метрик без PID используется явный key: `datid`,
  `relid`, `indexrelid`, `queryid`, `slot_name` и т.п. Key должен быть виден в
  `.feature`.
- **Оракул.** `Then section <type_id> <column> matches the <oracle-kind> oracle:
  <SQL docstring>` выполняет oracle SQL и сверяет результат с секцией по
  объявленному типу.
- **Дамп при провале.** Общий dump печатает decoded rows, expected rows, oracle
  rows и хвост логов подпроцессов.

Generic steps допустимы как транспорт. Смысл сценария остаётся в `.feature`:
SQL-вход, ожидаемые значения, oracle kind и ключ строки.

## 6. Изоляция, матрица и теги

- **Матрица поднимается один раз на прогон или worker.** Повторный `initdb` на
  каждый сценарий запрещён.
- **Сценарий изолирует своё состояние.** По умолчанию используется отдельная
  база или схема с уникальным именем. Для instance-wide метрик сценарий явно
  описывает допустимые shared side effects.
- **Cleanup обязателен.** Открытые транзакции, блокирующие statements, temp
  databases/schemas, roles, extensions и background tasks закрываются в
  after-hook.
- **Параллельность явная.** Сценарии, которые держат locks, меняют
  instance-wide state или зависят от timing, получают `@serial`.
- **Версии явные.** Сценарии используют теги `@pg10`, `@pg14`, `@pg16`, `@pg18`
  или version guards, если контракт зависит от версии PostgreSQL.
- **Дорогие сценарии помечаются.** Используются `@slow`, `@matrix`,
  `@requires_extension`, `@lock`, чтобы runner мог выбирать набор.

## 7. Надёжность и диагностика harness

1. **Захватывать вывод `initdb` и `postgres`.** `run_initdb` пишет stdout+stderr
   в файл или буфер. При провале этот вывод попадает в ошибку.
2. **Не выбрасывать stderr коллектора.** Ошибки collector process должны быть
   частью failure report.
3. **Логи доступны при любом падении.** `DEBUG=1` может расширять детализацию,
   но минимальный набор логов должен писаться всегда.
4. **Ограничивать объём сообщения.** Failure message содержит полезный tail; full
   raw output остаётся в данных прогона.
5. **`log_statement=all` включается осознанно.** Он полезен для диагностики, но
   нужно ограничивать размер логов и не печатать секреты/DSN.
6. **Падать с контекстом.** Ошибка уровня `open sealed segment` без секции,
   oracle rows и логов недостаточна.

## 8. Запрещённые анти-паттерны

- широкий step `seals X rows`, который скрывает setup и assert;
- domain SQL setup, спрятанный в Rust;
- самосогласованные checks и проверка правдоподобности вместо конкретных
  значений;
- булев `has_X` вместо проверяемого значения;
- копирование collector SQL в oracle SQL без независимой проверки смысла;
- перезагрузка матрицы на каждый сценарий;
- выброшенный вывод подпроцессов (`Stdio::null()` у `initdb`/`postgres`);
- generic step text, который скрывает доменную причину сценария;
- сценарии, зависящие от порядка выполнения других сценариев.

## 9. План чистки

План описывает порядок перевода старых сценариев на этот стандарт.

- **Фундамент.** Общий harness: именованные сессии, snapshot step,
  generic row assertion, oracle kinds, failure dump, cleanup guards, захват
  вывода `initdb` и boot матрицы один раз на прогон.
- **Сценарии на новом стандарте.** Уже переведены `collector`/
  `bgwriter-checkpointer`, `pg_stat_wal`, `pg_stat_archiver`, smoke,
  `pg_stat_activity`, `pg_stat_database`, `pg_stat_io`, `pg_prepared_xacts`,
  `pg_stat_progress_vacuum`, `pg_stat_statements`, `replication_instance`,
  `pg_stat_user_tables`/`pg_stat_user_indexes`, `connection_pool` и
  PG14+ layout для locks (`1_011_002`).
- **Отложено.** PG10-13 layout для locks (`1_011_001`) остаётся codec-level
  покрытием вне live matrix. `top-n` и `schema` oracle kinds реализуются вместе
  с первым сценарием, которому действительно нужны эти сравнения.

## 10. Definition of Done для BDD-сценария

Каждый новый или изменённый сценарий обязан:

- [ ] показывать domain SQL setup в `.feature` как docstring;
- [ ] утверждать конкретные значения колонок, привязанные к setup: реальные
      PID/OID, locktype, mode и другие проверяемые значения;
- [ ] указывать oracle kind: exact, transformed, subset, floor, ceiling,
      top-N/capped или schema-only; для монотонных счётчиков — оконная пара
      шагов (floor до снапшота, ceiling после);
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

Без этого чек-листа метрика не считается покрытой behavioral BDD.
