# Incident-анализ

`GET /v1/incidents` группирует аномальные эпизоды по времени и узлу, затем
выполняет активные диагностические линзы над типизированными counter-дельтами.
Это диагностические гипотезы, а не измеренная первопричина: направленная роль
требует структурного evidence или доказанного порядка часов.

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
          "evidence": ["ratio"]
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
finding — `lead`, `amplifier`, `downstream` и `coincident`. Направленная роль
требует структурного evidence или доказанного порядка часов. Сейчас endpoint
использует `ClockRelation::Unknown`; прямым структурным evidence остаётся только
сохранённое ребро `blocked_by`.

Событие лога не становится отдельной линзой только из-за нового источника данных.
Например, ENOSPC дополняет `OS-FS-027`, отказ подключения по лимиту —
`PG-CONN-014`, long lock wait — `PG-LOCK-012`, temp file — `PG-TEMP-003`, slow
query — `PG-QRY-001`, а ошибка архивации — `PG-ARCH-017`. Отдельными вопросами
могут стать только факты с другой семантикой и действием оператора, например
подтверждённый deadlock или точная отмена по timeout.

Текущий `kronika-source-log` читает только stderr-файл. Он ограничивает число и
размер строк, записывает gaps и хранит восемь типизированных log-секций, но не
даёт incident-движку bounded raw-event input. Ошибки сгруппированы, session/backend
identity не сохраняется, continuation связывается по смежности, SQLSTATE зависит
от фактического stderr-формата, а coverage ротации и effective GUC неполны. Поэтому:

- SIGKILL не доказывает kernel OOM victim;
- PANIC не доказывает повреждение данных;
- `out of shared memory` нельзя смешивать с физическим RAM OOM;
- slow-query record подтверждает превышение настроенного порога;
- локальный `invalid record length` может быть обычным концом WAL;
- отсутствие события ничего не доказывает без полного source/config coverage.

SQL, параметры, IP, user/database, пути, conninfo и archive command могут содержать
секреты. Будущий event path обязан редактировать их до публикации evidence и ключей.
Текущий incident API такие log payload не возвращает.

## Разработчику

Код подсистемы находится в `bins/pg_kronika-web/src/incident/`:

- `lenses.rs` — стабильные ID, slug, metadata и недостающие capabilities;
- `active.rs` — активные counter-линзы и их формулы;
- `lens.rs` — контракт `Lens`;
- `dispatch.rs` и `engine.rs` — допуск работы, вызов линз и output limits;
- `evidence.rs` — предел уверенности и правила роли;
- `model.rs`, `series.rs`, `typed.rs`, `cluster.rs` — identity, входные ряды,
  typed counter evidence и кластеры;
- `incident_input.rs` — bounded adapter reader → anomaly episodes;
- `incident_response.rs` — JSON transport.

Чтобы активировать линзу, нужно предоставить все указанные capabilities, добавить
bounded typed input, реализовать `Lens`, передать её в `analyze()` и удалить запись
из dormant-каталога в том же изменении. Переписывать стабильный `lens_id` нельзя;
читабельное имя меняется отдельно через `slug`. Линза обязана соблюдать общий
request work budget, лимиты findings/evidence и детерминированную сортировку.

Полные формулы, DQ-правила и каталог 28 вопросов находятся в
[контракте линз](superpowers/specs/2026-07-16-kronika-incident-lenses-design.md).
Границы модулей, response и resource model заданы в
[контракте реализации](superpowers/specs/2026-07-17-kronika-incident-implementation.md).
