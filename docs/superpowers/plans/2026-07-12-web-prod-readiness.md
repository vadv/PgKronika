# Прод-готовность pg_kronika-web — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Каждая задача — свежий имплементер по TDD, коммит по зелёному гейту, task-review контроллером. Шаги отмечаются `- [ ]`.

**Goal:** сделать `pg_kronika-web` пригодным к эксплуатации: пробы, сигнал возраста данных, Prometheus-метрики и структурные логи, опциональная Basic Auth, раздача встроенной статики — не меняя контракт `/v1/*`.

**Architecture:** роутер и хендлеры остаются в библиотеке `pg_kronika_web`, но `lib.rs` (1174 строки) режется на модули. `public`-роутер (`/healthz`,`/readyz`,`/metrics`) + `protected`-роутер (`/v1/*`+статика) с опциональным `route_layer(auth)`. Фоновая refresh-таска пишет `last_refresh`/`iterations` в общее состояние; величины возраста считаются в момент скрейпа `/metrics`.

**Tech Stack:** Rust 1.96, axum 0.8, tokio, `metrics`+`metrics-exporter-prometheus`, `tracing`+`tracing-subscriber`, `tower-http`, `rust-embed`, `base64`, `subtle`.

Спека: `docs/superpowers/specs/2026-07-12-web-prod-readiness-design.md` (коммит 51e607b). Ветка `feat/web-prod-readiness` (уже создана, поверх main d8e8dff; на ней спека + Cargo.lock-фикс).

## Global Constraints

- Гейт (ДО коммита каждой задачи): `export PATH="$HOME/.cargo/bin:$PATH"` затем `cargo fmt --all -- --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace --exclude kronika-bdd` && `cargo run -p xtask -- check-deps`.
- Комментарии/rustdoc/тесты — English; коммиты — русский; БЕЗ `Co-Authored-By`.
- Строгий clippy: all+pedantic+nursery+cargo+часть restriction. `.expect()`/`.unwrap()` — только в тестах. `missing_assert_message` НЕ срабатывает в `#[test]` (проверено в PR-B). Комментарии — минимум, контракт/невидимый инвариант, не пересказ кода.
- Новые сторонние зависимости (metrics/tracing/rust-embed/base64/subtle/tower-http) границы check-deps kronika-* НЕ трогают. check-deps allow-list `pg_kronika-web` не меняется.
- Платформа Linux (process-метрики через `/proc/self/*` допустимы).
- Тонкий `bin` уже имеет `#![allow(unused_crate_dependencies, reason=...)]` — сохранить.

## File Structure (после всех задач)

```
bins/pg_kronika-web/
  Cargo.toml               # + новые deps
  static/index.html        # плейсхолдер UI (rust-embed)
  src/
    lib.rs                 # pub app(state, auth, metrics_handle) -> Router; pub AppState; склейка
    handlers/
      mod.rs               # pub(crate) use ...
      v1.rs                # version/sources/sections/segments/section_data/sections_batch (перенос из lib.rs)
      probes.rs            # healthz, readyz
      metrics.rs           # GET /metrics -> render + data_age/reader_age/units/RSS/fds на скрейпе
      static_.rs           # rust-embed asset handler + SPA fallback
    serialize.rs           # value_to_json/row_to_json/page_to_json + column_type_name/_class_name/semantics_name (перенос)
    params.rs              # parse_u64/parse_i64/parse_limit/parse_cursor/bad_request (перенос)
    auth.rs                # AuthConfig, check_basic_auth, auth_layer
    startup.rs             # staleness, metric_labels, parse_basic_auth, WebConfig::from_env/validate
    main.rs                # тонкий
```

`AppState = { snapshot: Arc<ArcSwap<LocalDirSnapshot>>, last_refresh: Arc<AtomicU64>, refresh_loop_iterations: Arc<AtomicU64>, stale_after: Duration }`.

---

## Task 1: Распил `lib.rs` на модули (чистый рефактор, без новой функциональности)

Сначала разложить существующий монолит по модулям — дальше фичи добавляются в чистую структуру. Поведение и публичный API не меняются; существующие 20 тестов проходят.

**Files:**
- Create: `bins/pg_kronika-web/src/{serialize.rs, params.rs}`, `bins/pg_kronika-web/src/handlers/{mod.rs, v1.rs}`
- Modify: `bins/pg_kronika-web/src/lib.rs` (оставить `app()`, `AppState`, `mod`-декларации, `#[cfg(test)]` интеграционные тесты роутера)

