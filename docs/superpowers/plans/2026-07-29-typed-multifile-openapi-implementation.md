# План реализации типизированного многофайлового OpenAPI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** описать реальные ответы всех 15 web-операций, экспортировать
самодостаточное доменное YAML-дерево и добавить локальную OpenAPI-driven
проверку против уже запущенного demo.

**Архитектура:** Rust response DTO и `OpenApiRouter` остаются единственным
источником истины. Production строит bundled `/openapi.json`, а отдельный
структурный exporter раскладывает тот же `OpenApi` по доменным YAML-файлам и
проверяет обратную сборку. Schemathesis читает runtime-документ, подставляет
demo-параметры и проверяет схемы и лёгкие признаки данных.

**Стек:** Rust 1.96, Axum 0.8, serde, serde_json, utoipa 5, utoipa-axum 0.2,
YAML-сериализация, Schemathesis через закреплённый `uvx`.

## Глобальные ограничения

- Не менять URL, HTTP-статусы и JSON существующего API.
- Все 15 операций получают именованную успешную response schema.
- Известные статусы перечисляются для каждой операции; `304` не имеет body.
- Каждый handler имеет ровно один tag из `core`, `sections`, `analytics`,
  `timeline`, `ui`.
- Committed YAML является сгенерированным артефактом, а не источником истины.
- Многофайловый exporter работает со структурированными значениями, не со
  строковыми заменами.
- `/healthz`, `/readyz` и `/metrics` остаются вне OpenAPI.
- Smoke работает только локально против уже запущенного demo и не входит в CI.
- Новые DTO не создают вторую полную копию HTTP-ответа.
- Любая коллекция ответа сохраняет действующие query/cache limits.
- Комментарии и rustdoc описывают контракт, а не пересказывают код.
- Обязательные проверки:
  `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`,
  `cargo run -p xtask -- check-deps`.

---

### Задача 1. Общие, core и sections response DTO

**Файлы:**

- Создать: `bins/pg_kronika-web/src/api_response.rs`
- Изменить: `bins/pg_kronika-web/src/lib.rs`
- Изменить: `bins/pg_kronika-web/src/serialize.rs`
- Изменить: `bins/pg_kronika-web/src/handlers/v1.rs`
- Изменить: `bins/pg_kronika-web/src/tests/version_diff.rs`
- Изменить: `bins/pg_kronika-web/src/tests/sections.rs`

**Интерфейсы:**

- `ApiValue` описывает все JSON-варианты `kronika_reader::Value`.
- `ApiRow` сериализуется как объект с динамическими именами колонок.
- `SectionPageResponse::from(&SectionPage)` заменяет `page_to_json`.
- `SectionDiffResponse::from_parts(...)` заменяет `section_diff_object`.
- `VersionResponse`, `SectionsResponse`, `SegmentsResponse`,
  `SectionsBatchResponse` являются успешными телами handlers.

- [ ] **Шаг 1. Добавить падающие тесты сериализации общих DTO**

Добавить тест, который строит все варианты `kronika_reader::Value`, переводит
их в `ApiValue` и сравнивает JSON с текущим `value_to_json`. Отдельно проверить
blob, nullable, динамическую строку section и `next_cursor`.

```rust
let response = SectionPageResponse::from(&page);
assert_eq!(
    serde_json::to_value(response).expect("serialize"),
    page_to_json(&page),
);
```

- [ ] **Шаг 2. Запустить тест и подтвердить RED**

```bash
cargo test -p pg_kronika-web --lib api_response
```

Ожидается ошибка импорта `crate::api_response`.

- [ ] **Шаг 3. Реализовать общие wire-типы**

Создать DTO:

```rust
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub(crate) enum ApiValue {
    Null,
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
    Text(String),
    Blob(BlobValue),
    ListI32(Vec<i32>),
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct BlobValue {
    text: String,
    full_len: u64,
    truncated: bool,
}
```

`ApiRow` и карты diff/batch получают ручной `PartialSchema`, если derive не
может выразить `additionalProperties` без потери реальной формы.

- [ ] **Шаг 4. Перевести core и sections handlers на DTO**

Заменить `Json<Value>` на конкретные `Json<T>`. Сохранить порядок через
`BTreeMap`. Для batch использовать newtype над
`BTreeMap<String, SectionPageResponse>`. Для diff определить:

