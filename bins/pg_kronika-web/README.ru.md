# pg_kronika-web

[English version](README.md)

`pg_kronika-web` открывает локальный каталог PGM через встроенный UI, JSON API
и Prometheus endpoint. Он читает готовые сегменты и корректные кадры
`active.parts` через `LocalDirSnapshot`, обновляет snapshot раз в секунду и не
подключается к PostgreSQL.

## Настройки

| Переменная | Дефолт | Назначение |
| --- | ---: | --- |
| `KRONIKA_WEB_DIR` | обязательна | Каталог `.pgm` и необязательного `active.parts`. |
| `KRONIKA_WEB_ADDR` | обязательна | Адрес прослушивания в формате `host:port`. |
| `KRONIKA_WEB_BASIC_AUTH` | не задан | `user:password`; без него UI и `/v1/*` открыты. |
| `KRONIKA_WEB_STALE_AFTER_S` | `10` | `/readyz` возвращает `503`, если успешный refresh старше этого времени. |
| `KRONIKA_WEB_LOG` | `info` | Filter directive для `tracing-subscriber`. |

```sh
KRONIKA_WEB_DIR=/var/lib/pg_kronika \
KRONIKA_WEB_ADDR=127.0.0.1:8688 \
KRONIKA_WEB_BASIC_AUTH='operator:change-me' \
pg_kronika-web
```

TLS не встроен: слушайте loopback или используйте TLS reverse proxy. Basic
Auth закрывает UI и `/v1/*`; `/healthz`, `/readyz` и `/metrics` всегда
публичны. Credentials не выводятся в ошибке конфигурации и debug, но Basic Auth
не шифрует соединение.

## Endpoints

| Endpoint | Параметры | Результат |
| --- | --- | --- |
| `GET /healthz` | нет | Liveness процесса. |
| `GET /readyz` | нет | Готовность и возраст refresh snapshot. |
| `GET /metrics` | нет | Prometheus text: reader, data age, requests, RSS и fd. |
| `GET /v1/version` | нет | Версии API и PGM. |
| `GET /v1/sources` | нет | Source ids, временные диапазоны и число units. |
| `GET /v1/sections` | нет | Logical sections реестра и объединённые схемы. |
| `GET /v1/segments` | `source`, `from`, `to` | Каталоги пересекающихся сегментов без декода тел. |
| `GET /v1/section/{name}` | `source`, `from`, `to`; необязательные `limit`, `cursor` | Упорядоченные строки, gaps и cursor следующей страницы. |
| `GET /v1/sections/batch` | `source`, `from`, `to`, список `names` через запятую; необязательный `limit` | Несколько logical sections за один проход сегментов. |
| `GET /v1/section/{name}/diff` | `source`, `from`, `to` | Delta/rate по identity и no-data reasons. |
| `GET /v1/sections/batch/diff` | `source`, `from`, `to`, список `names` | Несколько diff за один проход. |
| `GET /v1/anomalies` | `source`, `from`, `to`; необязательные `window`, `step`, `threshold`, `eps_rel`, `limit`, `section` | Ранжированные эпизоды robust score и счётчики качества. |
| `GET /v1/incidents` | `source`, `from`, `to`; необязательные `window`, `step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section` | Кластеры связанных anomaly episodes. Диагностических findings пока нет. |
| `GET /` | нет | Встроенный UI. |

`source` — беззнаковый id из каталога. `from` и `to` — знаковые Unix
microseconds. Duration принимает `250ms`, `90s`, `15m`, `2h` или секунды без
суффикса. Row endpoints по умолчанию возвращают 1 000 строк и ограничивают
`limit` значением 10 000. Cursor непрозрачен: клиент передаёт его без изменений.

```sh
curl -u operator:change-me \
  'http://127.0.0.1:8688/v1/segments?source=1&from=0&to=9223372036854775807'
```

Ошибка имеет форму `{ "error": "code", "detail": "message" }`, если detail
нужен. Неизвестная секция даёт `404`, неверные параметры — `400`, превышение
materialization ceiling — `413`.

## Контракты чтения и анализа

- Reader берёт только пересекающиеся units, проверяет CRC PGM и секции до
  декодирования, объединяет версии layout по logical name и сортирует по ключу
  реестра. Точные sealed/live дубликаты подавляются.
- Diff выдаёт измеренный ноль только для неизменившегося счётчика. Reset, gap,
  первая точка, неверное время/значение и выключенный сбор имеют разные no-data
  reasons.
- Anomaly scoring использует устойчивую статистику окон, независимую от
  источника. Пропуски и разрывы считаются not evaluated, а не заменяются нулём.
- Incident request ограничен 24 часами и фиксированными потолками units,
  sections, materialized cells, series points, identity bytes, scoring work и
  episodes.
- Одновременно выполняется один anomaly или incident request. Конкурирующий
  тяжёлый запрос получает `503` и `Retry-After: 1`, а не ставится в очередь.
- `/v1/incidents` сейчас только группирует эпизоды. Поле `complete` всегда
  `false`, findings пусты, diagnostic lens catalog находится в dormant status.

Warnings сканирования и повреждённые диапазоны журнала остаются в reader и
влияют на gaps/completeness. Они не превращаются в успешные строки.

## Завершение и отказы

`SIGTERM` и `SIGINT` запускают graceful HTTP shutdown. Ошибка refresh
записывается в лог, а последний опубликованный snapshot остаётся доступен;
после заданного порога `/readyz` становится stale. Неверная environment
configuration или ошибка первого открытия store завершают процесс до bind.

У бинарника нет CLI-флагов. MCP, удалённые хранилища, retention и доставка
алертов не реализованы.
