# Incident-анализ

`GET /v1/incidents` группирует аномальные эпизоды по времени и узлу, затем
проверяет их диагностическими линзами над counter-дельтами, gauge-снимками,
активностью, блокировками и типизированными событиями stderr. Finding —
проверяемая гипотеза, а не установленная первопричина.

## Как читать ответ

```json
{
  "complete": false,
  "clustering_complete": true,
  "analysis_status": "incidents_detected",
  "incidents": [
    {
      "incident_key": "...",
      "members": ["..."],
      "findings": [
        {
          "lens_id": "PG-CACHE-010",
          "role": "amplifier",
          "confidence": "medium",
          "scope": {
            "logical_section": "pg_stat_database",
            "column": "blks_read",
            "identity": [5]
          },
          "evidence": [
            {
              "schema_version": 1,
              "type": "counter_aggregate",
              "claim": "derived_counter_threshold_crossing",
              "numeric_representation": "ieee754_binary64",
              "measurement": {
                "kind": "ratio",
                "formula": "blks_read / (blks_read + blks_hit)",
                "operands": [
                  {
                    "name": "blks_read",
                    "aggregation": "delta_sum",
                    "value": 80,
                    "unit": "count",
                    "purpose": "formula",
                    "numeric_representation": "ieee754_binary64"
                  },
                  {
                    "name": "blks_hit",
                    "aggregation": "delta_sum",
                    "value": 20,
                    "unit": "count",
                    "purpose": "formula",
                    "numeric_representation": "ieee754_binary64"
                  }
                ],
                "value": 0.8
              },
              "unit": "ratio",
              "threshold": { "operator": "at_least", "value": 0.2 },
              "coverage": {
                "basis": "paired_observed_interval_endpoints",
                "selection_from_us": 1700000000000000,
                "selection_to_us": 1700000180000000,
                "interval_end_bounds": "inclusive",
                "first_usable_interval_start_us": 1699999940000000,
                "first_usable_interval_end_us": 1700000000000000,
                "last_usable_interval_end_us": 1700000120000000,
                "candidate_interval_count": 3,
                "usable_interval_count": 3,
                "excluded_interval_count": 0,
                "excluded_by_reason": {
                  "unmatched_endpoint": 0,
                  "unusable_delta": 0,
                  "unaligned_or_invalid_duration": 0,
                  "numeric_limit": 0
                },
                "summed_interval_duration_us": 180000000,
                "observed_endpoint_pairing_complete": true,
                "source_period": {
                  "basis": "observed_series_delta_median",
                  "observed_source_period_us": 60000000,
                  "expected_interval_count": 3,
                  "expected_interval_count_reason": null,
                  "source_window_completeness": 1.0
                }
              },
              "entity": {
                "logical_section": "pg_stat_database",
                "identity": [5]
              }
            }
          ]
        }
      ],
      "evaluation_complete": true,
      "finding_evaluation_status": "complete"
    }
  ]
}
```

- `complete` относится ко всей подсистеме. Пока каталог реализован не полностью,
  значение остаётся `false` даже при завершённой оценке текущего запроса.
- `clustering_complete` сообщает только о чтении, scoring и группировке
  эпизодов. Полноту логов и диагностических данных оно не подтверждает.
- `analysis_status` принимает значения `no_data`, `insufficient_data`, `calm`,
  `incidents_detected` или `partial`.
- `findings=[]` при `finding_evaluation_status=complete` означает лишь, что
  активные линзы не нашли своих условий в этом окне.
- `finding_evaluation_status=partial` означает остановку по work/output limit;
  причина находится в `skipped.evaluations`.
- До интерпретации результата нужно проверить `coverage_by_section`,
  `data_quality`, `skipped` и `catalog.capabilities`.