```rust
struct SectionDiffResponse {
    section: String,
    identity: Vec<String>,
    series: Vec<DiffSeriesResponse>,
}
```

`DiffPointResponse` является untagged enum для value-point и nodata-point и
сохраняет текущие ключи `ts`, `delta`, `rate`, `dt_micros`, `nodata`.

- [ ] **Шаг 5. Проверить байтовую совместимость**

Запустить существующие golden HTTP-тесты:

```bash
cargo test -p pg_kronika-web --lib tests::version_diff
cargo test -p pg_kronika-web --lib tests::sections
cargo test -p pg_kronika-web --lib serialize::tests
```

Ожидается прежний JSON и прежние статусы.

- [ ] **Шаг 6. Проверить память и комментарии**

Убедиться, что conversion потребляет reader-значения или выполняется вместо
старого `serde_json::Value`, а не рядом с ним. Удалить комментарии, которые
только повторяют названия полей.

- [ ] **Шаг 7. Закоммитить DTO sections**

```bash
git add bins/pg_kronika-web/src/api_response.rs \
  bins/pg_kronika-web/src/lib.rs \
  bins/pg_kronika-web/src/serialize.rs \
  bins/pg_kronika-web/src/handlers/v1.rs \
  bins/pg_kronika-web/src/tests
git commit -m "refactor: типизировать ответы sections API"
```

### Задача 2. Analytics, timeline и UI response DTO

**Файлы:**

- Изменить: `bins/pg_kronika-web/src/serialize.rs`
- Изменить: `bins/pg_kronika-web/src/handlers/anomalies.rs`
- Изменить: `bins/pg_kronika-web/src/handlers/incidents.rs`
- Изменить: `bins/pg_kronika-web/src/incident_response.rs`
- Изменить: `bins/pg_kronika-web/src/overview/dto.rs`
- Изменить: `bins/pg_kronika-web/src/overview/health.rs`
- Изменить: `bins/pg_kronika-web/src/overview/handlers.rs`
- Изменить: `bins/pg_kronika-web/src/ui/catalog.rs`
- Изменить: `bins/pg_kronika-web/src/ui/data.rs`
- Изменить: `bins/pg_kronika-web/src/ui/heatmap.rs`
- Изменить: `bins/pg_kronika-web/src/ui/handlers.rs`
- Изменить: соответствующие файлы `bins/pg_kronika-web/src/tests/`

**Интерфейсы:**

- `AnomaliesResponse` и `IncidentsResponse` описывают текущие top-level keys.
- `OverviewResponseDto`, `EventsResponseDto`, `HealthResponseDto`,
  `ProjectionCatalog`, `ViewSummaryResponse`, `HeatmapResponse` реализуют
  `ToSchema`.
- Вложенные enums используют те же `serde(rename_all)` для wire и schema.

- [ ] **Шаг 1. Добавить RED-проверку успешных схем**

В `api_docs` test helper получить `openapi_document()` и потребовать, чтобы
операции `anomalies`, `incidents`, `overview`, `events`, `health`, `catalog`,
`summary`, `heatmap` имели `200.application/json.schema`.

```rust
assert!(success_schema(&document, "anomalies").is_some());
```

- [ ] **Шаг 2. Запустить RED**

```bash
cargo test -p pg_kronika-web --lib api_docs::tests::every_operation_has_a_success_schema
```

Ожидается отсутствие content у `200`.

- [ ] **Шаг 3. Типизировать существующие Serialize DTO**

Добавить `utoipa::ToSchema` всем публичным частям timeline/UI-ответов.
Сохранить `skip_serializing_if`, nullable-поля, строковые unix-микросекунды и
snake_case enums. Не добавлять `Clone`, если сериализация его не требует.

- [ ] **Шаг 4. Типизировать analytics JSON builders**

Заменить top-level `json!` на DTO. Динамические identity/series карты
используют `ApiRow`. Числа, которые текущий код заменяет на `null` при
NaN/Infinity, представлены как `Option<f64>`.

`IncidentsResponse` строится в `incident_response.rs`; существующие
ограниченные каталоги и vectors переносятся без дополнительного clone.

- [ ] **Шаг 5. Подключить успешные body к операциям**

Для каждого handler указать:

```rust
responses(
    (status = 200, description = "...", body = ConcreteResponse),
)
```

Добавить доменный tag. Фиксированные параметры получают полезные examples:
`pg_stat_database`, `pg_stat_database,pg_stat_wal`, `events`, `count`.

