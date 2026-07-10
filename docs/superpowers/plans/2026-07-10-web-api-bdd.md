# PR-B: pg_kronika-web (HTTP /v1/* над ридером) + инкрементальный refresh + BDD-heavy

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** тонкий read-only HTTP-слой `pg_kronika-web` (axum) поверх query-слоя из
PR-A (`section`/`sections`/`logical_section`/`LocalDirSnapshot`). Плюс инкрементальный
near-real-time refresh журнала (секундный поллинг без пере-скана). Плюс кратное
расширение BDD: сценарии на уровне HTTP против живого PG-оракула.

**База:** ветка `feat/web-api` от main (8ff0f0c: PR-A read-слой + PR #57 BDD-readable-names).
Веб-бинарь сейчас заглушка (9 строк). BDD после #57 адресует секции по ИМЕНИ
(`section pg_stat_archiver ...`, `parse_section_ref` → type_id+label) — веб-API тоже
по имени, совпадает.

**Дизайн-документы (утверждены):** контракты ручек — `output_to_user/pgkronika-api-over-files-spec.md` (v2);
секундный поллинг — `output_to_user/pgkronika-active-parts-polling-research.md`.

## Global Constraints

- MSRV 1.96, edition 2024. Гейт: `export PATH="$HOME/.cargo/bin:$PATH"` затем
  `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
  && cargo test --workspace --exclude kronika-bdd && cargo run -p xtask -- check-deps`.
  Live-BDD (`make test-bdd`) — на финале и после BDD-задач (нужен Docker+PG-матрица).
- ⛔ Имплементер НЕ спавнит сабагентов (глобальное CLAUDE.md pre-commit-review к нему
  НЕ применяется — ревью делает контроллер). Коммитит сам сразу по зелёному гейту.
- check-deps: `pg_kronika-web` allow-list уже включает `kronika-reader`, `kronika-registry`,
  `kronika-format`, `kronika-store*` (НЕ `kronika-source-*`: PG-клиент/procfs в веб не заходят).
  axum/tokio/serde_json — сторонние крейты, границы workspace check-deps не трогает.
- clippy: `Eq` только без f64-полей. Комментарии/rustdoc/BDD English; коммиты русские без `Co-Authored-By`.
- Регистратор: веб отдаёт СЫРЬЁ (строки/окна/разрывы). Rate/пороги/аномалии — НЕ здесь.

## Утверждённые решения (не развилки)

1. **Секундный поллинг active.parts** (ресёрч): детекция ТОЛЬКО по `size` относительно
   `last_valid_len` + CRC кадра/тела как арбитр целостности. **inode и inotify отвергнуты**
   (нестабильны при блочной записи; reset = `truncate-in-place` `set_len(0)` сохраняет инод →
   inode бесполезен; inotify не на NFS/коалесит/overflow). `size==last`→noop; `size>last`→скан
   хвоста `[last_valid_len, size)`; `size<last`→reset→скан с 0. Torn-tail → offset НЕ двигать,
   повтор через тик. Seal всегда пишет новый `.pgm` → ловится сканом каталога независимо от
   размера журнала (закрывает ABA-край); стейл-проверка каталога на декоде — вторичная страховка.
2. **Снапшот держим** (умный B): `LocalDirSnapshot` в `AppState` под `ArcSwap`, фоновая
   tokio-таска раз в ~1с делает `refresh_incremental` и свапает. Таймер/политика — в бинаре,
   НЕ в библиотеке-ридере (ридер отдаёт факты).
3. **BDD веб** — in-process: axum `Router` как `tower::Service` без сокета (детерминизм, нет
   портов в PG-матрице). `bind+serve` — отдельный мелкий smoke.
4. **Веб + его BDD — один PR** (веб нечем принять без BDD).
5. Секция по ИМЕНИ; время — unix микросекунды (i64); StrId инлайн; read-only.

## Ручки /v1/* (путь · вызов ридера · JSON)

- `GET /v1/version` → `{ "api": "v1", "format_version": 1 }` (статика).
- `GET /v1/sources` → `units()` группировать по `source_id` → `{ "sources": [ {source_id, min_ts, max_ts, segments} ] }`.
- `GET /v1/sections` → каталог из реестра: на логическое имя `logical_section(name)` →
  `{ "sections": [ {name, semantics, sort_key:[...], columns:[{name,type,class}]} ] }`. Статично.
- `GET /v1/segments?source&from&to` → `units()` ∩ окно `source` → на единицу `unit_catalog(idx)`
  → `{ "segments": [ {segment_id, source_id, min_ts, max_ts, sections:[{name, rows}]} ] }`
  (`type_id`→имя через `section_name`; суммировать `rows` одинаковых имён).
- `GET /v1/section/{name}?source&from&to&limit&cursor` → `section(...)` → `SectionPage`→JSON.
  Имени нет в реестре → 404; битые `from/to/limit/cursor` → 400.
- `GET /v1/sections/batch?source&from&to&names=a,b,c&limit` → `sections(...)` → `{имя: страница}`.

**JSON-сериализация:**
- `OutRow` → объект `{колонка: значение}` в порядке union-колонок секции.
- `Value`: `I64/U64/F64/Ts` → число (Ts = i64 микросекунды); `Bool` → bool; `Str` → строка;
  `Blob` → `{text, truncated, full_len}`; `ListI32` → массив чисел; `Null` → null.
- `SectionPage` → `{section, source_id, rows:[...], gaps:[{from,to}], next_cursor: строка|null}`.
  `next_cursor` = `Cursor::encode()` (готовый opaque hex из PR-A).
- Ошибки → `{error, detail}`; коды: 404 UnknownSection, 400 BadCursor/битые параметры, 500 init-io.
  Стейл ридер деградирует в gaps (не ошибка) → веб отдаёт 200 с gaps.

**Расхождения спека↔PR-A (спека писалась ДО кода):** курсор — keyset (`Cursor::encode/decode`),
не `(segment_id,entry_idx,row_offset)`; gaps — `{from,to}` без причины; агрегация/ts-фильтр/сорт/limit
уже в `section()`/`sections()` — веб не повторяет.

## Задачи (SDD, по одной, TDD, гейт после каждой)

### T1 — инкрементальный refresh (format + reader)
`crates/kronika-format`: `scan_journal_streaming_from(reader, start_at, limits, resync_chunk)` —
как `scan_journal_streaming`, но старт с `pos=start_at` (не 0). `start_at=last_valid_len` — всегда
граница кадра. `scan_journal_streaming` = обёртка `..._from(reader, 0, ...)`.
`crates/kronika-reader` `LocalDirSnapshot::refresh_incremental(&mut self)`:
stat журнала (`active.parts`); `size==last_valid_len` И sealed-каталог не изменился → noop;
`size>last_valid_len` → до-сканировать хвост `[last_valid_len, size)`, дописать новые части;
`size<last_valid_len` ИЛИ журнал NotFound → reset → полный re-scan (offset:=0);
новые sealed `.pgm` (изменился список файлов) → пере-листинг sealed. Torn-tail (`ScanReport`
не продвинул `valid_len` до `size`) → `last_valid_len` НЕ двигать. Хранить `last_valid_len` в снапшоте.
**TDD:** дозапись части → `refresh_incremental` видит её, скан только хвоста (счётчик прочитанных
байт/тест-хук); reset (усечка журнала) → скан с 0, старые части убраны; torn-tail (недописанный
кадр) → часть не появляется, `last_valid_len` не сдвинут, следующий refresh после дозаписи видит;
новый sealed .pgm → появляется; noop когда ничего не менялось.

### T2 — веб-скелет + /v1/version + golden-харнес
`bins/pg_kronika-web`: deps axum + tokio(rt-multi-thread,macros) + serde_json + kronika-reader
+ kronika-registry. `AppState { dir: PathBuf, snapshot: ArcSwap<LocalDirSnapshot> }`. Фоновая
tokio-таска ~1с: `refresh_incremental` на клоне + `ArcSwap::store`. axum `Router` с `GET /v1/version`.
Golden-харнес: `build_part`-фикстура в temp-каталог → `Router` через `tower::ServiceExt::oneshot`
(без сокета) → сверка тела с committed JSON. `main()` bind+serve — тонкий, отдельный smoke.
check-deps зелёный с axum. **TDD:** golden `/v1/version`; харнес поднимает роутер на фикстурном
каталоге и отдаёт ответ.

### T3 — дискавери: /v1/sources, /v1/sections, /v1/segments
Реализовать три ручки (см. JSON выше) над `units()`/`logical_section`/`registry()`/`unit_catalog`.
**TDD golden:** sources (два источника, охваты); sections (форма каталога из реестра, детерм.
порядок); segments (окно, секции с верным rows, суммирование одинаковых имён).

### T4 — данные: /v1/section/{name}
`section(...)` → JSON (сериализация `Value`/`SectionPage`), коды ошибок (404/400). Дефолт-`limit`
1000, потолок 10000. **TDD golden (спека §4):** строка с числами; NULL-колонка→null; StrId-резолв;
усечённый Blob; пустое окно→пустые rows+gap; несуществующее имя→404; битый параметр→400.

### T5 — пагинация + батч
`cursor` в `/v1/section` (сквозь `Cursor::encode/decode`); `/v1/sections/batch`. **TDD golden:**
page1+page2 курсором покрывают все строки без дублей (через границу сегментов); батч 2 имён —
один ответ, каждая секция корректна.

### T6 — BDD-харнес для веба (на текущем post-#57 харнесе)
Расширить `crates/kronika-bdd`: тот же boot PG-матрицы → коллектор пишет сегмент в каталог →
поднять веб-`Router` in-process на этом каталоге (`tower::ServiceExt::oneshot`, без сокета) →
HTTP-шаги (`When GET /v1/...`) → ассерт JSON против ЖИВОГО PG-оракула (переиспользовать
oracle-механику harness/oracle.rs: exact/subset/floor/...; секции по ИМЕНИ, как ввёл #57).
ПЕРЕД реализацией — изучить обновлённые harness/{oracle,assert_row}.rs + steps/common.rs
(parse_section_ref, oracle-kind). **Сценарии на 1 метрику** (archiver singleton) сквозь HTTP.

### T7 — BDD-кратность
Шаблон web-сценариев на КАЖДУЮ метрику поверх сегментного уровня, переиспользуемые шаги:
`GET /v1/section/{name}` отдаёт строку с ожидаемым значением (оракул из живого PG); окно
`[from,to]` отсекает вне-оконное; sort-key порядок в JSON; `/v1/segments` верный rows; курсор
page1+page2 без дублей; батч 2 метрик; gaps на непокрытом окне. Покрыть плотно 3–4
репрезентативных: activity (multi-row), archiver (singleton), store_plans (оба форка _vadv/_ossc),
один OS (multi-scope). Вынести шаблон в общие шаги, размножить.

## Финал PR-B

После T7 — whole-branch ревью (opus): контракты ручек, тонкость веба (вся логика в ридере),
инкрементальный refresh корректен (torn/reset/size-only+CRC), границы check-deps, регистратор,
BDD покрывает HTTP-уровень против живого оракула, ЖЁСТКАЯ вычитка текста от нейрослопа
(комментарии/коммиты/описание PR). Затем `make test-bdd` зелёный → PR (target main), мерж по
зелёным воротам (гейт+ревью+BDD).
