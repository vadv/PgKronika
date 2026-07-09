# Read query-layer (PR-A) Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** query-слой в `kronika-reader` поверх фундамента #58: `section(name, source,
window, limit, cursor) -> SectionPage` — строки логической секции в окне, слитые по
многим единицам (sealed+live), с union layout-версий, резолвом `str_id`, курсором и
gaps. Без HTTP (PR-B) и BDD (PR-C).

**Design:** контракты в `output_to_user/pgkronika-read-api-bdd-plan.md` §1 (утверждены).
Слой живёт в `kronika-reader` (уже тянет `kronika-store` + `kronika-registry` +
`kronika-format`). Ветка `feat/read-query-layer` стекнута на `feat/kronika-store-foundation`
(#58); после мержа #58 — ребейз на main.

## Global Constraints

- MSRV 1.96, edition 2024. Гейт: `export PATH="$HOME/.cargo/bin:$PATH"` затем
  `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
  && cargo test --workspace --exclude kronika-bdd && cargo run -p xtask -- check-deps`.
- ⛔ Имплементер НЕ спавнит сабагентов (глобальное CLAUDE.md pre-commit-review к нему
  НЕ применяется — ревью делает контроллер). Коммитит сам сразу по зелёному гейту.
- check-deps: query-слой — модуль `kronika-reader`, новых зависимостей крейта не
  добавлять (registry/store/format уже есть). НЕ трогать границы других крейтов.
- clippy: `Eq` только без `f64`-полей (иначе `PartialEq`). Комментарии/rustdoc/тесты
  English; коммиты русские без `Co-Authored-By`.
- Регистратор: сырьё, окна, резолв. Без rate/дельт/аномалий.

## Реальные API (verbatim, для кода)

```rust
// kronika-registry
pub fn registry() -> &'static [TypeContract];
pub fn section_name(type_id: u32) -> Option<&'static str>;
pub fn decode_rows(type_id: u32, section: VerifiedSection) -> Result<Vec<Row>, CodecError>;
pub type Row = BTreeMap<&'static str, Cell>;
pub enum Cell { I16(i16),I32(i32),I64(i64),U32(u32),U64(u64),F64(f64),Bool(bool),Ts(i64),StrId(u64),ListI32(Vec<i32>),Null }
pub struct TypeContract { pub type_id: TypeId, pub name: &'static str, pub semantics: Semantics,
    pub columns: &'static [Column], pub sort_key: &'static [&'static str], pub deprecated: bool }
pub struct Column { pub name: &'static str, pub ty: ColumnType, pub class: ColumnClass, pub nullable: bool }
pub enum ColumnType { I8,I16,I32,I64,U8,U16,U32,U64,F32,F64,Bool,Ts,StrId,ListI32 }
impl TypeId { pub fn get(&self) -> u32; }
// kronika-reader (фундамент #58)
impl PgmUnit<R: ReadAt> { pub fn catalog(&self)->&Catalog; pub fn decode(&self,&Entry)->Result<DecodedSection,ReadError>; pub fn dictionary(&self)->Result<Dictionary,ReadError>; }
impl Dictionary { pub fn resolve(&self, str_id:u64)->Option<Resolved>; }
pub enum Resolved<'a> { String(&'a [u8]), Blob{bytes:&'a [u8], full_len:u64, truncated:bool} }
impl LocalDirSnapshot { pub fn open(&Path)->io::Result<Self>; pub fn refresh(&mut self)->io::Result<()>;
    pub fn units(&self)->Vec<UnitMeta>; pub fn unit_catalog(&self, idx:usize)->Option<&Catalog>;
    pub fn decode_unit(&self, idx:usize, entry_idx:usize)->Result<DecodedSection,ReadError>; pub fn warnings(&self)->&[StoreWarning]; }
pub struct UnitMeta { pub source_id:u64, pub min_ts:i64, pub max_ts:i64, pub live:bool }
pub enum ReadError { /* ... */ StaleSnapshot{unit_idx:usize}, Codec(CodecError), Io(io::Error), /* ... */ }
// kronika-format
pub struct Entry { pub type_id:u32, pub offset:u64, pub len:u64, pub rows:u32, pub crc32c:u32, pub flags:u32 }
pub struct Catalog { pub entries:Vec<Entry>, pub min_ts:i64, pub max_ts:i64, pub source_id:u64, pub format_version:u32 }
```

Секции формат сортирует по `sort_key` → строки одной `(unit, type_id-entry)` уже
отсортированы; это гарантия входа в k-merge.

---

## Task 1: reader-примитивы для именованного декода

**Files:** modify `crates/kronika-reader/src/unit.rs`, `crates/kronika-reader/src/snapshot.rs`, `crates/kronika-reader/src/lib.rs`.
**Produces:**
- `PgmUnit::decode_rows(&self, entry: &Entry) -> Result<Vec<Row>, ReadError>` — `verified_body(entry)` → `kronika_registry::decode_rows(entry.type_id, verified)`; dict-секции отвергать (как `decode`). `Row` реэкспортить из reader.
- `LocalDirSnapshot::unit_dictionary(&self, idx: usize) -> Result<Dictionary, ReadError>` — открыть `PgmUnit` выбранной единицы (sealed `File` / active `&[u8]`, как `decode_unit`, со стейл-проверкой для live) → `.dictionary()`.
- `LocalDirSnapshot::decode_unit_rows(&self, idx: usize, entry_idx: usize) -> Result<Vec<Row>, ReadError>` — как `decode_unit`, но через `decode_rows`.

**TDD:** тест — на фикстуре (sealed `.pgm` + active part через `build_part`) `decode_unit_rows` даёт строки с именованными ячейками, совпадающие для sealed и active; `unit_dictionary` резолвит известный `str_id`. Стейл: удалить active-журнал → `decode_unit_rows`/`unit_dictionary` → `StaleSnapshot`.

---

## Task 2: логическая секция (union версий)

**Files:** create `crates/kronika-reader/src/query/logical.rs`; wire `mod query` in `lib.rs`.
**Produces:**
```rust
pub struct LogicalColumn { pub name: &'static str, pub ty: ColumnType }
pub struct LogicalSection {
    pub name: &'static str,
    pub type_ids: Vec<u32>,          // возрастающе
    pub columns: Vec<LogicalColumn>, // union, порядок = первое появление по возр. type_id
    pub sort_key: &'static [&'static str],
}
pub fn logical_section(name: &str) -> Option<LogicalSection>;
```
Построение: `registry().iter().filter(|c| c.name == name)`, сорт по `type_id.get()`;
union колонок по имени (первое появление); при разном `ColumnType` одной колонки между
версиями — `panic!`/ошибка с диагностикой (нарушение реестра). `sort_key` брать общий;
если у версий различается — ошибка.

**TDD:** тест по РЕАЛЬНОМУ реестру — для каждого имени с ≥2 версиями `sort_key`
совпадает и нет конфликта `ColumnType` (если реестр нарушает — это находка, эскалировать
контроллеру). Юнит-тест: имя с 2 версиями (напр. активити) → union-колонки в правильном
порядке, версия-специфичные включены.

---

## Task 3: Value/Row-модель + `Cell -> Value`

**Files:** create `crates/kronika-reader/src/query/value.rs`.
**Produces:**
```rust
pub enum Value { Null, I64(i64), U64(u64), F64(f64), Bool(bool), Ts(i64),
    Str(String), Blob { text: String, full_len: u64, truncated: bool }, ListI32(Vec<i32>) }
pub type OutRow = Vec<(String, Value)>;   // по union-колонкам, отсутствующие = Null
pub struct Gap { pub from: i64, pub to: i64, pub reason: GapReason }
pub enum GapReason { CorruptJournalFrame, CorruptSegment, NoCoverage }
pub fn cell_to_value(cell: &Cell, dict: &Dictionary) -> Value;  // + флаг «id не найден» наружу
```
`cell_to_value`: `I16/I32→I64`? нет — сохранить ширину как в API-спеке (`I64/U64/F64/Bool/Ts/ListI32` прямо; `I16/I32→I64`, `U32→U64` расширить до нейтральных). `StrId(0)→Null`; `StrId(id)`→`dict.resolve`: `String`→`Value::Str` (UTF-8 lossy), `Blob`→`Value::Blob`; `id` нет в словаре → `Value::Null` + сигнал вызывающему (для gap/warning). `Cell::Null→Value::Null`.

**TDD:** каждый вариант `Cell`; `StrId(0)→Null`; `Str`; усечённый `Blob{truncated:true}`; `ListI32`; `id`-нет-в-словаре→Null+сигнал.

---

## Task 4: батч-ядро `sections()` за один проход + `open_unit` (без курсора)

Веб тянет снапшот из нескольких метрик окна — ядро должно открывать сегменты один
раз на батч, а не раз-на-метрику (§1.3a).

**Files:** modify `crates/kronika-reader/src/snapshot.rs` (`open_unit`); create `crates/kronika-reader/src/query/section.rs`.
**Produces:**
```rust
// snapshot.rs — открыть единицу ОДИН раз, переиспользовать для многих секций
pub enum OpenUnit { Sealed(PgmUnit<File>), Active(PgmUnit<Vec<u8>>) }
impl OpenUnit {
    pub fn catalog(&self) -> &Catalog;
    pub fn decode_rows(&self, entry:&Entry) -> Result<Vec<Row>, ReadError>;
    pub fn dictionary(&self) -> Result<Dictionary, ReadError>;
}
impl LocalDirSnapshot { pub fn open_unit(&self, idx:usize) -> Result<OpenUnit, ReadError>; }
// active: read_active_part байты один раз (стейл-проверка при open), дальше все секции из них.

// query/section.rs
pub struct SectionPage { pub section:String, pub source_id:u64, pub rows:Vec<OutRow>, pub gaps:Vec<Gap>, pub next_cursor:Option<Cursor> }
pub enum QueryError { UnknownSection(String), Read(ReadError) }
pub fn sections(snap:&mut LocalDirSnapshot, source:u64, from:i64, to:i64, names:&[&str], limit:usize)
    -> Result<BTreeMap<String, SectionPage>, QueryError>;   // &mut уже здесь: T6 навесит refresh-retry без ресигнатуры
pub fn section(snap:&mut LocalDirSnapshot, name:&str, source:u64, from:i64, to:i64, limit:usize)
    -> Result<SectionPage, QueryError>;   // = sections(&[name])[name]
```
(Курсор не вводить; `next_cursor: None`, `gaps: vec![]` — T5/T6.)

Алгоритм §1.3a (ОДИН проход): `logical_section` каждого имени (нет → `UnknownSection`);
отобрать единицы `snap.units()` `source_id==source` ∩ окно; **на единицу `open_unit` ОДИН
раз** (словарь из неё один раз) → для каждого имени: каждый `entry` матчащего `type_id`
(повтор = мультиокно) `OpenUnit::decode_rows` → `cell_to_value` по union-колонкам
(отсутствующие Null) → фильтр `row["ts"](Cell::Ts) ∈ [from,to]` → per-name аккумулятор;
после обхода — per-name k-merge (min-heap по `sort_key`) + `limit`. `section(name)` =
`sections(&[name])`. Пик памяти: `names × units × MAX_SECTION_ROWS`.

**TDD:** одна единица (порядок sort_key); мультиокно; merge 2 sealed; union v1+v3 (Null);
ts-фильтр; `limit`; неизвестное имя → `UnknownSection`; **батч 2 имён открывает каждую
единицу ОДИН раз** — счётчик вызовов `open_unit` (тест-хук/обёртка) == число единиц окна,
не имён×единиц.

---

## Task 5: cursor (seek по sort_key + tie_break) + пагинация

**Files:** create `crates/kronika-reader/src/query/cursor.rs`; extend `section()` в `section.rs`.
**Produces:**
```rust
pub struct Cursor(/* непрозрачный */);
impl Cursor { pub fn encode(&self)->String; pub fn decode(s:&str)->Result<Self,QueryError>; }
// SectionQuery получает pub cursor: Option<Cursor>
```
Курсор = `(source_id, sort_key_values:[Cell], tie_break)`; `tie_break=(ts:i64, type_id:u32)`
+ лексикографика остальных колонок при равенстве. Encode = base64 длина-префиксных
типизированных ячеек. `section()` при `Some(cursor)`: слить окно и пропустить строки с
`(sort_key, tie_break) <= cursor`; `next_cursor` от последней выданной, `None` если поток
исчерпан. Валидировать `source_id` курсора против запроса.

**TDD:** page1(limit=N)+page2(cursor) покрывают все строки без дублей/пропусков, включая
границу между единицами; курсор стабилен при повторе; битый курсор → `QueryError`;
курсор чужого source → ошибка.

---

## Task 6: stale-retry + gaps

**Files:** extend `section.rs`; `query/gaps.rs`.
**Produces:** `section()` — стейл-retry (§1.5): при `ReadError::StaleSnapshot` до 2 раз
`snap.refresh()` + пересбор; после лимита — исключить единицу + `Gap`. `gaps` (§1.6):
из `snap.warnings()` (пропущенный sealed → `CorruptSegment`), damages журнала
(`CorruptJournalFrame`), непокрытые интервалы окна (`[from,to]` минус объединение
`[min_ts,max_ts]` читаемых единиц источника → `NoCoverage`).

`snap` в `section()` уже `&mut LocalDirSnapshot` (сигнатура задана в T4) — `refresh` доступен.

**TDD:** стейл (заменить/удалить active между units и decode) → refresh+retry → консистентно;
битый sealed → `CorruptSegment`-gap, остальное отдано; окно вне единиц → `NoCoverage`-gap
+ пустые rows; частичное покрытие → gap на непокрытом хвосте.

---

## Финал PR-A

После T6 — whole-branch ревью (opus) диапазона `e845a2a..HEAD`: контракты §1 соблюдены,
merge/cursor/union/str_id/stale/gaps корректны, память ограничена, границы check-deps,
регистратор. Затем PR (target main, пометить «depends on #58», не мержить).
