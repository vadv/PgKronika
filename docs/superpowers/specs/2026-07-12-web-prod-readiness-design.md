# Эксплуатационный контракт `pg_kronika-web`

## Пробы

- `GET /healthz` возвращает `200`, пока процесс обслуживает HTTP.
- `GET /readyz` возвращает `200`, если snapshot reader успешно обновлялся в
  пределах `KRONIKA_WEB_STALE_AFTER_S`; иначе `503`.
- Readiness описывает доступность reader, а не свежесть данных коллектора.
  Свежесть отражает `kronika_web_data_age_seconds`.

## Метрики и логи

`/metrics` всегда публичен. Метка `path` использует шаблон `MatchedPath`, чтобы
не создавать кардинальность из пользовательских URL.

- `kronika_web_requests_total{method,path,status}`;
- `kronika_web_request_duration_seconds{method,path}`;
- `kronika_web_refresh_errors_total`;
- `kronika_web_refresh_loop_iterations_total`;
- `kronika_web_data_age_seconds` — возраст последнего unit; `NaN`, когда данных
  нет;
- `kronika_web_units_total`;
- `kronika_web_reader_age_seconds`;
- `kronika_web_store_warnings`, `kronika_web_store_damages`;
- `kronika_web_inflight_requests`;
- `process_resident_memory_bytes`, `process_open_fds`;
- `kronika_web_build_info{version,format_version}`.

Возраст вычисляется при scrape, поэтому остановившийся refresh loop не замораживает
последнее корректное значение. RSS берётся из `VmRSS` в `/proc/self/status`.
Логи структурированы через `tracing`; уровень задаёт `KRONIKA_WEB_LOG`.

## Аутентификация

Без `KRONIKA_WEB_BASIC_AUTH` API и статика открыты. При заданном
`user:password` Basic Auth защищает `/v1/*` и статику. `/healthz`, `/readyz` и
`/metrics` остаются публичными. Пароль может содержать двоеточие. Некорректное
значение останавливает запуск, но не выводится в ошибку или лог.

Ожидаемый заголовок формируется при старте и сравнивается константно по времени.
Промах возвращает `401` и `WWW-Authenticate: Basic`.

## Статика и маршрутизация

Ассеты из `bins/pg_kronika-web/static/` встроены в бинарник. Неизвестный UI-путь
получает `index.html`; неизвестный `/v1/*` получает JSON `404`. `index.html`
отдаётся с `no-cache`, хэшированные ассеты допускают длительное кэширование.

## Конфигурация и запуск

Обязательны `KRONIKA_WEB_DIR` и `KRONIKA_WEB_ADDR`. Опциональны
`KRONIKA_WEB_BASIC_AUTH`, `KRONIKA_WEB_STALE_AFTER_S` и `KRONIKA_WEB_LOG`.
Некорректная конфигурация и недоступный при старте store завершают процесс с
ненулевым кодом. TLS, сетевые timeouts и rate limiting обеспечиваются внешним
прокси.