- [ ] **Шаг 6. Запустить domain-тесты**

```bash
cargo test -p pg_kronika-web --lib tests::anomalies
cargo test -p pg_kronika-web --lib tests::incidents
cargo test -p pg_kronika-web --lib tests::overview_timeline
cargo test -p pg_kronika-web --lib tests::ui_catalog
cargo test -p pg_kronika-web --lib tests::ui_data
```

Ожидаются прежние тела и статусы.

- [ ] **Шаг 7. Закоммитить остальные DTO**

```bash
git add bins/pg_kronika-web/src
git commit -m "refactor: типизировать аналитические ответы API"
```

### Задача 3. Полный OpenAPI операций и статусов

**Файлы:**

- Изменить: `bins/pg_kronika-web/src/api_error.rs`
- Изменить: `bins/pg_kronika-web/src/api_docs.rs`
- Изменить: все файлы с `#[utoipa::path]`
- Создать или изменить: `bins/pg_kronika-web/src/tests/openapi.rs`
- Изменить: `bins/pg_kronika-web/src/tests/mod.rs`

**Интерфейсы:**

- `ApiError` остаётся общей схемой error body.
- Общие response aliases/`IntoResponses` не добавляют невозможные статусы.
- `openapi_document()` содержит 15 уникальных operationId и пять tags.

- [ ] **Шаг 1. Написать таблицу ожидаемых операций**

В тесте определить 15 записей:

```rust
const OPERATIONS: &[(&str, &str)] = &[
    ("version", "core"),
    ("sections", "sections"),
    ("segments", "sections"),
    ("section_data", "sections"),
    ("sections_batch", "sections"),
    ("section_diff", "sections"),
    ("sections_batch_diff", "sections"),
    ("anomalies", "analytics"),
    ("incidents", "analytics"),
    ("overview", "timeline"),
    ("events", "timeline"),
    ("health", "timeline"),
    ("catalog", "ui"),
    ("summary", "ui"),
    ("heatmap", "ui"),
];
```

Потребовать ровно один tag, JSON schema у `200`, content type и отсутствие
`default`, который скрывает известные ошибки.

- [ ] **Шаг 2. Добавить тест фактических статусов**

На основе `ErrorCode::status()` и ветвей handlers закрепить применимые
`400`, `404`, `410`, `413`, `500`, `503`; для catalog закрепить `304` без
content. Проверить ссылки error responses на `ApiError`.

- [ ] **Шаг 3. Запустить RED**

```bash
cargo test -p pg_kronika-web --lib tests::openapi
```

Ожидаются `default` responses и отсутствующие tags/statuses.

- [ ] **Шаг 4. Обновить path-аннотации**

Добавить tag, полные описания параметров и фактические responses. Повторяемые
error response definitions оформить макросом или `IntoResponses`, только если
получившийся YAML сохраняет явный список статусов операции.

- [ ] **Шаг 5. Проверить runtime registry**

Добавить in-process запросы к `/openapi.json` и `/swagger-ui/`. Убедиться, что
документ совпадает с `openapi_document()` после JSON-нормализации.

- [ ] **Шаг 6. Запустить web package**

```bash
cargo test -p pg_kronika-web --lib
cargo clippy -p pg_kronika-web --all-targets -- -D warnings
```

- [ ] **Шаг 7. Закоммитить полный контракт**

```bash
git add bins/pg_kronika-web/src
git commit -m "feat: описать полные ответы web API"
```

### Задача 4. Структурный многофайловый exporter

**Файлы:**

- Создать: `bins/pg_kronika-web/src/openapi_export.rs`
- Изменить: `bins/pg_kronika-web/src/lib.rs`
- Переписать: `bins/pg_kronika-web/examples/export_openapi.rs`
- Создать: `bins/pg_kronika-web/tests/openapi_export.rs`
- Изменить: `bins/pg_kronika-web/Cargo.toml`

**Интерфейсы:**

- `export_openapi_tree(document: &OpenApi, output: &Path) -> Result<(), ExportError>`.
- `bundle_openapi_tree(root: &Path) -> Result<serde_json::Value, ExportError>`
  служит round-trip проверкой созданного формата.
- Exporter пишет только фиксированные domain-файлы из спецификации.

- [ ] **Шаг 1. Написать RED-тест разбиения**

