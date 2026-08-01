# Slim-контракт Web UI API

Дата: 2026-08-01.

Статус: DESIGN, согласовано направление. Заменяет части
`2026-07-31-web-ui-v5-api-gap-design.md`, касающиеся per-value статусов и
причин; там, где документы конфликтуют, действует этот.

## Причина

Клиент у API ровно один — встроенная SPA (AI-экспорт читает тот же payload).
Контракт v5 писал статусы и машинные причины на каждое значение, как если бы
клиентов была тысяча: `value_statuses` на каждый бакет spine, `cell_statuses`
на каждую ячейку frame, `statuses`/`reasons` на каждый снапшот истории entity.
Измеримый результат: до половины байтов ответа — повтор `"available"` и
одинаковых строк причин, которые интерфейс не показывает, а оператор не
читает. Операторская модель проще: данных нет — ячейка серая; почему нет —
одно место смотрит, а не каждая дыра.

## Решение

**Честность показывается один раз и одним местом, а не атрибутом каждого
значения.**

- Значения голые. Отсутствие данных — `null`; SPA рисует эм-тире/серое.
  Никаких `status`/`reason` полей рядом со значениями.
- Причины недоступности живут в одном агрегированном месте:
  `GET /v1/data/quality` (capabilities + collection state per view) и
  `availability`/`unavailable_reason` на колонке каталога. UI показывает один
  индикатор здоровья сбора, а не маркер в каждой ячейке.
- Действенные коды сохраняются только там, где они меняют действие оператора:
  `permission` (чинится грантом), `missing_extension` (чинится CREATE
  EXTENSION), `producer_gap` (окно мёртвого коллектора — иди в его логи),
  `reset` (счётчик обнулился, не сравнивай), `resource_limited` (это top-N,
  не полный список). Таксономии ради таксономии нет.
- Gaps остаются спан-маркерами времени (`{from_us, to_us}`): это сигнал
  инцидента сбора, а не «честность ячейки».
- Integrity — только crash-atomicity: `corrupt_segments` и
  `quarantined_entries` означают «запись обрезалась/битая», никакого
  анти-тампера и провенанса.

## Что удаляется из ответов

| Ответ | Удаляется | Остаётся |
| --- | --- | --- |
| `/v1/timeline/spine` | `value_statuses` (512×2 объектов) | `values` (null — маркер), `gaps`, `quality` |
| `/v1/frame/{view}` | `cell_statuses`, `categorical_classifications` | `cells`, `classifications` (вердикт-цвет — контент), sparks, `quality` |
| `/v1/entity` point | `status`/`reason` у каждого field | `fields[{code, value}]`, `related`, `quality` |
| `/v1/entity` history | `statuses`/`reasons` на снапшот | `snapshots[{ts_us, values}]`, `page`, `quality` |
| `/v1/ui/catalog` | колонки без данных навсегда (PSS до появления сбора) | `availability`/`unavailable_reason` на колонке — это и есть «одно место» по колонкам |
| все | дубли строк причин, тройные параллельные массивы | — |

`null` по-прежнему означает «не наблюдалось», наблюдённый ноль остаётся
числом — это видно по типу значения и не требует статуса.

## Что остаётся без изменений

- Вердикт-классификации для цвета ячеек (`classifications`) — это контент, а
  не честность.
- `gaps`, `quality.status`, `active_tail` во frame/spine/summary.
- `/v1/data/quality` как единственная поверхность «почему данных нет»
  (capabilities, coverage, gaps, producer, integrity).
- Entity tokens, cursors, pagination, `q`, `columns`, `database` фильтр.
- Closed enums в OpenAPI; 21 операция.

## Миграция SPA

Три места читают удаляемые поля и упрощаются:

- `Spine.tsx` — null-бакет = разрыв линии + нейтральный маркер, без wire-статуса.
- `TableView.tsx` — null-ячейка = эм-тире, без tooltip со статусом.
- `DockOverlay.tsx` — история рендерится по `values`, point — по `fields[{code, value}]`.

`AlertBar` и `DataHealthPopover` (агрегированные качество/здоровье) не меняются.

## Критерии приёмки

- В ответах нет полей `value_statuses`, `cell_statuses`, `statuses`, `reasons`,
  `categorical_classifications`.
- OpenAPI перегенерирован; drift-check чист; 21 операция на месте.
- Отсутствующее значение в SPA — эм-тире/серое; здоровье сбора видно одним
  индикатором из `/v1/data/quality`.
- Размер типичного frame- и spine-ответа уменьшается измеримо (замеры в PR).
- Тесты переписаны под голые значения; гейт (fmt, clippy, test, frontend) зелёный.
