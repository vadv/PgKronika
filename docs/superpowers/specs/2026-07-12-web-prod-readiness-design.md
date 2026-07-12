# Спека: прод-готовность pg_kronika-web

Ветка целевого PR: `feat/web-prod-readiness` над `main` (d8e8dff). Крейт `bins/pg_kronika-web`.

## Цель

`pg_kronika-web` из PR #60 отдаёт JSON-API, но не готов к эксплуатации: нет проб для оркестратора, нет аутентификации, нет наблюдаемости за самим сервисом, устаревание данных молчит, статику (будущий UI) отдавать нечем. Этот PR закрывает это, оставляя API-контракт `/v1/*` неизменным.

## Scope

**В PR:** liveness/readiness-пробы, сигнал устаревания, метрики + request-логи, HTTP Basic Auth, раздача встроенной статики (готовность к UI), конфиг через env.

**Вне PR:** TLS (задача обратного прокси); ограничители запроса (таймаут/конкурентность/размер тела — сознательно не делаем); perf-рефакторы ридера (отдельный PR, сперва benchmarks); сам веб-UI (только механизм раздачи + плейсхолдер).

## Компоненты

### 1. Пробы: liveness и readiness

- `GET /healthz` → всегда `200` пока процесс жив. Тело `{"status":"ok"}`.
- `GET /readyz` → `200` если готов, `503` если нет. Готов = последний успешный refresh не старше порога (см. §2). Тело `{"ready":bool,"seconds_since_refresh":n}`.
- Обе — БЕЗ аутентификации (проба оркестратора должна достучаться).
- Readiness намеренно НЕ делает отдельный stat каталога: фоновая таска и так читает стор раз в секунду, поэтому свежесть последнего успешного refresh уже отражает «стор читается». Это исключает дублирующий I/O и гонки.

### 2. Сигнал устаревания

- Общее состояние получает `last_refresh: Arc<AtomicU64>` — unix-секунды последнего успешного `refresh_incremental()`. Инициализируется при старте (первый `open` = успешный refresh).
- Фоновая таска обновляет его на каждом `Ok(())`. На `Err` — НЕ обновляет (значение стареет).
- Семантика: устаревание = refresh СБОИТ (каталог пропал/недоступен), а НЕ «данные не менялись». Простаивающий стор с успешным no-op refresh здоров и готов.
- Порог: env `KRONIKA_WEB_STALE_AFTER_S` (дефолт 10 = 10× интервал refresh).

### 3. Наблюдаемость

**`GET /metrics`** — Prometheus text. Метрики:
- `kronika_web_requests_total{method,path,status}` — counter (RED).
- `kronika_web_request_duration_seconds{method,path}` — histogram.
- `kronika_web_refresh_errors_total` — counter.
- `kronika_web_seconds_since_refresh` — gauge.
- `kronika_web_store_warnings` / `kronika_web_store_damages` — gauge (из `snapshot.warnings()/damages()`).

Реализация: фасад `metrics` + `metrics-exporter-prometheus` (глобальный recorder ставится в `main()`, `PrometheusHandle` рендерит текст в хендлере). Request-метрики — тонкий middleware, пишущий count/duration/status.

**Request-логи:** `tracing` + `tracing-subscriber` (EnvFilter из `KRONIKA_WEB_LOG`, дефолт `info`) + `tower_http::trace::TraceLayer`. Инициализация в `main()`.

### 4. HTTP Basic Auth

- Учётки из env: `KRONIKA_WEB_BASIC_AUTH="user:password"` (одна переменная-секрет).
- Middleware проверяет `Authorization: Basic <base64>` против учёток; при отсутствии/несовпадении — `401` + `WWW-Authenticate: Basic`. Сравнение пароля — константное по времени (`subtle`/`constant_time_eq`), чтобы не течь по таймингу.
- Защищает: `/v1/*` и статику. НЕ защищает: `/healthz`, `/readyz` (пробы). `/metrics`: под auth (Prometheus умеет `basic_auth` в scrape-конфиге) — см. открытый вопрос.
- **Инвариант безопасности (fail-fast на старте):** bind НЕ на loopback без заданных учёток → отказ запуска. Матрица: loopback+без-учёток = dev, auth off; loopback+учётки = auth on; НЕ-loopback+учётки = auth on (безопасно наружу); НЕ-loopback+без-учёток = ОТКАЗ. Это заменяет и «гардрейлы», и «auth — задача прокси».
- «loopback?» — чистая функция от строки адреса: парсим как `SocketAddr`, `ip().is_loopback()`; всё, что не парсится в loopback-IP (0.0.0.0, конкретный IP, хостнейм), считаем сетевым.