**Interfaces — Produces:**
- `pub(crate) fn value_to_json(&CellValue) -> serde_json::Value`, `row_to_json(&OutRow) -> Value`, `page_to_json(&SectionPage) -> Value` (в `serialize.rs`)
- `pub(crate) fn parse_u64/parse_i64(&HashMap<String,String>, &str) -> Result<_, (StatusCode, Json<Value>)>`, `parse_limit`, `parse_cursor`, `bad_request(&str) -> (StatusCode, Json<Value>)` (в `params.rs`)
- `pub(crate) async fn version/sources/sections/segments/section_data/sections_batch(...)` (в `handlers/v1.rs`) — сигнатуры как сейчас
- `pub struct AppState`, `pub fn app(state: AppState) -> Router` (в `lib.rs`, без изменений сигнатуры на этой задаче)

- [ ] **Step 1** Перенести чистые сериализаторы (`value_to_json`/`row_to_json`/`page_to_json`/`column_type_name`/`column_class_name`/`semantics_name`) и их юнит-тесты в `serialize.rs`. В `lib.rs` — `mod serialize;` и `use serialize::*` где нужно.
- [ ] **Step 2** Перенести парсеры (`parse_u64`/`parse_i64`/`parse_limit`/`parse_cursor`/`bad_request`) и их юнит-тесты в `params.rs`.
- [ ] **Step 3** Перенести хендлеры `/v1/*` в `handlers/v1.rs` (`mod.rs`: `pub(crate) mod v1;`). Импорты reader/registry — в `v1.rs`.
- [ ] **Step 4** В `lib.rs` оставить `AppState`, `app()` (роутер зовёт `handlers::v1::*`), интеграционные `#[cfg(test)]` (fixture_response/serve и голдены) — они проверяют роутер целиком.
- [ ] **Step 5** Гейт зелёный (20 тестов проходят без изменений значений). Коммит: «Веб-крейт распилен на модули».

Замечание: `#[cfg(test)]`-юниты переезжают ВМЕСТЕ со своими функциями (в `serialize.rs`/`params.rs`); интеграционные (через `app()`) остаются в `lib.rs`. Публичные `app`/`AppState` — без изменений сигнатуры (расширим в T3/T7).

---

## Task 2: Чистые функции старта + расширение `AppState`

**Files:**
- Create: `bins/pg_kronika-web/src/startup.rs`
- Modify: `bins/pg_kronika-web/src/lib.rs` (`AppState` поля; `mod startup;`)

**Interfaces — Produces:**
- `pub(crate) fn staleness(now_secs: u64, last_refresh_secs: u64, stale_after: Duration) -> bool` — `true` если устарел (`now - last > stale_after`, насыщение при `last>now` → не устарел).
- `pub(crate) struct WebConfig { pub dir: PathBuf, pub addr: String, pub basic_auth: Option<(String,String)>, pub stale_after: Duration, pub log: String }`; `pub(crate) fn WebConfig::from_env() -> Result<WebConfig, String>` (валидирует: битый `KRONIKA_WEB_BASIC_AUTH` без `:`/пустой user → Err; невалидный `STALE_AFTER_S` → Err; отсутствие DIR/ADDR — как сейчас в main).
- `pub(crate) fn parse_basic_auth(raw: &str) -> Result<(String,String), String>` — split по первому `:`.
- `AppState` расширен: `last_refresh: Arc<AtomicU64>`, `refresh_loop_iterations: Arc<AtomicU64>`, `stale_after: Duration`. `AppState::new` принимает начальные значения; тест-конструктор для инъекции `last_refresh`.

- [ ] **Step 1** Тесты `staleness`: свежий(`now=100,last=99,after=10s`)→false; устаревший(`now=100,last=80,after=10s`)→true; ровно на пороге; `last>now`(рассинхрон)→false. Каждый assert с сообщением.
- [ ] **Step 2** Реализовать `staleness`. Гейт по этому юниту.
- [ ] **Step 3** Тесты `parse_basic_auth`: `"u:p"`→`("u","p")`; `"u:p:x"`→`("u","p:x")`; нет `:`→Err; пустой user `":p"`→Err.
- [ ] **Step 4** Реализовать `parse_basic_auth` + `WebConfig::from_env`+валидация. Тесты `WebConfig` через `std::env` не делать (глобал); вынести чистую `WebConfig::parse(dir,addr,basic_auth_raw,stale_raw,log) -> Result<WebConfig,String>` и тестировать её (битый basic_auth/stale → Err).
- [ ] **Step 5** Расширить `AppState` (поля + `#[derive(Clone)]` сохранить; `Arc<AtomicU64>` клонируется дёшево). Обновить существующие тест-конструкторы `AppState::new`. Гейт (20 тестов + новые юниты).
- [ ] **Step 6** Коммит: «Чистые функции конфигурации и готовности веб-сервиса».

