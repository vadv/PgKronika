# Дизайн PostgreSQL row mapper

## Цель

Сократить повторяющийся код маппинга `tokio_postgres::Row -> *Row` в
`kronika-source-pg`.

Существующий поток обработки сохраняется:

- SQL для конкретной версии остается в модуле каждого сборщика;
- сборщики по-прежнему возвращают owned raw rows до интернирования в словарь;
- функции `to_vN` остаются чистыми и удобными для тестирования;
- клиент БД остается `tokio-postgres`.

Приоритеты:

1. производительность;
2. явные ошибки;
3. безопасный, ревьюируемый код.

## Вне Области Работ

- Переход на `sqlx`, Diesel, Cornucopia или другой SQL-слой.
- Proc-macro derive в первом PR.
- Генерация SQL.
- Изменения в registry codec generation.
- Изменения collector row limits, top-N selection или memory policy.

## Схема Маппера

Добавляется небольшой модуль `pg_row` и `macro_rules!` DSL внутри
`kronika-source-pg`.

Макрос описывает соответствие поля Rust колонке SQL и генерирует:

- структуру с индексами колонок;
- конструктор, который резолвит ожидаемые колонки из `Statement::columns()`;
- метод `read`, который строит raw row через `Row::try_get(index)`.

Пример:

```rust
pg_row_mapper! {
    ActivityCols(version: ActivityVersion) => ActivityRow {
        ts: i64 = "ts_us",
        pid: i32 = "pid",
        datname: Option<String> = "datname",

        query_id: Option<i64> = "query_id"
            if matches!(version, ActivityVersion::V3),
    }
}
```

Эквивалентная форма сгенерированного кода:

```rust
struct ActivityCols {
    ts: PgCol<i64>,
    pid: PgCol<i32>,
    datname: PgCol<Option<String>>,
    query_id: Option<PgCol<Option<i64>>>,
}

impl ActivityCols {
    fn new(
        version: ActivityVersion,
        columns: &[tokio_postgres::Column],
    ) -> Result<Self, PgRowError> {
        Ok(Self {
            ts: PgCol::required("ActivityRow", "ts", "ts_us", columns)?,
            pid: PgCol::required("ActivityRow", "pid", "pid", columns)?,
            datname: PgCol::required("ActivityRow", "datname", "datname", columns)?,
            query_id: if matches!(version, ActivityVersion::V3) {
                Some(PgCol::required("ActivityRow", "query_id", "query_id", columns)?)
            } else {
                None
            },
        })
    }

    fn read(&self, row: &tokio_postgres::Row) -> Result<ActivityRow, PgRowError> {
        Ok(ActivityRow {
            ts: self.ts.get(row)?,
            pid: self.pid.get(row)?,
            datname: self.datname.get(row)?,
            query_id: match &self.query_id {
                Some(col) => col.get(row)?,
                None => None,
            },
        })
    }
}
```

DSL не привязывается к row struct. В Rust нет runtime tags как в Go; field
attributes потребовали бы proc macro. Отдельный macro block оставляет `*Row`
обычной data-структурой и держит version conditions рядом с SQL.

## Сбор Данных

Переведенные сборщики подготавливают statement, строят column map, выполняют
statement и декодируют строки:

```rust
pub async fn collect_activity(
    client: &Client,
    major: u32,
) -> Result<(ActivityVersion, Vec<ActivityRow>), PgCollectError> {
    let version = activity_version(major);
    let stmt = client.prepare(activity_query(version)).await?;
    let cols = ActivityCols::new(version, stmt.columns())?;
    let rows = client.query(&stmt, &[]).await?;
    let parsed = rows
        .iter()
        .map(|row| cols.read(row))
        .collect::<Result<Vec<_>, PgRowError>>()?;
    Ok((version, parsed))
}
```

`prepare` валидирует ожидаемый набор колонок даже при пустом результате.
Отсутствующие и дублирующиеся aliases падают до декодирования строк.

## Ошибки

Сгенерированный код маппера не должен вызывать `Row::get`. Все декодирование
идет через `try_get` и возвращает ошибки с именами row, field и column.

```rust
pub enum PgRowError {
    MissingColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
    },
    DuplicateColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
    },
    DecodeColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
        source: tokio_postgres::Error,
    },
}
```

Дубли выбранных имен колонок являются ошибкой. Выбор первого совпавшего alias
скрыл бы ошибку в SQL.

Переведенным сборщикам нужен тип ошибки, который несет сбои query и
row-mapping:

```rust
pub enum PgCollectError {
    Query(tokio_postgres::Error),
    Row(PgRowError),
}
```

Публичный return type меняется только у сборщиков, переведенных в этом PR.
Callers, которым нужен SQLSTATE matching, должны смотреть `PgCollectError::Query`.

## Производительность

Текущий ручной путь делает name lookup для каждого mapped field каждой строки.
Новый путь резолвит имя каждой колонки один раз на result set и декодирует
строки по `usize` index.

Наибольший выигрыш ожидается на широких или многорядных sources:

- `pg_stat_statements`;
- `pg_stat_user_tables`;
- `pg_locks`.

Маппер хранит только индексы и static metadata. Он не убирает owned string
allocation, который уже нужен raw row structs.

## Безопасность

- Только safe Rust.
- Сгенерированный код маппера использует `try_get`, а не `get`.
- SQL aliases остаются явными в DSL, например `ts: i64 = "ts_us"`.
- Version-gated columns остаются явными в месте маппинга.
- Раскрытый код должен оставаться читаемым через `cargo expand`.

## Migration Inventory

Маппер покрывает весь слой `tokio_postgres::Row -> *Row`, а не только pilot
collectors.

Явные `row_from_pg` targets:

- `activity`;
- `database`;
- `io`;
- `locks`;
- `progress_vacuum`;
- `statements`;
- `store_plans`;
- `user_indexes`;
- `user_tables`.

Inline row mappings с тем же шаблоном:

- `prepared_xacts`;
- `replication_details` (`ReplicaRow`, `SlotRow`);
- `settings`.

Первый PR не обязан переводить весь inventory, но API маппера должен подходить
для всех этих форм: version-gated columns, renamed SQL aliases, nullable
значения, singleton rows и multi-row snapshots.

## Объем Первого PR

Первый PR вводит маппер и переводит только pilot collectors:

1. Добавить `pg_row` primitives и unit tests.
2. Добавить `pg_row_mapper!`.
3. Перевести `activity` как малый pilot.
4. Перевести `statements` как широкий pilot.
5. Обновить только затронутых callers для `PgCollectError`.

Не переводить все collectors в первом PR. Более широкая миграция идет после
ревью API, ошибок и сгенерированного кода.

## Тестирование

- `PgCol::required`: случаи found, missing и duplicate-name из
  `Statement::columns()`.
- Error display/context для `MissingColumn`, `DuplicateColumn` и
  `DecodeColumn`.
- Покрытие сгенерированного маппера для required и version-gated fields.
- Существующие тесты `to_vN` остаются behavioral tests для registry row
  construction.
- Обычные gates: `cargo fmt --all --check`, strict clippy,
  `cargo test --workspace` и `cargo run -p xtask -- check-deps`.

## Границы Памяти

Маппер добавляет одну column map на result set: один `usize` плюс static
metadata на mapped field. Он не делает per database row allocations сверх
существующих owned raw row structs.

Объем result rows остается под существующими collector SQL limits, top-N
selection или `Vec<Row>`, который уже материализует `tokio-postgres`.