`counter_aggregate` публикует формулу, исходные операнды, их назначение и единицы.
Интервалы объединяются только при одинаковых endpoint и длительности; reset,
gap, отрицательная дельта, несовпадение длительности, потеря точности и overflow
не превращаются в ноль. `excluded_by_reason` объясняет исключённые наблюдаемые
пары. Первый usable-интервал может начаться раньше `selection_from_us`, поэтому
его фактический старт указан отдельно.

Порог покрытия 70% применяется к endpoint, которые видны хотя бы в одном
операнде. Это отдельная проверка от `coverage.source_period`: cadence выводится
как медиана длительностей usable counter-интервалов или разрывов между gauge-
снимками. Для устойчивого периода нужны минимум три интервала. Если их меньше,
`observed_source_period_us` и `expected_interval_count` равны `null`,
`source_window_completeness` — `"unknown"`, а
`expected_interval_count_reason` содержит закрытую причину `{kind,params}`.
Допустимы `insufficient_intervals_for_observed_period`,
`incident_window_shorter_than_observed_period` и `empty_incident_window`. При
доказанном периоде полнота равна usable-интервалам, делённым на ожидаемое число
интервалов incident-окна; значение намеренно не ограничивается единицей. Нельзя
делать вывод об отсутствии события по одному полю
`observed_endpoint_pairing_complete`.

Gauge-evidence имеет `type=gauge`, `observed_at_us`, число снимков,
`coverage.source_period` и имена операндов (`operand` либо
`numerator_name`/`denominator_name`). Для отношения
отдельно указаны единицы числителя, знаменателя и результата: например,
`milliseconds / count` даёт `milliseconds_per_call`, а не безразмерный `ratio`.
Совместимые поля `operand_unit` и `headroom` сохраняют числовое значение, когда
единицы операндов совпадают, и равны `null`, когда они различаются. Поле
`numeric_representation` фиксирует JSON-представление. Целые counter-дельты и их
суммы, которые нельзя точно представить в binary64, исключаются с причиной
`numeric_limit`.

`catalog.applied` содержит активные стабильные `lens_id`; `catalog.dormant` —
неактивные линзы и недостающие prerequisites. `slug` — машинное имя и не
заменяет `lens_id`. API не возвращает `title`, `question` или `text_locale`:
локализованные EN/RU подписи принадлежат каталогам UI. `confidence_cap` —
верхняя граница, а не оценка несуществующего finding. Product-owned причины
неполноты имеют закрытую форму `{ "kind": "...", "params": { ... } }`;
исходный текст PostgreSQL, ОС и пользователя остаётся буквальным.

## Граница доказательств

Допустимые роли finding: `lead`, `amplifier`, `downstream` и `coincident`.
Endpoint работает с `ClockRelation::Simultaneous`. По принятой продуктовой
договорённости timestamp снимка является истинным временем наблюдения, но все
обычные метрики одного snapshot/incident observation считаются одновременными
событиями. Порядок `EpisodeRefV1.start_us` внутри такого observation не назначает
`lead` или `downstream`: metric findings остаются `coincident` независимо от
того, оказался timestamp минимальным, промежуточным или максимальным. Равные
timestamps, включая разделённые несколькими сигналами экстремумы, также ничего
не меняют: все эти findings остаются `coincident`.

`Lead` и `downstream` разрешены только при отдельном структурном evidence. Уже
назначенный `amplifier` также не переинтерпретируется по времени. Если
запрошенная lock-роль противоречит стороне участника ребра, finding остаётся
`coincident`.

Отдельный структурный источник роли — сохранённое ребро
`pg_locks.blocked_by`, полученное из `pg_blocking_pids`. Для конкретного снимка
blocker публикуется как `lead`, waiter
как `downstream`. Это направление структуры ожидания, а не доказательство root
cause всего инцидента. PostgreSQL возвращает как hard blocker — держателя
конфликтующей блокировки, так и soft blocker — стоящего раньше конфликтующего
waiter. PID `0` означает prepared transaction. Несколько blocker сохраняются
отдельными детерминированными рёбрами. Повтор одного `(waiter_pid,
blocker_pid)` в снимке дедуплицируется, а транзитивные рёбра не синтезируются.

