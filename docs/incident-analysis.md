# Incident-анализ

`GET /v1/incidents` группирует аномальные эпизоды по времени и узлу, затем
выполняет активные диагностические линзы над типизированными counter-дельтами,
gauge-снимками и снимками активности и блокировок, а параллельно — линзы над
лог-событиями. Это диагностические гипотезы, а не измеренная первопричина.

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
              "type": "gauge",
              "measurement": {
                "kind": "ratio",
                "numerator": 80,
                "denominator": 100,
                "value": 0.8,
                "operand_unit": "count"
              },
              "unit": "ratio",
              "threshold": { "operator": "at_least", "value": 0.2 }
            }
          ]
        }
      ],
      "evaluation_complete": true,
      "finding_evaluation_status": "complete"
    }
  ],
  "catalog": {
    "status": "partial",
    "diagnosis_available": true,
    "scope": "diagnostic_lenses",
    "applied": ["PG-CACHE-010", "..."],
    "dormant": ["..."]
  }
}
```

- `complete` относится ко всему incident-анализу. Пока каталог покрыт частично,
  поле остаётся `false` даже при полном результате кластеризации и lens evaluation.
- `clustering_complete` сообщает только о полноте чтения, scoring и группировки
  эпизодов. Оно не подтверждает полноту логов или диагностических evidence.
- `analysis_status` описывает результат текущего запроса: `no_data`,
  `insufficient_data`, `calm`, `incidents_detected` или `partial`.
- `incident_key` — детерминированный ключ из node identity, интервала и членов
  кластера. Он не содержит `lens_id`.
- `findings=[]` при `finding_evaluation_status=complete` означает, что активные
  линзы не нашли своих условий в этом incident-окне. Это не доказывает отсутствие
  проблем вне активного каталога.
- `finding_evaluation_status=partial` означает, что лимит работы или output
  остановил оценку части линз; подробность находится в `skipped.evaluations`.
- `skipped` и `data_quality` нужно проверять до интерпретации результата.
- `evidence` каждого finding несёт наблюдённую величину: `gauge`-наблюдение с
  числами (`numerator`/`denominator`/`value` или `value` с единицей) и порогом,
  который сигнал пересёк. Линзы над лог-событиями несут `event`-факты без величины.

`catalog.applied` содержит активные `lens_id`. `catalog.dormant` содержит
оставшиеся диагностические вопросы, для которых не хватает входов. Поле `lens_id`
сохраняет опубликованный стабильный идентификатор (`PG-QRY-001`, …,
`OS-NET-028`), а `slug` даёт читаемое snake_case-имя. Русские `title` и
`question` явно помечены `text_locale="ru"`. `confidence_cap` — верхняя граница
уверенности, а не уверенность несуществующего finding.

Пример записи:

```json
{
  "lens_id": "PG-LOCK-012",
  "slug": "lock_wait_graph",
  "domain": "pg",
  "title": "Граф ожидания блокировок",
  "question": "Кто блокировал ожидающего в момент снимка (`blocked_by` из `pg_locks`).",
  "text_locale": "ru",
  "confidence_cap": "high",
  "awaiting": ["sampled_blocked_by_edges", "lock_snapshot_coverage"],
  "requirements_status": "incomplete"
}
```

## Граница доказательств

Линза формирует проверяемую гипотезу, а не измеренный root cause. Допустимые роли
finding — `lead`, `amplifier`, `downstream` и `coincident`.

Направление причина→следствие выводится двумя путями. Структурно: ребро
`blocked_by` из снимка `pg_locks` доказывает, что держащий блокировку — причина
(`lead`), а ожидающий его — следствие (`downstream`); такой finding достигает
high confidence. По времени: один коллектор штампует все секции цикла общими
серверными часами, поэтому порядок фиксации сигналов сравним. Сигнал, чей эпизод
в инциденте начался раньше остальных, помечается `lead`, начавшийся позже —
`downstream`, при равном времени старта остаётся `coincident`. Время фиксации
считается временем события; на порядок внутри одного цикла съёмки движок не
претендует.

Событие лога не становится отдельной линзой только из-за нового источника данных.
Например, ENOSPC дополняет `OS-FS-027`, отказ подключения по лимиту —
`PG-CONN-014`, long lock wait — `PG-LOCK-012`, temp file — `PG-TEMP-003`, slow
query — `PG-QRY-001`, а ошибка архивации — `PG-ARCH-017`. Отдельными вопросами
могут стать только факты с другой семантикой и действием оператора, например
подтверждённый deadlock или точная отмена по timeout.

Лог-события проходят отдельным путём. `kronika-source-log` читает stderr-файл,
ограничивает число и размер строк, записывает gaps и хранит восемь типизированных
log-секций. Над ними работают восемь event-линз; их findings возвращаются в секции
`log` ответа, а `log.complete` остаётся `false` — stderr-источник не доказывает
исчерпывающего покрытия. Ошибки сгруппированы, session/backend identity не
сохраняется, continuation связывается по смежности, SQLSTATE зависит от
фактического stderr-формата, а coverage ротации и effective GUC неполны. Поэтому:

- SIGKILL не доказывает kernel OOM victim;
- PANIC не доказывает повреждение данных;
- `out of shared memory` нельзя смешивать с физическим RAM OOM;
- slow-query record подтверждает превышение настроенного порога;
- локальный `invalid record length` может быть обычным концом WAL;
- отсутствие события ничего не доказывает без полного source/config coverage.

SQL, параметры, IP, user/database, пути, conninfo и archive command могут содержать
секреты. Event-линзы редактируют их до публикации evidence и ключей; raw log payload
incident API не возвращает.

## Разработчику

Код подсистемы находится в `bins/pg_kronika-web/src/incident/`:

- `lenses.rs` — стабильные ID, slug, metadata и недостающие capabilities;
- `active.rs` — активные counter- и snapshot-линзы и их формулы;
- `gauge_contracts.rs`, `os_lenses.rs`, `query_plan.rs` — gauge-, OS- и
  query-линзы;
- `events.rs` — линзы над лог-событиями и их каталог;
- `lens.rs` — контракт `Lens`;
- `dispatch.rs` и `engine.rs` — допуск работы, вызов линз, output limits и
  назначение направления по времени фиксации сигналов;
- `evidence.rs` — предел уверенности и правила роли;
- `model.rs`, `series.rs`, `typed.rs`, `cluster.rs` — identity, входные ряды,
  typed counter/gauge evidence и кластеры;
- `incident_input.rs` — bounded adapter reader → anomaly episodes;
- `incident_response.rs` — JSON transport.

Чтобы активировать линзу, нужно предоставить все указанные capabilities, добавить
bounded typed input, реализовать `Lens`, передать её в `analyze()` и удалить запись
из dormant-каталога в том же изменении. Переписывать стабильный `lens_id` нельзя;
читабельное имя меняется отдельно через `slug`. Линза обязана соблюдать общий
request work budget, лимиты findings/evidence и детерминированную сортировку.

Активный каталог — 34 линзы: 28 над метриками и снимками плюс шесть линз над
лог-событиями (`PG-EVT-*`; ещё две event-линзы переиспользуют ID `OS-FS-027` и
`PG-CONN-014`). Актуальный перечень с доменом, `title` и `question` отдаёт сам API
в `catalog.applied` и `log.evaluated_lens_ids`. Полные формулы и DQ-правила — в
[контракте линз](superpowers/specs/2026-07-16-kronika-incident-lenses-design.md).
Границы модулей, response и resource model заданы в
[контракте реализации](superpowers/specs/2026-07-17-kronika-incident-implementation.md).