---

## Task 3: Пробы `/healthz`, `/readyz`

**Files:**
- Create: `bins/pg_kronika-web/src/handlers/probes.rs`
- Modify: `handlers/mod.rs` (`pub(crate) mod probes;`), `lib.rs` (`app()` добавляет `/healthz`,`/readyz`)

**Interfaces — Consumes:** `AppState.last_refresh`, `AppState.stale_after`, `startup::staleness`.
**Produces:** `pub(crate) async fn healthz() -> impl IntoResponse` (200 `{"status":"ok"}`); `pub(crate) async fn readyz(State<AppState>) -> impl IntoResponse` (200/`503` + тело `{"ready":bool,"seconds_since_refresh":u64}`), время берётся `SystemTime::now()`.

- [ ] **Step 1** Тест (in-process oneshot) `healthz` → 200, тело `{"status":"ok"}`.
- [ ] **Step 2** Тест `readyz`: сконструировать `AppState` с `last_refresh` = `now` → 200 `ready:true`; с `last_refresh` = `now - 3600` (stale_after 10s) → 503 `ready:false`.
- [ ] **Step 3** Реализовать `healthz`/`readyz` (readyz: `now - last_refresh.load(Relaxed)`, сравнить через `staleness`). Добавить роуты в `app()`.
- [ ] **Step 4** Гейт зелёный. Коммит: «Пробы liveness и readiness».

---

## Task 4: Метрики `/metrics` + структурные логи

**Files:**
- Create: `bins/pg_kronika-web/src/handlers/metrics.rs`
- Modify: `Cargo.toml` (+`metrics="0.24"`, `metrics-exporter-prometheus="0.16"`, `tracing="0.1"`, `tracing-subscriber={version="0.3",features=["env-filter","json"]}`, `tower-http={version="0.6",features=["trace"]}`), `handlers/mod.rs`, `lib.rs` (`app(state, metrics_handle)`, request-метрики-слой, TraceLayer), `startup.rs` (`metric_labels`)

**Interfaces — Produces:**
- `pub(crate) fn metric_labels(method: &str, matched_path: Option<&str>) -> (String, &'static str)` — путь = `matched_path` или `"other"` (чистая, тестируемая).
- `pub(crate) async fn metrics_handler(State<AppState>, Extension<PrometheusHandle>) -> impl IntoResponse` — на скрейпе выставляет `kronika_web_data_age_seconds` (`now - max(units().max_ts)`, если юнит нет → пропустить/0), `kronika_web_units_total`, `kronika_web_reader_age_seconds` (`now - last_refresh`), `process_resident_memory_bytes`/`process_open_fds` (из `/proc/self/statm` и `/proc/self/fd`), затем `handle.render()`.
- `pub fn app(state: AppState, metrics_handle: PrometheusHandle) -> Router` (сигнатура расширена; auth добавит T5).

**Метрики (imена/типы):** `kronika_web_requests_total{method,path,status}` counter; `kronika_web_request_duration_seconds{method,path}` histogram (бакеты `0.001,0.005,0.01,0.05,0.1,0.5,1,2.5,5,10,30`, через `PrometheusBuilder::set_buckets_for_metric`); `kronika_web_refresh_errors_total`, `kronika_web_refresh_loop_iterations_total` counter; `kronika_web_data_age_seconds`, `kronika_web_units_total`, `kronika_web_reader_age_seconds`, `kronika_web_store_warnings`, `kronika_web_store_damages`, `kronika_web_inflight_requests`, `process_resident_memory_bytes`, `process_open_fds` gauge; `kronika_web_build_info{version,format_version}` gauge=1.

- [ ] **Step 1** Тесты `metric_labels`: `("GET", Some("/v1/section/{name}"))`→`("GET","/v1/section/{name}")`; `("GET", None)`→`("GET","other")`.
- [ ] **Step 2** Реализовать `metric_labels`.
- [ ] **Step 3** Request-метрики-слой: middleware из `axum::middleware::from_fn`, читает `MatchedPath` (extension) → `metric_labels`, инкрементит `counter!`/`histogram!` по завершении, ведёт `inflight` gauge (inc на входе, dec на выходе). Метка `path` — из `MatchedPath`, НЕ `uri().path()`.
- [ ] **Step 4** `metrics_handler`: выставить gauge на скрейпе + `handle.render()`; `content-type: text/plain; version=0.0.4`.
- [ ] **Step 5** Тест (oneshot, с установленным per-тест recorder через `OnceLock`): `/metrics` → 200, тело содержит `kronika_web_requests_total`, `kronika_web_data_age_seconds`, `kronika_web_reader_age_seconds`, `kronika_web_units_total`. Значения не проверяем (глобал-синглтон).
- [ ] **Step 6** Гейт. Коммит: «Prometheus-метрики и структурные логи веб-сервиса». (TraceLayer + tracing-subscriber init выполняется в main T7; здесь — слой метрик + `/metrics`; per-request span подключит T7.)

