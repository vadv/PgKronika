# Спека: прод-готовность pg_kronika-web

Ветка: `feat/web-prod-readiness` над `main` (d8e8dff). Крейт `bins/pg_kronika-web`.

## Цель

`pg_kronika-web` (PR #60) отдаёт JSON-API, но не готов к эксплуатации: нет проб, аутентификации, наблюдаемости за сервисом; устаревание данных молчит; статику (будущий UI) отдавать нечем. Этот PR закрывает это, не меняя контракт `/v1/*`.

## Scope

**В PR:** пробы liveness/readiness; сигнал возраста данных; метрики и структурные логи; HTTP Basic Auth; раздача встроенной статики; валидация конфига и стартовый баннер; разбиение `lib.rs` на модули.

**Вне PR:** TLS и rate-limit/timeout — задача обратного прокси; perf-рефакторы ридера (отдельный PR, сперва benchmarks); сам UI (только механизм и плейсхолдер); OpenAPI-схема (utoipa позже); lazy-start при недоступном на старте сторе.

## Компоненты

### 1. Пробы

- `GET /healthz` → всегда `200` пока процесс жив. Тело `{"status":"ok"}`. Без auth.
- `GET /readyz` → `200` если refresh-loop здоров (last_refresh не старше порога), иначе `503`. Без auth.
- `readyz` отвечает «веб может читать стор» — это то, что нужно оркестратору для снятия трафика; за свежесть данных отвечает `data_age` (§3), а не readyz.
- Порог `STALE_AFTER_S` дефолт 10 (10× интервал refresh) — буфер против единичного пропуска планирования. Без отдельного stat каталога: свежесть последнего успешного refresh уже отражает «стор читается».

### 2. Состояние готовности (в `AppState`)

- `last_refresh: Arc<AtomicU64>` — unix-секунды последнего `Ok(refresh_incremental)`, `Ordering::Relaxed`.
- `refresh_loop_iterations: Arc<AtomicU64>` — счётчик итераций таски. Растёт ⇒ таска жива; позволяет отличить «таска мертва/висит» от «стор сбоит» (тогда растёт и `refresh_errors_total`).

### 3. Наблюдаемость

Величины возраста считаются в момент скрейпа (`now − last`), а не пушатся из таски: иначе при мёртвой таске gauge замерзает и алерт не срабатывает.

Метрики `/metrics` (Prometheus):
- `kronika_web_requests_total{method,path,status}` — counter.
- `kronika_web_request_duration_seconds{method,path}` — histogram, бакеты `0.001..30s` (профиль бимодальный: version — микросекунды, section с большим окном — секунды).
- Метка `path` = шаблон маршрута (`axum::extract::MatchedPath`: `/v1/section/{name}`, `/v1/segments`, …), не сырой `uri().path()`; несматченное и статика → бакет `other`. Сырой путь дал бы неограниченную кардинальность под внешним трафиком.
- `kronika_web_data_age_seconds` (gauge) = `now − max(units().max_ts)` — возраст свежих данных, сигнал «коллектор молчит». `UnitMeta.max_ts` уже в снапшоте.
- `kronika_web_units_total` (gauge) = `units().len()` — детект пустого стора.
- `kronika_web_reader_age_seconds` (gauge) = `now − last_refresh` — reader-liveness (веб может читать), не свежесть данных.
- `kronika_web_refresh_errors_total`, `kronika_web_refresh_loop_iterations_total` — counter.
- `kronika_web_store_warnings`, `kronika_web_store_damages` — gauge (целостность журнала).
- `kronika_web_inflight_requests` — gauge (лимитера конкурентности нет — отсутствие обязано быть наблюдаемым).
- `process_resident_memory_bytes`, `process_open_fds` — process-collector (RSS важен из-за клона снапшота на запрос).
- `kronika_web_build_info{version,format_version}` gauge=1.

Реализация: фасад `metrics` + `metrics-exporter-prometheus`. Recorder ставится в `main` (процесс-синглтон); тесты проверяют метки чистой функцией `metric_labels(...)`, не значения через глобал. Порог алерта `data_age` — порядка каденса коллектора (не `STALE_AFTER_S`); правило Alertmanager — вне PR.

Логи: `tracing` + `tracing-subscriber` (JSON) + `tower_http::trace::TraceLayer` с request_id. Per-request span на уровне `debug` (дефолт `info` не означает строку лога на каждый запрос). Все `eprintln!` из #60 переводятся на `tracing`. Уровень из `KRONIKA_WEB_LOG` (дефолт `info`).

### 4. Аутентификация — опциональная, всё-или-ничего

По умолчанию API открыт всем: auth выключена. Включается заданием env `KRONIKA_WEB_BASIC_AUTH="user:password"`. Когда включена, Basic Auth требуется на ВСЁ, кроме публичного списка `/healthz`, `/readyz`, `/metrics` (оркестратору и Prometheus нужен доступ без учёток). То есть под auth попадают все `/v1/*` и статика.

- Композиция (идиома axum): `public`-роутер = `/healthz`, `/readyz`, `/metrics`; `protected`-роутер = `/v1/*` + статик-fallback, с `route_layer(auth)`, который навешивается только когда auth включена. `public.merge(protected)`; `TraceLayer`/metrics — снаружи через `.layer()`. `route_layer` не превращает 404 в 401, поэтому публичные пути мимо auth по построению.
- Проверка: ожидаемый заголовок `Basic <base64>` предвычисляется при старте; входящий сравнивается константно по времени, оба поля; промах → `401` + `WWW-Authenticate: Basic`.
- Учётки: одна env `KRONIKA_WEB_BASIC_AUTH="user:password"`, парсинг по первому `:` (пароль может содержать `:`). Не задан → auth выключена. Задан, но битый (нет `:` / пустой user) → fail-fast на старте.
- НЕТ fail-fast по адресу и bind-vs-auth-матрицы: открытость по умолчанию, в том числе на сети, — намеренная.

### 5. Встроенная статика

- `bins/pg_kronika-web/static/` зашивается `rust-embed` (release — встроено, debug — с диска для итерации UI без пересборки). Плейсхолдер `index.html`.
- Роутинг: служебные пути как выше; статик-хендлер как `.fallback()` `protected`-роутера. Известный путь → ассет с content-type; не найдено → `index.html` (SPA). Неизвестный `/v1/*` → `404` JSON, не `index.html`.
- Кэш: `index.html` — no-cache; хэшированные ассеты — долгий `Cache-Control`.
- В `protected`: открыта когда auth выключена, под auth когда включена (как и `/v1`).

### 6. Конфиг и старт

- env: `KRONIKA_WEB_DIR`, `KRONIKA_WEB_ADDR`, `KRONIKA_WEB_BASIC_AUTH`, `KRONIKA_WEB_STALE_AFTER_S` (дефолт 10), `KRONIKA_WEB_LOG` (дефолт `info`).
- Валидация: невалидный `STALE_AFTER_S` → fail-fast. Заданный, но битый `BASIC_AUTH` (нет `:` / пустой user) → fail-fast (§4). Не задан → auth выключена (открыто).
- Стартовый баннер (`tracing` info): адрес, auth on/off, порог, версия и format_version, путь стора.
- Стор недоступен на старте → crash с ненулевым кодом. Требование деплоя: стор доступен до старта (initContainer/mount-dependency для сетевых томов). Lazy-start-в-not-ready — follow-up.

## Структура

`lib.rs` уже 1174 строки; этот PR добавляет пять ответственностей, поэтому режем на модули сейчас:

```
bins/pg_kronika-web/src/
  lib.rs          # pub app(state, auth, metrics_handle) -> Router; pub AppState; склейка роутера
  handlers/{mod,v1,probes,metrics,static_}.rs
  serialize.rs    # value/row/page_to_json + *_name (перенос существующего)
  params.rs       # parse_u64/i64/limit/cursor + bad_request (перенос существующего)
  auth.rs         # check_basic_auth, auth_layer
  startup.rs      # staleness, metric_labels, parse_basic_auth, парсинг/валидация конфига — чистые функции
  main.rs         # тоньше
```

- `AppState = { snapshot, last_refresh, refresh_loop_iterations, stale_after }` — только runtime-данные, дёшево клонируется.
- Auth — `route_layer(auth)` на `protected`-роутере (`/v1/*` + статик), навешивается только при `auth = Some`. `auth: Option<AuthConfig>` (предвычисленный ожидаемый заголовок) захватывается слоем при построении — не в `AppState` (least-privilege). `None` = auth выключена, слой не навешивается.
- `PrometheusHandle` — аргумент `app()` или под-роутер `/metrics`, не в общем state.
- `main`: config (распарсить, провалидировать, fail-fast первым) → init tracing и recorder → open store → `AppState` → spawn refresh (обновляет `last_refresh`, `iterations`, метрики; `tracing` вместо `eprintln!`) → serve + graceful shutdown (abort таски).

## Тесты

Чистые (без сервера): `staleness(now,last,thr)`; `check_basic_auth` (нет / не-Basic / битый base64 / неверный user / неверный pass / верные); `metric_labels` (шаблон маршрута; несматченное → `other`); `parse_basic_auth` (нет `:` / пустой user / `:` в пароле).

In-process (`tower::oneshot`, без PG): `/healthz`→200; `/readyz` свежий→200, устаревший→503 (инъекция `last_refresh`); `/metrics` формат и наличие ключевых метрик; статика и SPA-fallback; неизвестный `/v1/*`→404 JSON. Auth выключена: всё открыто. Auth включена: `/v1/*` и статика без учёток→401, с учётками→200; `/metrics`, `/healthz`, `/readyz` открыты и при включённой auth.

BDD не требуется (сервисные ручки без PG).

## Решения по открытым вопросам

1. `/metrics` — открыт всегда (метрики о сервисе, не данные; никогда не за auth).
2. Статика — `rust-embed` (dev-режим с диска окупает зависимость).
3. Учётки — одна `KRONIKA_WEB_BASIC_AUTH` (парсинг первого `:`); две переменные — future.
4. Метрики — фасад `metrics` + exporter (покрывает и доменные метрики, слабее связан с версией axum).

## Инварианты для код-ревью

- По умолчанию всё открыто; auth включается только заданием `KRONIKA_WEB_BASIC_AUTH`.
- Публичны всегда, в том числе при включённой auth: `/healthz`, `/readyz`, `/metrics`.
- Auth включена → Basic Auth на всё остальное (`/v1/*` + статика); выключена → всё открыто.
- `last_refresh`/`iterations` пишет таска; `data_age`/`reader_age` считаются на скрейпе.
- Метка `path` = `MatchedPath`; статика и несматченное → бакет `other`.
- SPA-fallback только для UI-путей; неизвестный `/v1/*` → 404 JSON.
- `eprintln!` отсутствует; всё через `tracing`.
- `lib.rs` распилен; чистые функции тестируются без сервера.