### 5. Встроенная статика (подготовка к UI)

- Каталог `bins/pg_kronika-web/static/` зашивается в бинарь (`rust-embed`). Сейчас — плейсхолдер `index.html` («pg_kronika-web»), чтобы механизм работал и тестировался; реальный UI дропается позже без изменения кода.
- Роутинг: `/v1/*`, `/healthz`, `/readyz`, `/metrics` — как выше; всё остальное → статик-хендлер: ищет путь во встроенных ассетах, отдаёт с корректным content-type; не найдено → `index.html` (SPA-fallback).
- Под той же Basic Auth, что API.

### 6. Конфиг (env, все с дефолтами)

- `KRONIKA_WEB_DIR`, `KRONIKA_WEB_ADDR` — как есть.
- `KRONIKA_WEB_BASIC_AUTH` — `user:password`.
- `KRONIKA_WEB_STALE_AFTER_S` — порог устаревания (дефолт 10).
- `KRONIKA_WEB_LOG` — уровень логов (дефолт `info`).

## Структура (границы крейта)

- **lib.rs:** `app()` строит роутер со всеми ручками, статик-fallback, auth-middleware, trace/metrics-слоями. `AppState` расширяется: `snapshot` (как есть) + `last_refresh: Arc<AtomicU64>` + конфиг (учётки, порог, `PrometheusHandle`). Чистые функции (auth-проверка, loopback-проверка, staleness) — в lib, юнит-тестируемы.
- **main.rs (тонкий):** init tracing + install prometheus recorder → open store → построить `AppState` (initial `last_refresh`) → проверить инвариант bind-vs-auth (fail-fast) → spawn refresh-таску (обновляет `last_refresh` + пишет метрики warnings/damages/refresh-errors) → serve с graceful shutdown (как в #60).
- check-deps: новые зависимости — сторонние (`metrics`, `metrics-exporter-prometheus`, `tracing`, `tracing-subscriber`, `tower-http`, `rust-embed`, `base64`, `subtle`), границы kronika-* allow-list не трогают.

## Тесты

Чистые функции (юнит): staleness (now/last/порог → устарел?), инвариант bind-vs-auth (адрес/учётки → пуск/отказ), проверка Basic-заголовка (заголовок/учётки → ok/401).

In-process (`tower::oneshot`, без PG): `/healthz`→200; `/readyz` свежий→200, устаревший→503 (инъекция `last_refresh`); `/metrics` формат + наличие ключевых метрик; auth — нет/битые/верные учётки → 401/401/200; пробы без auth достижимы; статика отдаётся + SPA-fallback на `index.html`; `/v1/*` работает под auth.

Живой PG этим ручкам не нужен → BDD не требуется (юнит+интеграция покрывают). При желании — 1 web-BDD-сценарий на `/readyz` поверх снятого стора, но не обязателен.

## Открытые вопросы (для ревьюеров)

1. `/metrics` — прятать за Basic Auth (лин: да) или оставлять открытым для внутреннего скрейпа?
2. `rust-embed` vs `include_dir`+`mime_guess` для статики — что легче/чище.
3. Учётки: одна `user:password` vs две переменные.
4. Метрики: фасад `metrics`+exporter vs `axum-prometheus`-обёртка.

## Инварианты, которые должен проверить ревьюер

- Неаутентифицированный API НЕ может быть выставлен в сеть (fail-fast).
- Пробы не за auth; всё остальное — за auth.
- `last_refresh` обновляется только при успехе; readiness честно краснеет при сбое refresh.
- Порядок middleware: auth ДО раздачи данных/статики; trace/metrics — снаружи.
