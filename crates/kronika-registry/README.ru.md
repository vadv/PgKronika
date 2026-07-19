# kronika-registry

[English version](README.md)

`kronika-registry` задаёт смысл PGM sections: назначает стабильные type ids,
описывает схемы и data semantics, содержит typed Parquet codecs для writer и
reader.

## Type contract

`TypeContract` связывает один `type_id` со следующими данными:

- logical name и признак deprecation;
- порядок колонок, физические типы, nullability и column classes;
- collection semantics;
- sort key и series identity;
- collection gates и row-specific overrides.

Type id записывается как `C_SSS_VVV`: класс, номер источника и layout version.
Текущие классы: snapshot (`1`), event (`2`), dictionary (`3`) и chart (`10`).
Несовместимая раскладка получает новую версию. Несколько versions могут иметь
одно logical name и объединяться reader.

`ColumnClass` задаёт дальнейшую обработку. Cumulative values идут в diff,
gauges читаются напрямую; identity, timestamp, constant и label columns имеют
свои invariants. Nullable value означает отсутствие, а не ноль.

## Кодеки секций

Каждая зарегистрированная строка реализует закрытый trait `Section`. Внутренний
proc-macro `kronika-derive` создаёт contract, Arrow schema, Parquet
encoder/decoder и time range из одной annotated struct. Сторонний crate не
может регистрировать произвольные типы.

`VerifiedSection` владеет bytes после проверки CRC вызывающим кодом. Registry
не зависит от PGM framing, но нормальный API не передаёт непроверенные bytes в
Parquet. Generic decode возвращает positional `Row`/`Cell` или Arrow batches,
не создавая map для каждой строки.

## Invariants и limits

`lint` проверяет ids, timestamp columns, sort/identity keys, совместимость
type/class и semantics. `lint_references` проверяет targets collection gate и
row overrides по всему реестру.

Каждый codec ограничен:

- `MAX_SECTION_ROWS = 65_536`;
- `MAX_SECTION_BYTES = 8 MiB`;
- `MAX_ROW_GROUPS = 16`.

Encode отклоняет лишние rows до построения Arrow arrays. Decode проверяет
размер и Parquet metadata до materialization. `BytesPool` переиспользует
ограниченные input buffers, но не кэширует распакованные Arrow arrays.

## Ответственность за качество данных

Факты PostgreSQL/Linux, влияющие на интерпретацию, находятся в contracts:
layout versions, identity, reset sources, entry epochs и collection gates.
Например, timing под `track_io_timing=off` становится `NotCollected`, а не
измеренным нулём.

Registry не выполняет SQL, не выбирает расписание, не пишет сегменты, не
сканирует store и не формирует HTTP. Список типов находится в
[`../../docs/type-registry.md`](../../docs/type-registry.md); при расхождении
каноничны [`src/lib.rs`](src/lib.rs) и `src/codec/`.
