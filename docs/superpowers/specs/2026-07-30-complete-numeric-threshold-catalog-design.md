# Полный числовой каталог абсолютных порогов

Дата: 2026-07-30.

Статус: утверждено к реализации в PR с типизированным каталогом.

Документ расширяет
`2026-07-29-absolute-threshold-catalog-design.md`: к первым 42 политикам
добавляются оставшиеся числовые правила исследования и config-bound
индикаторы PostgreSQL. Категориальные правила для состояний, `wait_event`,
locks и событий журнала остаются отдельной последующей работой.

## Решение

Каталог сохраняет существующие `MetricInput`, `Policy`, `Evidence` и
детерминированную классификацию O(1). Новые метрики используют `Scalar` или
`Fraction`; новый архетип политики не требуется.

Ёмкость connection pool не определяется абсолютным числом активных сессий.
Адаптер передаёт число строк `pg_stat_activity` с
`backend_type = 'client backend'` как numerator и `max_connections` как
denominator. Политика выдаёт warning при доле `>= 0.70` и critical при
`>= 0.90`. Состав сессий сохраняется отдельными метриками: idle-in-transaction,
blocked, long query и long transaction. Для них сам факт ненулевого значения
остаётся сигналом, поэтому `max_connections` не размывает событие.

Config-bound правила autovacuum принимают наблюдаемый счётчик и уже
вычисленный effective threshold. Effective threshold обязан учитывать
версию PostgreSQL, `pg_settings` и применимые relation/TOAST reloptions.
Каталог не вычисляет серверную формулу и не соединяет несогласованные
снимки. Отношение `observed / effective_threshold > 1` даёт warning без
выдуманного critical-множителя. Отключённое или неприменимое правило
передаётся как `MetricInput::NotApplicable`; нулевой или отрицательный
denominator остаётся `InvalidDenominator`.

## Новые политики

### Sessions и activity

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `pg.activity.query_duration_seconds` | scalar, s; idle неприменим | `>= 1` | `>= 30` |
| `pg.activity.transaction_duration_seconds` | scalar, s | `>= 5` | `>= 60` |
| `pg.activity.client_backend_capacity` | client backends / `max_connections` | `>= 0.70` | `>= 0.90` |
| `pg.activity.idle_in_transaction_sessions` | scalar, count | `-` | `> 0` |
| `pg.activity.blocked_sessions` | scalar, count | `> 0` | `>= 5` |
| `pg.activity.long_queries` | scalar, count | `> 0` | `>= 5` |
| `pg.activity.long_transactions` | scalar, count | `> 0` | `>= 3` |
| `pg.database.rollback_pct` | scalar, % | `> 3` | `> 10` |
| `pg.database.deadlocks_delta` | scalar, count delta | `-` | `> 0` |

`query_duration_seconds` получает `NotApplicable` для idle-строки.
Накопительный `deadlocks` обязан пройти reset-aware diff до классификации.

### Cache, bgwriter и checkpoints

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `pg.database.cache_hit_pct` | scalar, % | `< 99` | `< 90` |
| `pg.database.io_cache_hit_pct` | scalar, % | `< 99` | `< 90` |
| `pg.database.effective_cache_hit_pct` | scalar, % | `< 99` | `< 90` |
| `pg.checkpointer.checkpoints_per_minute` | scalar, per-minute | `> 2` | `-` |
| `pg.checkpointer.write_time_ms_delta` | scalar, ms delta | `> 30000` | `> 120000` |
| `pg.bgwriter.buffers_backend_per_second` | scalar, per-second | `> 0` | `-` |
| `pg.bgwriter.maxwritten_clean_delta` | scalar, count delta | `> 0` | `-` |
| `pg.bgwriter.client_evictions_per_second` | scalar, per-second | `> 0` | `>= 10` |

У исходной строки `client_evictions_s` warning записан как `< 10`, что
классифицировало бы ноль как проблему. Каталог фиксирует фактический смысл
индикатора: ноль — `Inactive`, положительное значение ниже 10 — warning,
значение `>= 10` — critical.

### Statements и plans

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `pg.statements.milliseconds_per_row` | scalar, ms | `>= 10` | `>= 100` |
| `pg.statements.mean_time_ms` | scalar, ms | `>= 10` | `>= 100` |
| `pg.statements.time_pct` | scalar, % | `>= 20` | `>= 50` |
| `pg.statements.plan_time_pct` | scalar, % | `>= 50` | `>= 80` |
| `pg.statements.plan_count` | scalar, count | `> 1` | `> 3` |

`time_ratio`, `query_time_ratio` и coefficient of variation не дублируются:
они остаются Классом 2 относительно истории ряда.

### Replication

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `pg.replication.replay_lag_seconds` | scalar, s | `> 10` | `> 60` |
| `pg.database.recovery_conflicts_delta` | scalar, count delta | `> 0` | `-` |

Оба накопительных счётчика, `deadlocks` и recovery conflicts, получают в
каталог только reset-aware delta. Отсутствие реплики передаётся как
`NotApplicable`.

### Config-bound autovacuum

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `pg.tables.vacuum_threshold_ratio` | dead tuples / effective vacuum threshold | `> 1` | `-` |
| `pg.tables.analyze_threshold_ratio` | modified tuples / effective analyze threshold | `> 1` | `-` |
| `pg.tables.insert_vacuum_threshold_ratio` | inserted tuples / effective insert-vacuum threshold | `> 1` | `-` |

Эти три записи сообщают о пересечении серверного условия запуска, а не
утверждают, что autovacuum сломан или обязан уже завершиться.

## Инварианты и тесты

- Итоговый каталог содержит 69 записей: 42 исходные, 24 оставшиеся числовые
  политики исследования и 3 config-bound autovacuum-индикатора.
- `MetricId::ALL`, строковые коды и `CATALOG` имеют одинаковый стабильный
  порядок и не содержат дубликатов.
- Golden-таблица независимо фиксирует форму входа, единицу, операторы,
  пороги и `ZeroDisposition`.
- Boundary-тесты покрывают равенство для `>= 5`, `>= 10`, `>= 90`,
  `>= 120000` и строгие `> 0`, `> 1`, `> 2`, `> 3`, `> 10`, `> 60`.
- Fraction-тесты доказывают `5 / 35 = Ok`, `70 / 100 = Warning`,
  `90 / 100 = Critical` и typed error для недопустимого `max_connections`.
- Ноль даёт `Inactive` для event/rate/delta-индикаторов и `Ok` для
  применимых gauge/percentage-метрик.
- Новые записи не добавляют I/O, clock access, heap allocation или
  зависимости; пиковая дополнительная память одной классификации остаётся
  O(1).

## Нецели

- Web/API/UI-проекция.
- Вычисление effective autovacuum threshold внутри analytics.
- Категориальные `state`, `wait_event`, `lock_granted`, severity, category и
  event type.
- Калибровка provisional-порогов на демостенде.
- Диагноз причин нагрузки по одному числовому вердикту.