---

## Task 5: Basic Auth (опциональная, всё-или-ничего)

**Files:**
- Create: `bins/pg_kronika-web/src/auth.rs`
- Modify: `Cargo.toml` (+`base64="0.22"`, `subtle="2"`), `lib.rs` (`app(state, auth, metrics_handle)`; public/protected-композиция), `startup.rs`/`WebConfig` (basic_auth уже есть из T2)

**Interfaces — Consumes:** `WebConfig.basic_auth: Option<(String,String)>`.
**Produces:**
- `pub(crate) struct AuthConfig { expected_header: String }`; `AuthConfig::new(user,pass) -> Self` (предвычисляет `"Basic "+base64(user:pass)`).
- `pub(crate) fn check_basic_auth(header: Option<&str>, cfg: &AuthConfig) -> bool` — константное сравнение (`subtle::ConstantTimeEq` по байтам).
- `pub(crate) fn auth_layer(cfg: AuthConfig) -> impl Layer` (или `from_fn_with_state`) — 401 + `WWW-Authenticate: Basic` при промахе.
- `pub fn app(state: AppState, auth: Option<AuthConfig>, metrics_handle: PrometheusHandle) -> Router` — финальная сигнатура. `public` = `/healthz`,`/readyz`,`/metrics`; `protected` = `/v1/*` (+статик из T6), с `.route_layer(auth_layer)` только при `Some`; `public.merge(protected)`; TraceLayer/metrics-слой — `.layer()` снаружи.

- [ ] **Step 1** Тесты `check_basic_auth`: `AuthConfig::new("u","p")`; заголовок `Some("Basic <base64(u:p)>")`→true; неверный pass→false; нет заголовка→false; не-`Basic`→false; битый base64→false.
- [ ] **Step 2** Реализовать `AuthConfig`/`check_basic_auth` (base64 encode ожидаемого при `new`; входящий сравнивать целиком константно).
- [ ] **Step 3** `auth_layer` (middleware): извлечь `Authorization`, `check_basic_auth`, при промахе `401`+`WWW-Authenticate`.
- [ ] **Step 4** Пересобрать `app()`: public/protected-роутеры, `route_layer` при `Some(auth)`. Обновить существующие тест-вызовы `app(...)` (передавать `None` auth + тестовый metrics handle).
- [ ] **Step 5** Тесты (oneshot): auth `None` → `/v1/version` открыт (200). auth `Some` → `/v1/version` без заголовка → 401, с верным → 200; `/healthz`,`/readyz`,`/metrics` → 200 без заголовка даже при `Some`.
- [ ] **Step 6** Гейт. Коммит: «Опциональная Basic Auth: всё-или-ничего с публичным списком».

---

## Task 6: Встроенная статика + SPA-fallback

**Files:**
- Create: `bins/pg_kronika-web/src/handlers/static_.rs`, `bins/pg_kronika-web/static/index.html`
- Modify: `Cargo.toml` (+`rust-embed={version="8",features=["mime-guess"]}`), `handlers/mod.rs`, `lib.rs` (`protected`-роутер: `.fallback(static_handler)`)

**Interfaces — Produces:** `pub(crate) async fn static_handler(uri: Uri) -> impl IntoResponse` — путь без ведущего `/`; найдено в embed → тело+`content-type`+кэш (хэш-ассеты — `Cache-Control: max-age=31536000`, `index.html` — `no-cache`); не найдено И путь НЕ под `/v1/` → `index.html` (200); путь под `/v1/` неизвестный → уже 404 роутером (fallback НЕ перехватывает известные префиксы — `/v1/*` роуты объявлены, промах внутри `/v1` даёт 404 JSON, не fallback). Embedded struct: `#[derive(RustEmbed)] #[folder="static/"] struct Assets;`.