Построить маленький `OpenApi` с двумя domains, общей схемой и циклической
ссылкой. Потребовать `paths/sections.yaml`, `paths/ui.yaml`,
`schemas/common.yaml`, корректные относительные `$ref` и семантическое
равенство bundle исходнику.

- [ ] **Шаг 2. Написать RED-тест ошибок**

Проверить ошибки для отсутствующего/двойного/неизвестного tag,
повторного `operationId`, неразрешимой схемы и успешного ответа без schema.
Создать прежний sentinel-файл и доказать, что failed export его не меняет.

- [ ] **Шаг 3. Запустить RED**

```bash
cargo test -p pg_kronika-web --test openapi_export
```

Ожидается отсутствие `openapi_export`.

- [ ] **Шаг 4. Реализовать partition и graph ownership**

Сериализовать `OpenApi` в `serde_json::Value`, работать с maps/arrays и
проверять их типы. Для каждой операции собрать достижимые
`#/components/schemas/*`; схема одного domain принадлежит domain, схема
нескольких domains принадлежит `common`.

- [ ] **Шаг 5. Реализовать refs и round-trip**

Root paths ссылаются на `./paths/<domain>.yaml#/<escaped-path>`. Root
components ссылаются на `./schemas/<owner>.yaml#/<SchemaName>`. В path/schema
фрагментах ссылки переписываются на `../schemas/<owner>.yaml#/<SchemaName>`.
JSON Pointer tokens экранируются по RFC 6901.

- [ ] **Шаг 6. Реализовать атомарную запись**

Записать соседний временный каталог, перечитать и сравнить нормализованный
bundle, затем заменить destination. Ошибка до замены сохраняет старый каталог.
Сортировать maps до YAML-сериализации.

- [ ] **Шаг 7. Запустить exporter-тесты**

```bash
cargo test -p pg_kronika-web --test openapi_export
cargo test -p pg_kronika-web --lib tests::openapi
```

- [ ] **Шаг 8. Закоммитить exporter**

```bash
git add bins/pg_kronika-web/src/openapi_export.rs \
  bins/pg_kronika-web/src/lib.rs \
  bins/pg_kronika-web/examples/export_openapi.rs \
  bins/pg_kronika-web/tests/openapi_export.rs \
  bins/pg_kronika-web/Cargo.toml Cargo.lock
git commit -m "feat: экспортировать OpenAPI по доменным файлам"
```

### Задача 5. Generated tree, Makefile, CI и документация

**Файлы:**

- Удалить: `bins/pg_kronika-web/swagger.yaml`
- Создать: `bins/pg_kronika-web/openapi/openapi.yaml`
- Создать: `bins/pg_kronika-web/openapi/README.md`
- Создать: `bins/pg_kronika-web/openapi/paths/*.yaml`
- Создать: `bins/pg_kronika-web/openapi/schemas/*.yaml`
- Изменить: `Makefile`
- Изменить: `.github/workflows/ci.yml`
- Изменить: `README.md`
- Изменить: `README.ru.md`
- Изменить: `bins/pg_kronika-web/README.md`
- Изменить: `bins/pg_kronika-web/README.ru.md`

**Интерфейсы:**

- `make openapi` обновляет committed tree.
- `make openapi-bundle` пишет
  `target/pg-kronika-web/openapi.yaml`.
- CI проверяет diff всего `bins/pg_kronika-web/openapi`.

- [ ] **Шаг 1. Обновить команды**

Переименовать phony target `swagger` в `openapi`; добавить bundle target с
явным output mode exporter-а. Не сохранять bundle в source tree.

- [ ] **Шаг 2. Сгенерировать дерево**

```bash
make openapi
make openapi
git diff --check
```

Второй запуск не меняет файлы.

- [ ] **Шаг 3. Обновить CI freshness gate**

Проверять наличие `openapi/openapi.yaml`, выполнять `make openapi` и
`git diff --exit-code -- bins/pg_kronika-web/openapi`.

- [ ] **Шаг 4. Обновить английскую и русскую документацию**

Указать точные команды, пути, `/openapi.json`, `/swagger-ui/` и правило
«YAML сгенерирован, вручную не редактировать». Синхронизировать смысл языковых
версий.

- [ ] **Шаг 5. Проверить ссылки и generated diff**

```bash
rg -n 'swagger\\.yaml|make swagger' README.md README.ru.md \
  bins/pg_kronika-web .github Makefile
make openapi-bundle
test -s target/pg-kronika-web/openapi.yaml
```