Lock-evidence содержит timestamp, waiter PID, blocker PID, сторону finding,
политику дедупликации, `transitive_inference=false` и пометку
`hard_or_soft_block`. Текущий typed input не переносит lock target и mode,
поэтому `evidence_completeness=edge_only`, а confidence cap равен `medium`.
Отсутствующий или частичный snapshot не используется как доказательство
отсутствия блокировок.

Обогащение activity-finding ребром блокировки разрешено, только когда producer
сохраняет явный токен общего snapshot и совпадают `(snapshot_ts, waiter_pid,
waiter_backend_start)`. Совпавшие timestamps сами по себе не являются
provenance. Сейчас `pg_stat_activity` и `pg_locks` читаются разными statements,
поэтому адаптер не выдаёт токен, а capability `cross_section_entity_join`
остаётся missing. После появления токена тот же typed-контракт активирует
bounded evidence без PID-only fallback.

`OS-CGRP-021` показывает `throttled_usec` в микросекундах на секунду
наблюдаемых интервалов. `usage_usec` имеет другую область учёта и не служит
знаменателем wall time. Без подтверждённой связи PostgreSQL→cgroup и данных о
свободном CPU finding остаётся `coincident`.

События stderr проходят отдельным путём. Восемь event-веток возвращаются в
секции `log`; `log.complete=false`, потому что stderr-источник не доказывает
исчерпывающего покрытия. В частности:

- SIGKILL не доказывает, что процесс выбрал kernel OOM killer;
- PANIC не доказывает повреждение данных;
- `out of shared memory` нельзя смешивать с физическим RAM OOM;
- slow-query record подтверждает превышение настроенного порога;
- локальный `invalid record length` может быть обычным концом WAL;
- отсутствие события ничего не доказывает без полного source/config coverage.

SQL, параметры, IP, user/database, пути, conninfo и archive command могут
содержать секреты. Event-линзы редактируют их до публикации; raw log payload
incident API не возвращает.

## Разработчику

Код подсистемы находится в `bins/pg_kronika-web/src/incident/`:

- `active.rs` — активные counter- и snapshot-линзы;
- `gauge_contracts.rs`, `os_lenses.rs`, `query_plan.rs` — gauge-, OS- и
  query-линзы;
- `events.rs` — event-линзы и их каталог;
- `dispatch.rs`, `engine.rs`, `evidence.rs` — допуск работы, output limits,
  роли и confidence;
- `model.rs`, `series.rs`, `typed.rs`, `cluster.rs`, `entity_join.rs` — identity,
  входные ряды, кластеризация и типизированные связи сущностей;
- `incident_input.rs` — bounded adapter reader → incident engine;
- `incident_response.rs` — JSON transport.

Новая линза должна получить bounded typed input, объявить capabilities и entity
join, соблюдать общий work budget, лимиты findings/evidence и детерминированную
сортировку. Стабильный `lens_id` менять нельзя; читаемое имя меняется через
`slug`.

Совпадение числового identity и нахождение эпизодов в одном временном кластере
не образуют entity join. Cross-section ветка включается только после появления
явного producer contract: одинаковые source/node, тип и полная identity
сущности, общий snapshot token, сохранённая snapshot-scoped relation либо
bounded mapping с пересекающимися lifetime, а также coverage обоих входов и
самой relation/mapping. До этого `cross_section_entity_join` остаётся missing
capability конкретной ветки.

Ответ содержит 34 уникальных `lens_id`: 28 metric/snapshot-линз и восемь
event-веток, две из которых переиспользуют `OS-FS-027` и `PG-CONN-014`. Формулы
и DQ-правила описаны в
[контракте линз](superpowers/specs/2026-07-16-kronika-incident-lenses-design.md),
границы модулей и ресурсов — в
[контракте реализации](superpowers/specs/2026-07-17-kronika-incident-implementation.md).