- [ ] **Step 1** `static/index.html` — минимальный плейсхолдер (`<!doctype html><title>pg_kronika-web</title>`).
- [ ] **Step 2** Тест (oneshot): `GET /index.html` → 200 `text/html`; `GET /` → 200 (SPA-fallback на index.html); `GET /nonexistent-ui-route` → 200 index.html; `GET /v1/nope` → 404 JSON (не index.html).
- [ ] **Step 3** Реализовать `static_handler` + `Assets`. Подключить `.fallback(static_handler)` на `protected`-роутере.
- [ ] **Step 4** Гейт. Коммит: «Раздача встроенной статики с SPA-fallback».

Замечание: `/v1/*` роуты объявлены явно в `protected`; неизвестный `/v1/...` НЕ попадёт в fallback только если пути не матчат — для этого `/v1/section/{name}` и прочие покрывают формы; произвольный `/v1/xxx` (не матчащий ни один роут) попадёт в fallback → нужно в `static_handler` вернуть 404 JSON для путей с префиксом `/v1/`. Реализовать эту проверку в хендлере.

---

## Task 7: Тонкий `main` — конфиг, инициализация, refresh-таска, старт

**Files:**
- Modify: `bins/pg_kronika-web/src/main.rs`

**Interfaces — Consumes:** `WebConfig::from_env`, `AuthConfig`, `AppState`, `app(state, auth, metrics_handle)`, `staleness`.

- [ ] **Step 1** `main`: (1) `WebConfig::from_env()` — при `Err` → eprintln + `exit(2)` (fail-fast первым, до I/O); (2) init `tracing_subscriber` (JSON, EnvFilter из `cfg.log`); (3) `PrometheusBuilder` с бакетами → `install_recorder()` → `PrometheusHandle`; выставить `build_info` gauge; (4) `LocalDirSnapshot::open(&cfg.dir)?`; (5) `AppState` (initial `last_refresh`=now, `iterations`=0, `stale_after`); (6) стартовый баннер через `tracing::info!` (адрес, auth on/off, порог, версия+format_version, путь стора); (7) spawn refresh-таска: цикл `sleep→iterations.fetch_add(1)→match refresh_incremental{Ok→last_refresh.store(now)+обновить store_warnings/damages gauge; Err→counter refresh_errors_total + tracing::warn!}` — БЕЗ `eprintln!`; (8) `auth = cfg.basic_auth.map(|(u,p)| AuthConfig::new(u,p))`; (9) `axum::serve(...).with_graceful_shutdown(signal).await` (как в #60) + `handle.abort()` refresh-таски после serve.
- [ ] **Step 2** Убедиться: ни одного `eprintln!` в main/lib/таске (всё через tracing). Smoke: `cargo run -p pg_kronika-web -- <tmpdir с сегментом>` поднимается, `curl /healthz`→200 (ручная проверка описана, не автотест).
- [ ] **Step 3** Гейт зелёный (весь воркспейс). Коммит: «Тонкий бинарь: конфиг, инициализация метрик/логов, refresh-таска».

---

## Task 8 (финал): whole-branch ревью, спец-ревью, PR

- [ ] **Step 1** Гейт полностью зелёный на HEAD ветки.
- [ ] **Step 2** Whole-branch opus-ревью диффа `main..HEAD` (корректность, инварианты §Спека, язык, тесты). Блокеры → починить.
- [ ] **Step 3** ЧЕТЫРЕ спец-ревью кода (как для плана): DBA, DevOps, Rust performance, Rust architect по диффу ветки. Блокеры → починить.
- [ ] **Step 4** Создать PR (`feat/web-prod-readiness` → `main`), описание поведенческое (что даёт, без перечисления файлов).
- [ ] **Step 5** ПОСЛЕ создания PR — жёсткая антислоп-вычитка PR-текста/коммитов/комментариев: СОКРАЩАТЬ и УПРОЩАТЬ (директива владельца), не только убирать ИИ-маркеры.
- [ ] **Step 6** Мерж при зелёном (`gh pr merge --merge --admin` — обход биллинг-красного CI; авторитет = локальный гейт).

## Self-Review (покрытие спеки)

- §1 Пробы → T3. §2 Состояние → T2 (AppState) + T7 (таска пишет). §3 Метрики+логи → T4 (+ T7 subscriber/refresh-метрики). §4 Auth → T5. §5 Статика → T6. §6 Конфиг+старт → T2 (WebConfig) + T7 (main). §Структура (распил) → T1. §Тесты → в каждой задаче. §Инварианты → проверяются на whole-branch/спец-ревью (T8).
- Порядок зависимостей: T1(структура)→T2(чистые+AppState)→T3(пробы)→T4(метрики, расширяет app→+handle)→T5(auth, финальная app→+auth)→T6(статика в protected)→T7(main склеивает всё)→T8(финал). Каждая задача — зелёный гейт, независимо ревьюибельна.