Первый `rg` не находит устаревших инструкций.

- [ ] **Шаг 6. Закоммитить интеграцию**

```bash
git add Makefile .github/workflows/ci.yml README.md README.ru.md \
  bins/pg_kronika-web/README.md bins/pg_kronika-web/README.ru.md \
  bins/pg_kronika-web/openapi bins/pg_kronika-web/swagger.yaml
git commit -m "docs: разложить OpenAPI по доменным файлам"
```

### Задача 6. Локальный Schemathesis smoke

**Файлы:**

- Создать: `scripts/demo-api-smoke.sh`
- Создать: `scripts/demo_api_smoke.py`
- Создать: `scripts/test_demo_api_smoke.py`
- Изменить: `Makefile`
- Изменить: `README.md`
- Изменить: `README.ru.md`

**Интерфейсы:**

- `make demo-api-smoke` использует
  `DEMO_WEB_URL`, по умолчанию `http://127.0.0.1:18081`.
- Python module регистрирует Schemathesis generation hooks и class-based
  `DemoEvidence`.
- Скрипт не управляет lifecycle demo.

- [ ] **Шаг 1. Написать unit-тест evidence**

Синтетическими response JSON проверить evidence для всех 15 operationId,
включая допустимо пустые `anomalies`, `incidents`, `events` с непустой
coverage/quality metadata. Не выполнять сетевые запросы в unit-тесте.

- [ ] **Шаг 2. Запустить RED**

```bash
uv run scripts/test_demo_api_smoke.py
```

Ожидается отсутствие `scripts/demo_api_smoke.py`.

- [ ] **Шаг 3. Реализовать hooks и custom check**

Generation hook подставляет общее текущее окно, `pg_stat_database`,
`pg_stat_database,pg_stat_wal`, `events` и `count`. `DemoEvidence` запоминает
короткий текст доказательства для каждого успешного operationId и в
`after_run` отклоняет пропуски.

- [ ] **Шаг 4. Реализовать shell preflight**

Проверить наличие `uvx` и `curl`, дождаться `/healthz`, `/readyz` и непустых
segments с ограниченным timeout. Затем запустить закреплённый Schemathesis с
`--phases examples`, runtime `/openapi.json` и extension module.

- [ ] **Шаг 5. Добавить Makefile и инструкции**

Добавить `demo-api-smoke` без зависимости от `demo-up`. В README дать
последовательность:

```bash
make demo-up
make demo-api-smoke
make demo-down
```

- [ ] **Шаг 6. Проверить локальную часть без demo**

```bash
uv run scripts/test_demo_api_smoke.py
shellcheck scripts/demo-api-smoke.sh
```

Если `shellcheck` недоступен, выполнить `bash -n` и явно отметить отсутствие
shellcheck в итоговой проверке.

- [ ] **Шаг 7. Выполнить live smoke**

При доступном локальном demo:

```bash
make demo-api-smoke
```

Ожидается 15 подтверждённых operations. Если demo не запущен, не запускать его
скрыто и отметить live-проверку как невыполненную.

- [ ] **Шаг 8. Закоммитить smoke**

```bash
git add scripts/demo-api-smoke.sh scripts/demo_api_smoke.py \
  scripts/test_demo_api_smoke.py Makefile README.md README.ru.md
git commit -m "test: добавить локальный smoke web API"
```

### Задача 7. Финальная проверка

**Файлы:** все изменённые файлы ветки.

- [ ] **Шаг 1. Проверить generated artifacts**

```bash
make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
make openapi-bundle
```

- [ ] **Шаг 2. Выполнить project gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```

- [ ] **Шаг 3. Провести memory-bounds review**

Для каждого нового conversion подтвердить отсутствие одновременного старого
`Value` и нового DTO. Для exporter-а подтвердить ограничение числом
скомпилированных operations/schemas. Для smoke подтвердить хранение только
коротких evidence.

- [ ] **Шаг 4. Провести comment-quality review**

Удалить narration-комментарии, проверить rustdoc на контракт, единицы,
nullable и реальные ограничения.

- [ ] **Шаг 5. Проверить scope и историю**

```bash
git status --short
git log --oneline origin/main..HEAD
git diff --check origin/main...HEAD
```

Изменения не затрагивают несвязанные crates и не меняют HTTP JSON.
