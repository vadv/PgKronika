# Health Score v1 и развитие диагностических сценариев

Дата: 2026-07-28. Статус: целевая design/spec для последовательных
implementation PR. Текущее покрытие сверено с `origin/main` на
`5b72cf9d3dd0782b456efdce0d35f69c92eb613c`.

## 1. Решение и границы

PgKronika развивает существующую модель «типизированный снимок → исторический
факт → bounded analytics → machine API» и не вводит параллельный контур
диагностики. Health Score v1 становится сводной проекцией сохранённых фактов,
но не заменяет события, critical findings, состояние источников и исходные
доказательства.

Документ фиксирует:

- только оставшиеся target gaps и компактный evidence-backed baseline;
- целевой контракт Health Score v1;
- будущие SQL/data contracts для недостающих фактов;
- API/UI-сценарии, ограничения безопасности и производительности;
- исполняемую приёмку и порядок отдельных implementation PR.

Production code в рамках этого документа не меняется. Здесь не задаются
готовые команды изменения PostgreSQL, завершения сеансов или удаления
объектов.

Термины:

- **факт PostgreSQL** — значение или семантика, определённые официальной
  документацией PostgreSQL 15–18;
- **начальная политика PgKronika** — изменяемые веса, пороги, правила и
  ограничения продукта; это не свойство PostgreSQL;
- **owner value** — явно настроенный владельцем лимит или бюджет; он имеет
  приоритет над начальным значением политики;
- **окно** — полуоткрытый интервал `[from_us, to_us)` в UTC;
- **достаточный сигнал** — вход, чья применимость, полнота, непрерывность,
  права и reset-семантика удовлетворяют versioned rule contract;
- **ложный ноль** — ноль, полученный из отсутствия строки, неполной выборки,
  отказа в доступе, reset или разрыва.

## 2. Что ещё предстоит

Все 24 target IDs остаются открытыми. Статус `частично` означает, что
production-wired часть контракта подтверждена, но ниже планируется только
остаток; prerequisite сам по себе не снижает target.

| Статус main | Количество | IDs |
| --- | ---: | --- |
| Реализовано | 0 | — |
| Частично | 21 | `HS-001..004`, `DATA-001..003`, `DATA-005..009`, `UX-001..004`, `UX-006`, `EXT-001`, `SAFE-001..003` |
| Будущее | 3 | `DATA-004`, `DATA-010`, `UX-005` |
| Отклонено/заменено | 0 | — |

На этом `main` уже работают shared `web_projection`, OVF
`UiSummary`/`EntitySeries`, selective reads и server-side
`/v1/ui/catalog`, `/v1/views/summary`, `/v1/timeline/heatmap` с committed
OpenAPI и hard format/read/request/response caps. Это data/API primitives для
существующих фактов, а не `health_score_v1`, target Health history,
per-database/evidence services, production browser state или generated
frontend client. Поэтому статусы и количества target IDs не меняются.
Positive-delta formulas этих проекций не заменяют canonical typed Health
window diff, а projection revisions/labels не являются persisted lifecycle
identity или lazy/redaction contract; `DATA-001`, `SAFE-002` и `SAFE-003`
также остаются частично реализованными.

Остаток выполняется в dependency order:

1. additive Health Score 0–100, canonical extractors, availability,
   completeness и critical ceiling (`HS-001..003`, `DATA-001`);
2. target score/history/per-database/evidence machine contract,
   runtime/OpenAPI ID parity и затем generated client/production UI
   (`HS-004`, `UX-004`, `UX-005`);
3. universal attempt/population/per-database coverage (`DATA-002`);
4. reloption-aware maintenance, sequences, complete horizon/worst axes и
   structural catalog с observation episodes (`DATA-003..007`,
   `SAFE-002`);
5. четыре ещё отсутствующих progress sources, inspector и опциональное
   physical evidence (`DATA-008..010`);
6. query/settings compare, schema history и остаток log search
   (`UX-001..003`, `UX-006`);
7. resource/privacy qualification каждого нового path (`SAFE-001`,
   `SAFE-003`) и scoped RBAC/audit/isolation поверх read-only machine API
   (`EXT-001`).

Точные уже работающие части и evidence вынесены в раздел 12. Они не являются
задачами траншей.

## 3. Health Score v1

### 3.1. Категории и веса

Stable category IDs и начальные веса:

| `category_id` | Вес | Основные существующие входы | Применимость |
| --- | ---: | --- | --- |
| `connections` | 0.15 | connection capacity, activity, idle-in-transaction, long transactions, session failures | instance/database |
| `performance` | 0.15 | database/query cache hit, query time/buffers, CPU/load, table HOT/newpage | instance/database |
| `storage` | 0.10 | PostgreSQL mount capacity, disk I/O/pressure | только при доказанном mapping |
| `replication` | 0.15 | role, senders, LSN/time lag, slots | `not_applicable` без реплик |
| `maintenance` | 0.15 | dead tuples, vacuum/analyze activity, backlog, autovacuum settings | `not_applicable` на standby |
| `mvcc_horizon` | 0.10 | XID/MXID database/table/TOAST horizons, prepared xacts, long xmin | primary и standby с доступными gauges |
| `wal_checkpoints` | 0.10 | WAL rate/FPI/buffers-full, timed/requested checkpoints, write/sync time | instance |
| `locks` | 0.10 | blocking graph, wait duration, deadlock delta | instance/database |

Сумма базовых весов равна `1.00`. Веса являются начальной политикой
`health_score_v1`, а не фактами PostgreSQL. Изменение веса, rule topology,
порогов или reduction semantics увеличивает `health_policy_version`; история
всегда возвращает версию, с которой была рассчитана точка.

### 3.2. Формула и детерминизм

Каждый rule выдаёт `penalty` в диапазоне `0.0..100.0`. Внутри категории
`category_penalty` равна максимальной penalty среди допустимых сработавших
rules. Это продолжает уже существующую в health kernel семантику «худший factor
ведёт домен» и не усредняет сильный сигнал со слабыми. Если все обязательные
входы категории доказанно доступны и ни одно правило не сработало, penalty
равна настоящему нулю.

Категория имеет один из статусов:

- `available` — обязательный factor set достаточен для окна;
- `unavailable` — категория применима, но данных недостаточно;
- `not_applicable` — политика доказала неприменимость.

Для `unavailable` и `not_applicable` effective weight равен нулю; нулевая
penalty им не присваивается. Для множества доступных категорий `A`:

```text
available_weight = sum(base_weight[i] for i in A)
effective_weight[i] =
  base_weight[i] / available_weight, если i in A
  0, иначе

raw_score =
  round_0_1(clamp(100 - sum(category_penalty[i] * effective_weight[i]), 0, 100))
```

Если `available_weight = 0`, `raw_score` и `score` равны `null`, а status —
`unavailable`. Вычисление использует fixed-point/rational arithmetic:
penalty хранится в десятых долях (`0..1000`), веса — целыми миллионными долями,
нормализация выполняется до финального округления. `round_0_1` округляет
неотрицательное значение к ближайшей десятой, ровно половину — вверх. На
промежуточных шагах округление запрещено.

Пример: если replication доказанно неприменима, а maintenance недоступна,
остальные шесть категорий участвуют с весами, нормализованными на их сумму
`0.70`. Coverage при этом остаётся partial: нормализация не превращает
пропущенные данные в полные.

### 3.3. Полнота

Score point возвращает одновременно:

- `applicable_base_weight` — сумма весов всех не-`not_applicable` категорий;
- `available_base_weight` — сумма весов `available`;
- `completeness = available_base_weight / applicable_base_weight`;
- число категорий по статусам;
- factor-level coverage с window, population, cadence, reset/gap и
  privilege state.

`status=complete` допустим, только если каждая применимая категория доступна.
При хотя бы одной `unavailable` числовой score может быть рассчитан по
оставшимся категориям, но `status=partial`; UI не окрашивает partial point как
«норма» без соседнего явного признака неполноты.
При `applicable_base_weight=0` completeness и score равны `null`, status —
`unavailable`, reason — `no_applicable_categories`; деление `0/0` не
выполняется. При ненулевом applicable weight и нулевом available weight
completeness равна `0.0`, а score остаётся `null`.

Replication получает `not_applicable` только при полном topology observation,
который доказывает отсутствие upstream/senders/слотов и не противоречит
owner-declared expectation. Неполная видимость даёт `unavailable` с typed
reason. Maintenance на standby получает `not_applicable {kind:
"standby_role"}`; её вес перераспределяется.

### 3.4. Rule contract и политика порогов

Каждый rule registry entry содержит:

```text
rule_id
rule_revision
category_id
required_inputs[]
optional_inputs[]
scope
applicability
reduction                    # max, ratio, higher_worse, lower_worse
parameters[]                 # name, value, unit, provenance
penalty_formula_revision
critical_ceiling_eligible
```

`provenance` принимает `owner`, `postgresql_setting` или `initial_default`.
Неявных числовых констант нет. Там, где имеется owner value, rule использует
его. Для XID/MXID используются effective значения сохранённых GUC и
per-relation reloptions после versioned PostgreSQL normalization: relation
freeze max ограничивается global max, а failsafe вычисляется отдельно.
Connection capacity использует сохранённые настройки и catalog limits.
Остальные начальные пороги обязаны находиться в versioned policy manifest и
возвращаться API, а не прятаться в UI.

Для монотонного показателя с порогами `warning` и `critical` стандартная
начальная функция:

```text
penalty = clamp(100 * (value - warning) / (critical - warning), 0, 100)
```

Для lower-is-worse направление меняется. Rule может использовать другую
формулу только с новым `penalty_formula_revision`, тестами монотонности и
публичными параметрами. Нулевой denominator или неизвестный budget делает
rule unavailable.

### 3.5. Critical ceiling

Подтверждённый catastrophic rule не подменяет penalty. Сначала вычисляется
`raw_score`, затем:

```text
score = min(raw_score, 30.0)
```

Если `raw_score=null`, ceiling не создаёт число: `score` остаётся `null`, а
`state=critical` определяется отдельным `critical_findings`. Начальное
значение ceiling `30.0` — политика PgKronika.

| `rule_id` | `category_id` | Подтверждающий факт | Начальная/default policy |
| --- | --- | --- | --- |
| `integrity.checksum_failure_delta` | `storage` | при co-temporal `data_checksums=on` reset-aware `checksum_failures` увеличился в окне; raw delta и last failure сохранены | любое подтверждённое приращение включает ceiling |
| `storage.disk_used_ratio` | `storage` | полный local mapping, `total_bytes > 0` и `used = total_bytes - available_bytes` с `available_bytes` по `f_bavail` | `used / total >= 0.90`; owner budget имеет приоритет |
| `mvcc.xid_failsafe_zone` | `mvcc_horizon` | максимальный доказанный XID age достигает effective `vacuum_failsafe_age` | фактический GUC/owner value; fallback `1_600_000_000` только как initial default |
| `maintenance.autovacuum_off` | `maintenance` | writable primary и effective global `autovacuum=off` | состояние настройки включает ceiling; anti-wraparound отдельно остаётся фактом PostgreSQL |
| `maintenance.track_counts_off` | `maintenance` | writable primary и effective `track_counts=off` | состояние настройки включает ceiling |
| `capacity.sequence_or_id_exhaustion` | `storage` | bounded sequence/id contract доказывает расход нециклического диапазона | `fraction_used >= 0.95`; owner threshold имеет приоритет |

Закрытые params этих findings:

| `rule_id` | Params |
| --- | --- |
| `integrity.checksum_failure_delta` | `failure_delta: U64Decimal`, `last_failure_us?`, `data_checksums: on` |
| `storage.disk_used_ratio` | `observed_ratio`, `threshold_ratio`, `threshold_provenance`, `storage_episode_id` |
| `mvcc.xid_failsafe_zone` | `age`, `effective_failsafe_age`, `configured_failsafe_age`, `global_autovacuum_max_age`, `threshold_provenance` |
| `maintenance.autovacuum_off` | `observed: false`, `server_role: primary`, `setting_sampled_at_us` |
| `maintenance.track_counts_off` | `observed: false`, `server_role: primary`, `setting_sampled_at_us` |
| `capacity.sequence_or_id_exhaustion` | `fraction_used`, `threshold_ratio`, `threshold_provenance`, `direction`, `effective_bound: I64Decimal`, `cycle: false`, `sequence_episode_id` |

Факт и классификация не смешиваются: например, `used_ratio=0.91` — факт,
`0.90` и ceiling — initial policy. Катастрофический finding остаётся в
`critical_findings` даже после завершения окна, если его evidence относится к
этой точке; число и цвет не могут его скрыть. Одновременно возвращаются все
degradations, включая те, чьи penalties не стали максимальными в категории.

Catastrophic rule оценивает собственную достаточность evidence независимо от
общего статуса категории. Он может включить ceiling и `state=critical`, когда
category unavailable из-за другого обязательного factor. Отсутствующее или
неполное evidence никогда не включает ceiling.

`state` — закрытый enum `unknown|normal|degraded|critical`, вычисляемый
сервером в строгом порядке:

1. непустой `critical_findings` либо `score < 50.0` даёт `critical`;
2. иначе непустой `degradations` либо `score < 80.0` даёт `degraded`;
3. иначе `status=complete` и `score >= 80.0` дают `normal`;
4. остальные случаи, включая partial без finding и `raw_score=null`, дают
   `unknown`.

Границы `50.0/80.0` имеют provenance `initial_default`, входят в
`health_policy_version=2` и возвращаются policy metadata. Response
`status=partial` остаётся отдельной осью и сам по себе не может дать
`state=normal`.

### 3.6. Окно, typed diff и evidence

Для cumulative inputs используются только PgKronika typed diff за выбранное
окно. Абсолютный since-reset counter не является penalty input.

- `Value` допускает расчёт при полной boundary/cadence coverage.
- `Reset`, `Gap`, `FirstPoint`, `Anomaly` и `NotCollected` сохраняются как
  typed состояния и не становятся нулём.
- Gauge используется с точным `sampled_at` и правилом window attribution.
- Top-N input содержит `source_total`, его качество, collected, cutoff и tail
  evidence. Неизвестный total остаётся неизвестным.
- Один factor не объединяет точки разных postmaster/boot/reset/catalog
  episodes.
- Query text, log text и object definitions не нужны kernel и не попадают в
  score response по умолчанию.

Каждая category возвращает `source_window`, полный список `fired_rule_ids`,
driving rule IDs, raw evidence refs и typed unavailability. Evidence ref
указывает стабильный `fact_id`, type/section/field, наблюдаемое значение,
единицу, diff state, sample/window и coverage. Display prose в evidence
отсутствует. Для category и finding списки refs сопровождаются
`evidence_total`, `evidence_tail` и `evidence_set_id`; полный bounded набор
доступен через evidence route, а усечение никогда не остаётся неявным.

### 3.7. Machine API

`GET /v1/health/score` возвращает одну evaluation без server-side locale.
Новый `GET /v1/health/history` возвращает историю score/category points для
`scope=instance|database`; database scope требует opaque database episode ID.
Существующий `/v1/timeline/health` и его schema остаются без изменений до
отдельного deprecation PR. `GET /v1/health/databases` выдаёт bounded
per-database points с cursor, а `GET /v1/health/evidence` раскрывает один
bounded evidence set по opaque token. Новые paths и модели сначала добавляются в Rust registry и
`bins/pg_kronika-web/openapi.json`, затем из OpenAPI генерируются frontend
client и models.

Пример locale-neutral ответа `GET /v1/health/score`:

```json
{
  "response_schema_version": 3,
  "score_contract": "health_score_v1",
  "health_policy_version": 2,
  "reduction_semantics_version": 2,
  "factor_set_id": "_vm7KhNmKRz_OUudBrs_ovrtOsYSMOyuD_WjWJckE2A",
  "fact_set_id": "zAy9JIcKY-Zb-jWuOgpK7JsrfPqZ3LyRA1x7DQ6Yblw",
  "evaluation_id": "7-d7IB3CFsDt9DWiJ98rKJjT9uof8W5dTQ1OwM9ZMnE",
  "scope": {
    "kind": "instance",
    "node_self_id": "node-7",
    "database_episode_id": null
  },
  "source_window": {
    "from_us": 1785196800000000,
    "to_us": 1785197700000000
  },
  "status": "partial",
  "raw_score": 72.0,
  "score": 30.0,
  "state": "critical",
  "coverage": {
    "applicable_base_weight": 0.85,
    "available_base_weight": 0.70,
    "completeness": 0.8235,
    "available_categories": 6,
    "unavailable_categories": 1,
    "not_applicable_categories": 1
  },
  "ceiling": {
    "value": 30.0,
    "applied": true,
    "rule_ids": ["storage.disk_used_ratio"]
  },
  "categories": [
    {
      "category_id": "connections",
      "status": "available",
      "base_weight": 0.15,
      "effective_weight": 0.214286,
      "penalty": 10.0,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": ["connections.capacity_pressure"],
      "driving_rule_ids": ["connections.capacity_pressure"],
      "reason": null,
      "evidence_refs": ["UQjetx7h0A2OFK1I8t3uPcomRSilroAqxaaC7hHswNc"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "dywjcAo4C8vjZ_rwdxlJ0yYZARWQoZe3pzXUzb6ox8o"
    },
    {
      "category_id": "performance",
      "status": "available",
      "base_weight": 0.15,
      "effective_weight": 0.214286,
      "penalty": 10.0,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": ["performance.cache_miss_ratio"],
      "driving_rule_ids": ["performance.cache_miss_ratio"],
      "reason": null,
      "evidence_refs": ["_OFTlQztJruPsT6bbIIS_HIP7fIxYV-aXDzi4s3pBFE"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "_fJRXVLUe323q8QWitTV70rWlAnRt4jrdv0MG_xztyg"
    },
    {
      "category_id": "storage",
      "status": "available",
      "base_weight": 0.10,
      "effective_weight": 0.142857,
      "penalty": 100.0,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": ["storage.disk_used_ratio"],
      "driving_rule_ids": ["storage.disk_used_ratio"],
      "reason": null,
      "evidence_refs": ["E_gLPgJRzoBcORMAFEvRy3HZGbQPUFqCwuC7fmApiK8"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "IUWJiGrEcTMvxzYMygTz7vVPuHxN16giXN57jzkHtlM"
    },
    {
      "category_id": "replication",
      "status": "not_applicable",
      "base_weight": 0.15,
      "effective_weight": 0.0,
      "penalty": null,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": [],
      "driving_rule_ids": [],
      "reason": {
        "kind": "no_replicas",
        "params": {}
      },
      "evidence_refs": ["LUk_JVWMQK1Yenmydvp814BKunleLbFvE-l5yzzzses"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "dr5LdyA8coGbgHPRYA-eOCDeygtfYuWurRyh8n7Z5ik"
    },
    {
      "category_id": "maintenance",
      "status": "unavailable",
      "base_weight": 0.15,
      "effective_weight": 0.0,
      "penalty": null,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": [],
      "driving_rule_ids": [],
      "reason": {
        "kind": "source_partial",
        "params": {
          "source_type_id": 1013004,
          "collected": 500,
          "source_total": 912
        }
      },
      "evidence_refs": [],
      "evidence_total": 0,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "UP8ko6VtQHjM-JbCudqVVZx3XR4bjxxMunnUIAut6UQ"
    },
    {
      "category_id": "mvcc_horizon",
      "status": "available",
      "base_weight": 0.10,
      "effective_weight": 0.142857,
      "penalty": 5.0,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": ["mvcc_horizon.xid_age_ratio"],
      "driving_rule_ids": ["mvcc_horizon.xid_age_ratio"],
      "reason": null,
      "evidence_refs": ["WaBC_g8fsiB2hmbN5Njx3y2lpEC0ZpVOayzb2uydROQ"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "gi_B_TUTygGV8RVxm6SGSVsZ6cPut5E0uvicYtJSZIA"
    },
    {
      "category_id": "wal_checkpoints",
      "status": "available",
      "base_weight": 0.10,
      "effective_weight": 0.142857,
      "penalty": 20.0,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": ["wal_checkpoints.requested_ratio"],
      "driving_rule_ids": ["wal_checkpoints.requested_ratio"],
      "reason": null,
      "evidence_refs": ["w-JuXbzUgguenFupu7lijD95JnfYIzu41yDNyoIFCo4"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "9-lVMOMJfNLm0dv7tuZU1HLcqAFAWDpVHnrqmdICOZc"
    },
    {
      "category_id": "locks",
      "status": "available",
      "base_weight": 0.10,
      "effective_weight": 0.142857,
      "penalty": 41.0,
      "source_window": {
        "from_us": 1785196800000000,
        "to_us": 1785197700000000
      },
      "fired_rule_ids": ["locks.wait_duration"],
      "driving_rule_ids": ["locks.wait_duration"],
      "reason": null,
      "evidence_refs": ["I36f7nQt2ETSMd8pWVhI_lMIubhgNenjkCfCrp5ILAU"],
      "evidence_total": 1,
      "evidence_tail": {"status": "complete", "reason": null},
      "evidence_set_id": "M4r8rx6anE5KxLs6VE_e6EhtTseGfAc_5-SVFRiOtAg"
    }
  ],
  "critical_findings": {
    "rows": [
      {
        "finding_id": "pA6Y1soUbywoywYd9emS6gwsFVAxy2J63Co2kfI-tsU",
        "finding_class": "critical_policy_breach",
        "rule_id": "storage.disk_used_ratio",
        "category_id": "storage",
        "evidence_refs": ["E_gLPgJRzoBcORMAFEvRy3HZGbQPUFqCwuC7fmApiK8"],
        "evidence_total": 1,
        "evidence_tail": {"status": "complete", "reason": null},
        "evidence_set_id": "S_3kD2EH7UXS6Dr1bbEZ7MijkEkfBBBL-QApko2EE9g",
        "params": {
          "observed_ratio": 0.91,
          "threshold_ratio": 0.90,
          "threshold_provenance": "initial_default"
        }
      }
    ],
    "returned": 1,
    "source_total": 1,
    "tail": {"status": "complete", "reason": null}
  },
  "degradations": {
    "rows": [
      {
        "finding_id": "2OtvB99wZgggii89nFkm8W43xijQKGCaUS6_BB3vUwc",
        "finding_class": "degraded_policy_breach",
        "rule_id": "locks.wait_duration",
        "category_id": "locks",
        "penalty": 41.0,
        "evidence_refs": ["I36f7nQt2ETSMd8pWVhI_lMIubhgNenjkCfCrp5ILAU"],
        "evidence_total": 1,
        "evidence_tail": {"status": "complete", "reason": null},
        "evidence_set_id": "ZFPAIjA60VqqRXXAAUfmMH5zogZ3WsJ28puMaG_F20Y",
        "params": {}
      }
    ],
    "returned": 1,
    "source_total": 1,
    "tail": {"status": "complete", "reason": null}
  }
}
```

Закрытые enums и typed reasons имеют stable IDs. `reason` всегда имеет форму
`{kind, params}` с закрытой схемой params; произвольная строка `reason` или
`context` запрещена. Product-owned English prose backend не возвращает.
Сырые значения PostgreSQL остаются данными и выдаются только в разрешённом
detail contract.

`critical_findings.rows[].params` и `degradations.rows[].params` имеют
закрытую schema, выбранную по `rule_id`; generic object запрещён. Восемь
categories всегда присутствуют. Начальные bounds: 256 grouped critical
findings, 256 degradations и 64 evidence refs на finding. Усечение возвращает
`source_total`, deterministic tail и `status=partial`. Каждый finding
возвращает `evidence_total`, `evidence_tail` и `evidence_set_id` для bounded
detail, поэтому лимит refs не является тихим усечением; critical tail нельзя
скрыть или превратить в ноль.

UI владеет EN/RU-каталогами по `category_id`, `rule_id`, `finding_class` и
reason kind. `finding_id` остаётся opaque identity экземпляра. Переключение
языка меняет только URL-параметр `locale`; запрос данных, score, sort,
source/scope/evidence identities и остальные URL-параметры не меняются.
Карточка всегда показывает score, completeness, число и список critical
findings, degradations и source window. Raw evidence открывается через
bounded доступный с клавиатуры drilldown; свернуть сам факт наличия critical
finding нельзя.

## 4. Общий контракт новых сохранённых фактов

Все новые catalog/diagnostic sources используют существующий registry и
versioning. Новый смысл или новая nullable-колонка получают новый `type_id`;
старые типы остаются читаемыми. Один источник не может выдавать одинаковую
физическую схему с разной семантикой на разных major.

Обязательный envelope новой логической секции:

```text
sampled_at
node_self_id
server_version_num
server_role
database_oid?                 # для per-database catalogs
database_episode_id?
query_contract_revision
collection_attempt_id
attempt_outcome               # complete | partial | not_collected |
                              # unavailable | not_applicable
reason?                       # closed {kind, params}
limits                        # rows, bytes, time, work/concurrency
returned
source_total?
source_total_quality          # exact | lower_bound | unknown
coverage
```

`not_collected` означает, что source был запланирован, но не получил slot,
подключение или cycle budget; это не пустая успешная выборка.
`not_applicable` допустим только при доказанной неприменимости source к роли,
версии или типу объекта. `partial` означает, что attempt состоялся, но
population, visibility либо объявленный work не покрыты полностью.
`unavailable` фиксирует состоявшуюся попытку без достаточного результата.
Каждый исход, кроме `complete`, несёт закрытый typed reason.

Object row дополнительно содержит catalog OID, stable typed identity и
`observation_episode_id`. Identity включает source, database episode, object
class и OID; имя, OID или `relfilenode` по отдельности не считаются вечными.
Доказанный drop завершает episode, последующее появление того же OID начинает
новый. После gap continuity имеет состояние `uncertain`; rewrite и rename
сохраняются как отдельные наблюдаемые изменения, а не угадываются.

Per-database source запускается только через покрытые подключения пула. Для
каждого connectable database сохраняется состояние `collected`,
`pool_limit`, `connect_failed`, `privilege_denied`, `cycle_budget_deferred`,
`query_timeout` или `excluded_by_owner`. Непокрытые базы участвуют в cluster
coverage и запрещают вывод «проблем нет».

Ни один новый source не блокирует основной collection loop:

- отдельный scheduler slot и bounded concurrency;
- существующие `statement_timeout`, `lock_timeout` и cycle DB budget либо
  более строгие source-specific limits;
- circuit breaker после повторяющихся ошибок;
- независимая запись attempt/coverage даже при пустом результате;
- deterministic top-N с устойчивым tie-break, `source_total`, cutoff и tail
  summary;
- лимиты rows, bytes и work применяются до неограниченной материализации и
  интернирования строк.

### 4.1. Reloption-aware autovacuum и autoanalyze

**Осталось.** Создать согласованный снимок всех effective autovacuum
параметров на уровне таблицы/TOAST, exact eligibility, backlog и полный
инвентарь отключённых таблиц. Нельзя строить строгий вывод простым
соединением уже сохранённых global settings и top-N table rows.

**Target contract.** Новый per-database source сохраняет bounded relation
inventory для ordinary/materialized relations и отдельно описывает
partitioned parents, TOAST и temporary exclusions. Reloptions разбираются
каталожной функцией `pg_options_to_table`, не regex или ручным split.

Для каждого параметра сохраняются:

```text
raw_global_value
raw_relation_value?
raw_toast_value?
effective_value?
effective_source             # global | relation | toast | unavailable
formula_revision
server_major
```

Состояния вычисляются раздельно:

- `routine_vacuum`;
- `insert_vacuum`;
- `autoanalyze`;
- `wraparound_protection`.

`autovacuum_enabled=false` отключает routine VACUUM/ANALYZE, но не
anti-wraparound; table-local `autovacuum_enabled=true` не включает routine
worker при global `autovacuum=false`. `track_counts=false` делает routine
eligibility недоступной. Поэтому «autovacuum off» и «wraparound protection
off» никогда не являются одним bool.

Partitioned parent не хранит tuples, не обрабатывается autovacuum и не
поддерживает эти storage parameters как обычная heap relation; его
autoanalyze state фиксируется отдельно. Для TOAST effective vacuum parameter
берётся из `toast.*`, а при его отсутствии — из соответствующего table/global
контекста по documented semantics. TOAST не получает autoanalyze rule.

Формулы повторяют major semantics PostgreSQL и сравниваются строго через `>`:

| Major | Routine vacuum threshold | Insert vacuum threshold | Analyze threshold |
| --- | --- | --- | --- |
| 15–17 | `base + scale * effective_reltuples` | `insert_base + insert_scale * effective_reltuples` | `analyze_base + analyze_scale * effective_reltuples` |
| 18 | `min(max_threshold, base + scale * effective_reltuples)`; `max_threshold=-1` отключает cap | `insert_base + insert_scale * effective_reltuples * percent_unfrozen` | `analyze_base + analyze_scale * effective_reltuples` |

Для exact server semantics формулы используют
`effective_reltuples = max(reltuples, 0)`. В PG18
`percent_unfrozen = 1`, если `relpages <= 0` или `relallfrozen <= 0`; иначе
`1 - min(relallfrozen, relpages) / relpages`. Это исключает деление на ноль и
отрицательную долю. `autovacuum_vacuum_insert_threshold=-1` отключает insert
trigger во всех четырёх major; в PG18
`autovacuum_vacuum_max_threshold=-1` отключает cap routine threshold.
Компоненты и специальные состояния формулы сохраняются рядом с threshold,
observed counter, `eligible`, `backlog = max(observed - threshold, 0)` и typed
reason.

Обязательные наборы результата:

- все отношения с effective `autovacuum_enabled=false`, включая явный
  table/TOAST override и global-off context;
- все отношения, у которых routine vacuum или insert vacuum eligible;
- все отношения, у которых exact reloption-aware autoanalyze threshold
  превышен;
- partitioned parents с `autoanalyze=not_applicable` и отдельным evidence о
  необходимости исторического ручного ANALYZE;
- tail summary для невыданной части популяции.

Превышение autoanalyze threshold материализует finding
`maintenance.stale_planner_stats` с relation episode, `n_mod_since_analyze`,
точным effective threshold и его компонентами, `last_analyze`,
`last_autoanalyze`, effective source, formula revision, source window и
coverage. Это свидетельство о достижении условия автоматического ANALYZE, а
не утверждение о плохом плане или обязательности немедленного ANALYZE.

Temporary tables получают `not_applicable` для daemon; foreign tables и
partitioned parents имеют отдельные versioned причины. TOAST не получает
ложный autoanalyze contract. На standby maintenance category остаётся
`not_applicable`, но raw settings/catalog facts могут сохраняться для
расследования.

Reset/gap между `n_mod_since_analyze` или `n_ins_since_vacuum` и параметрами
делает backlog partial. Eligibility — snapshot fact; оно не доказывает, что
worker уже должен был начать операцию, и не является remediation verdict.

**Acceptance.**

- Production-path BDD на PG15, 16, 17 и 18 проверяет global values,
  table/TOAST overrides, disabled states, exact version formulas,
  `reltuples=-1`, PG18 `relpages/relallfrozen` edges,
  `insert_threshold=-1`, empty/partitioned/temp cases и privilege denial.
- Golden rows фиксируют effective source и formula revision.
- Fixture с reset/gap не выдаёт нулевой backlog.
- Инвентарь отключённых таблиц доказывает population coverage либо явно
  сообщает partial/tail.

**Implementation order:** tranche C, отдельный source/type PR после coverage
foundation.

### 4.2. Sequence и identity exhaustion

**Осталось.** Создать sequence source: declared range, direction, cycle,
cached disk value, ownership/identity, privilege state и самый узкий
подтверждённый предел.

**Target contract.** Новый per-database catalog source объединяет семантику
`pg_sequence`, `pg_sequences`, `pg_attribute.attidentity` и dependency
catalog. Сохраняются:

```text
sequence_oid
sequence_episode_id
data_type
relpersistence
start_value
min_value
max_value
increment_by
cache_size
cycle
last_disk_value?
last_value_status
owned_relation_episode_id?
owned_attribute_number?
identity_kind?                # always | by_default
dependency_kind
effective_bound_source        # sequence | column_type
fraction_used?
remaining_unreserved_steps_lower_bound?
```

`last_disk_value` из `pg_sequences` может опережать фактически выданные
значения из-за CACHE. Поэтому поле не называется current value.
`last_value_status` различает `available`, `never_called`,
`privilege_denied`, `standby_unlogged` и `ambiguous`; `NULL` не угадывается.

Для положительного increment граница — верхняя, для отрицательного — нижняя:

```text
fraction_used_ascending  = (last_disk_value - min_value) / (max_value - min_value)
fraction_used_descending = (max_value - last_disk_value) / (max_value - min_value)
remaining_unreserved_steps_lower_bound =
  floor(abs(effective_bound - last_disk_value) / abs(increment_by))
```

Арифметика выполняется в widened numeric domain до вычитания и затем
ограничивается диапазоном `0..1`; нулевой диапазон/шаг и отсутствующий disk
value дают typed unavailable. `pg_sequences.last_value` обозначает последнее
значение, записанное на диск, и при CACHE может быть концом уже
зарезервированного блока. Поэтому `remaining_unreserved_steps_lower_bound`
описывает ещё не зарезервированную часть диапазона и является консервативной
нижней границей общего headroom, а не обещанием точного числа будущих
`nextval`.

Для `CYCLE` exhaustion получает `not_applicable`; reuse risk показывается
отдельно. Если sequence привязана к smallint/int/bigint identity column,
effective bound берёт самый узкий доказанный предел sequence и типа колонки.
Domain/CHECK constraint остаётся evidence и не меняет bound без отдельного
строгого анализатора. Unowned sequence не считается unused.

Начальный critical threshold `fraction_used >= 0.95` имеет provenance
`initial_default`; owner value приоритетен. В finding входят direction,
effective bound, cache, cycle и privilege/coverage.

**Acceptance.**

- PG15–18 BDD покрывает ascending/descending, cache, cycle, never-called,
  identity always/by-default, serial ownership, unowned, unlogged standby и
  privilege denial.
- Property tests проверяют обе формулы на краях типов и отсутствие overflow.
- Top-N сортируется по `fraction_used DESC`, затем stable identity; response
  содержит полный total/tail.

**Implementation order:** tranche C, отдельный source/type PR.

### 4.3. XID/MXID drilldown и worst tables

**Осталось.** Дополнить существующие horizon/table facts: сохранить
`source_total`/tail для horizon candidates, применить global clamp и добавить
гарантированные оси worst dead ratio, HOT failure и newpage update.
Существующий top-N не доказывает полноту такого ранжирования.

**Target contract.**

- Расширить `1_031` новой версией либо совместимой coverage section, не
  создавать параллельную horizon model.
- Хранить по каждой базе independent XID/MXID population total, returned,
  cutoff, base/TOAST winner, raw reloption, global limit,
  `effective_relation_freeze_max_age = min(relation_or_global, global)` и
  per-database collection state. То же правило действует отдельно для MXID.
- API drilldown сначала показывает database horizon, затем bounded table/TOAST
  rows; incomplete database pool запрещает cluster-safe verdict.
- Для table stats добавить versioned full-population boundary snapshot в
  пределах database work cap. Если cap превышен, источник сохраняет partial
  population, а API может назвать только худшую строку среди покрытых.
- Из boundary snapshots typed diff строит interval axes
  `non_hot_update_ratio` и, PG16+, `newpage_update_ratio`; абсолютные
  since-reset counters в ranking запрещены.
- `dead_tuple_ratio` остаётся snapshot estimate:
  `n_dead_tup / (n_live_tup + n_dead_tup)`. Raw numerator, denominator,
  estimate provenance и tail cutoff сохраняются; нулевой denominator даёт
  typed unavailable.
- На PG15 newpage factor имеет
  `{kind:"unsupported_server_major", params:{input_id,server_major:15}}`.

Для одного reset episode interval formulas равны:

```text
non_hot_update_ratio =
  max(delta(n_tup_upd) - delta(n_tup_hot_upd), 0) / delta(n_tup_upd)
newpage_update_ratio =
  delta(n_tup_newpage_upd) / delta(n_tup_upd)
```

Каждый operand обязан быть `Value`; `delta(n_tup_upd)=0` даёт typed
zero-denominator, а reset/gap/first point/not-collected остаются исходным diff
state.

XID/MXID являются gauges и не проходят counter diff. Effective failsafe
вычисляется отдельно как сохранённый `vacuum_failsafe_age` /
`vacuum_multixact_failsafe_age`, поднятый при необходимости до
документированного floor 105% соответствующего global autovacuum max-age.
Relation freeze limit не заменяет failsafe threshold и возвращается отдельным
полем. Значение 1.6B — лишь initial default/fallback policy. XID и MXID
никогда не объединяются в одну шкалу.

**Acceptance.**

- PG15–18 BDD: database/table/TOAST winner, custom reloption, pool partial,
  reloption выше global limit, effective failsafe floor, privilege denial,
  standby и empty database.
- Fixtures доказывают, что объект вне прежних шести top-N осей попадает в
  новую worst axis при complete boundary population, а partial population не
  выдаёт глобальный worst claim.
- Typed-diff fixtures покрывают reset/gap/first point, нулевой denominator и
  обе interval formulas.
- Coverage/tail присутствуют даже для пустого полного результата.

**Implementation order:** tranche C, coverage PR до API drilldown.

### 4.4. Index states, constraints и structural hygiene

**Осталось.** Создать полный bounded catalog source с structural identity,
constraints/FK/index fingerprints и episodes. Существующих operational top-N
flags и display text недостаточно для исторического schema finding.

**Target contract.** Per-database schema snapshot хранит normalized catalog
facts и materialized finding rows. Общая object identity следует envelope из
раздела 4.

Отбор разделён на независимые deterministic axes:

- indexes с `NOT indisvalid`, `NOT indisready` или `NOT indislive`;
- constraints с `NOT convalidated`, а на PG18 отдельно с
  `NOT conenforced`;
- все FK structural rows в пределах database population cap;
- все index structural rows в пределах database population cap и bounded
  pair-comparison work;
- отдельные materialized axes для duplicate/overlap/type/nullability/array
  advisories.

Каждая axis сохраняет `returned`, `source_total`,
`source_total_quality`, stable cutoff, membership и tail reason. Отрицательный
вывод разрешён только для exact complete population соответствующей axis.
Tranche C не зависит от новых progress types: без co-temporal operation
evidence переходное index state остаётся `race_unknown`. Tranche D позже
обогащает его сохранённым progress episode.

#### Invalid/not-ready indexes

Состояние строится независимо из `indislive`, `indisready`, `indisvalid` и
связи с active `pg_stat_progress_create_index`, constraint ownership и
partition hierarchy:

- `active_build`;
- `active_reindex`;
- `partition_hierarchy_incomplete`;
- `drop_in_progress`;
- `persistent_unexplained`;
- `race_unknown`.

`indisvalid=false` не равен автоматически повреждению: failed concurrent build
и незавершённый partitioned parent имеют разный контекст.
`indisready=false` означает, что DML не поддерживает индекс; `indislive=false`
может означать drop in progress. Один sample не создаёт claim о ненужности.

#### Unvalidated constraints

Version adapter хранит `contype`, `convalidated`, hierarchy и major-specific
поля:

- PG15–16: CHECK/FK validation и известные constraint types;
- PG17: domain NOT NULL constraint type;
- PG18: table NOT NULL в `pg_constraint`, отдельный `conenforced` и temporal
  `conperiod`.

`not_validated` означает, что существующие данные не были полностью
проверены, а не что нарушение найдено. В PG18 validation и enforcement —
независимые оси. Partition children группируются по `conparentid`.
Structural constraint fingerprint включает ordered `conkey`, `conexclop`,
type-specific operator data, `conperiod`, `conenforced` и их applicability по
major; неприменимое поле типизировано, а не заменено пустым массивом.

#### Foreign keys

Structural FK fingerprint включает:

```text
source/target relation episode IDs
ordered conkey/confkey
ordered conpfeqop/conppeqop/conffeqop
match/update/delete actions
confdelsetcols
deferrability
validation/enforcement
temporal flag
conperiod and conexclop where applicable
partition parent
```

Duplicate fingerprint — точное structural evidence. Overlap/prefix — advisory,
не эквивалентность. Для каждой пары колонок сохраняются `atttypid`, typmod,
collation, formatted display type и equality operator. Type/typmod mismatch
показывается как evidence; implicit casts и workload не угадываются.

Nullable FK хранит `attnotnull` vector и MATCH type. Для MATCH SIMPLE любой
NULL component может исключить строку из проверки; MATCH FULL разрешает все
NULL, но не смешанный набор. Поэтому nullable FK — evidence для review, не
вердикт. PostgreSQL не создаёт автоматически индекс на referencing side;
наличие/отсутствие подходящего structural prefix фиксируется отдельно.

#### Equivalent и redundant indexes

Fingerprint строится только внутри одного server major, database и base
relation и включает:

```text
access method
ordered key slots and exact expression nodes
opclasses
collations
indoption/order/null ordering
predicate
INCLUDE tail and boundary
unique/nulls-not-distinct/exclusion/immediate flags
constraint ownership through conindid
replica identity
parent index and partition hierarchy
```

`pg_get_indexdef` остаётся display field и не участвует в identity. Raw
`pg_node_tree` для expressions/predicate не сравнивается между major; после
upgrade fingerprint пересчитывается в новом episode. Exact duplicates можно
группировать hash-based. Prefix analysis ограничен одной таблицей и cap на
число индексов, чтобы исключить неограниченный O(n²).

Constraint-owned, replica-identity и partition indexes никогда не получают
claim `safe_to_drop`. Names, tablespace и storage parameters сохраняются как
физические различия. Nonunique B-tree по array column — только
`low_confidence` advisory: B-tree может обслуживать equality/order, а другой
AM — array operators.

#### Явно отклонённые сокращения

- regex-сравнение `indexdef`;
- вывод «missing index» только по `seq_scan/idx_scan`;
- claim `safe_to_drop` по одному или короткому окну;
- неограниченный scan каталогов;
- DDL/remediation SQL в response.

**Acceptance.**

- PG15–18 BDD покрывает major differences, concurrent build states,
  partitioned index/constraint hierarchy, expression/partial/INCLUDE indexes,
  opclass/collation/order, FK actions/MATCH, typmod mismatch и privilege
  visibility.
- Golden fingerprints устойчивы к rename и меняются при любом structural
  различии; across-major comparison запрещён типом.
- Catalog history отличает rename, rewrite, drop/recreate и uncertain gap.
- Resource test покрывает database с большим числом objects и доказывает
  rows/bytes/time/work caps.

**Implementation order:** tranche C несколькими независимыми PR:
catalog facts → stable identity/history → findings/API.

### 4.5. Progress views

**Осталось.** Добавить stored types, version adapters и исторические episodes
для create-index, analyze, cluster и basebackup; UI должен отличать невидимую
операцию от отсутствующей. Существующий vacuum source не переделывается.

**Target contract.** Добавить cluster-sampled stored types для:

- `pg_stat_progress_create_index`;
- `pg_stat_progress_analyze`;
- `pg_stat_progress_cluster`;
- `pg_stat_progress_basebackup`.

Create-index/analyze/cluster rows имеют database/object scope; basebackup —
cluster scope. Phase/command нормализуются в closed versioned enums, а
неизвестное новое значение получает reason
`unknown_version_value {input_id, server_major}`. Исходное bounded значение
может сохраняться только как permission-aware literal detail и не входит в
reason, cursor или telemetry label. Поля чужих sessions могут быть NULL без
`pg_read_all_stats`; coverage отражает visibility.

Totals `0`/`NULL` и phase-specific counters не образуют процент. Процент
вычисляется только для разрешённой phase и положительного denominator.
`backup_total` является estimate и может измениться. PG18 `delay_time` для
ANALYZE хранится только в PG18 layout.

Episode identity включает target, PID, backend start/first seen,
command/object. Gap разрывает continuity. Пустой полный снимок означает «в
момент sample нет видимых активных операций», а не исторический ноль.

**Acceptance.** Для каждого нового stored type — production-path BDD на
PG15–18, own/foreign session visibility, empty/full/partial, phase transitions,
zero/NULL totals, PID reuse и gap. Collector benchmark подтверждает
неблокирующий bounded sample.

**Implementation order:** tranche D, отдельный PR на каждую логическую группу
layout.

### 4.6. Bounded object inspector

**Осталось.** Создать bounded inspector и одну object identity, связывающую
columns, types, indexes, checks, обе стороны FK, partition hierarchy, sizes и
maintenance state. Он использует существующие stored stats только как вход.

**Target contract.** UI передаёт opaque typed object ID; server разрешает его
в exact database/object episode. Free-form SQL и интерполяция имени
запрещены. Один ответ ограничен одним base object и содержит:

- columns: type/typmod/collation/nullability/default/generated/identity;
- indexes и constraint ownership;
- checks;
- FKs и referenced-by;
- parent/children/partition bounds;
- table/index size и row estimates;
- vacuum/analyze state и coverage.

Основной ответ строится из сохранённых catalog facts. Explicit asynchronous
refresh — отдельная bounded операция с RBAC, concurrency, rows/bytes/work,
`statement_timeout` и `lock_timeout`; UI request не запускает arbitrary live
diagnostics. Большие partition trees и referenced-by списки пагинируются.
Decompiled definitions являются display data, могут содержать literals и
подчиняются redaction/privilege policy.

**Acceptance.** Exact-OID query fixtures, pagination/cursor binding, large
partition tree, object disappearance/recreate, privilege denial, redaction,
timeout и concurrent admission. Один request не читает больше объявленного
object/segment/catalog budget.

**Implementation order:** tranche D после structural catalog.

### 4.7. Опциональный `pgstattuple_approx`

**Осталось.** Создать policy-gated одиночный physical scan с typed
privilege/load/timeout outcomes. Существующие table estimates не заменяют
такой факт.

**Target contract.** Путь выключен по умолчанию и доступен только явным
on-demand запросом для одного allowlisted ordinary/materialized relation OID.
PgKronika не устанавливает extension. Typed состояния:
`not_installed`, `privilege_denied`, `disabled_by_policy`, `timeout`,
`load_guard`, `partial` и `available`.

`pgstattuple_approx` может просмотреть до 100% страниц. Поэтому обязательны:
relation size/page/work cap, lock/statement timeout, concurrency=1 на target,
cancel, load guard и RBAC. Сохраняются table length, `scanned_percent`, exact
dead-tuple fields, approximate live/free fields, extension version, sample,
role и coverage. Результат называется physical dead/free evidence, а не
точным bloat и не основанием для remediation.

**Acceptance.** PG15–18 BDD с extension present/absent, default role and
explicit grant, 0/partial/100% scan, timeout/cancel/load guard. Resource test
проверяет hard cap и отсутствие вызова из обычного page load.

**Implementation order:** последняя часть tranche D.

## 5. Product actions

Ниже перечислены только ещё отсутствующие product actions. Работающие
timeline/anomaly/incident/raw-section и server-side UI
catalog/summary/heatmap primitives перечислены в baseline раздела 12.

### 5.1. Health history, per-database и drilldown

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Перейти на target formula и добавить category history, per-database score и rule evidence route. | `GET /v1/health/score` — одна подробная evaluation; новый `/v1/health/history` — bounded score/category history; `/v1/health/evidence` — cursor-bound rule/evidence detail. Существующий timeline contract не меняется. `scope=instance|database`; database выбирается opaque episode ID. | Detail и history с одинаковым `evaluation_id` совпадают; per-database isolation; instance-only evidence не размножается по базам; cursor связывает source, scope, policy, fact set, filters и redaction revision. | B |

Существующий `/v1/views/summary` возвращает population/status/notable для
generic UI views и не является Health evaluation или category history.

Instance score не усредняет базы. Для database-scoped factor сначала строится
penalty каждой покрытой базы, затем instance category берёт worst penalty и
возвращает driving database IDs. Непокрытая connectable database делает
instance category partial. Per-database score не присваивает instance-only
CPU/disk/WAL evidence каждой базе: такой factor помечается
`not_applicable {kind:"scope_mismatch", params:{...}}`.

`required_inputs` и applicability версионируются отдельно для двух scopes:

| Category | Instance scope | Database scope |
| --- | --- | --- |
| `connections` | global capacity, sessions и activity всех покрытых баз | sessions/activity/long transaction только выбранной базы; global capacity factor неприменим |
| `performance` | database/query/table factors плюс доказанные OS CPU/load | database/query/table factors выбранной базы; OS factors неприменимы |
| `storage` | local PostgreSQL storage mapping и owner budgets | только при owner per-database/tablespace budget и доказанной object attribution; иначе `scope_mismatch` |
| `replication` | topology/lag/slots; `no_replicas` при полном доказательстве | неприменима, пока нет отдельного database-scoped replication contract |
| `maintenance` | worst покрытой базы на writable primary | relation facts выбранной базы; неприменима на standby |
| `mvcc_horizon` | worst database/table/TOAST horizon | horizon выбранной базы и её relations |
| `wal_checkpoints` | instance WAL/checkpoint facts | неприменима |
| `locks` | полный покрытый blocking graph и deadlock deltas | граф и deadlock deltas выбранной базы |

Веса нормализуются заново по применимым и доступным категориям данного scope.
Нельзя переносить instance penalty в каждую базу или считать
scope-inapplicable category нулевой.

Точка history содержит source window, raw/final score, state, completeness,
восемь category summaries, critical/degradation rule IDs и
`health_policy_version`.
Downsampling выбирает худшую точку по final score, затем critical count,
completeness и времени; среднее не скрывает короткий critical interval.

### 5.2. Query A/B по двум сохранённым окнам

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Добавить двухоконный domain API, continuity-aware matching, общее coverage и UI поверх stored statements/plans/diff. | `GET /v1/compare/queries` принимает один source, database episode и окна A/B. Обе стороны вычисляются только из stored facts через typed diff; query/plan text загружается отдельным detail. | Все `Value/Reset/Gap/FirstPoint/Anomaly/NotCollected`; stable match/sort/cursor; plan absent; top-N partial; privacy/redaction; resource benchmark двух максимальных окон. | E |

Один или два независимых запроса текущего heatmap route не образуют
continuity-aware query A/B contract.

Основная identity: поддерживаемые поля соответствующего
`pg_stat_statements` contract — database, role, query ID и `toplevel`.
Matching требует наблюдаемый query ID и доказанную непрерывность
provider/algorithm, server major, extension version, reset episode и fact
set. `compute_query_id=off` сам по себе не запрещает matching: ID может
предоставить сторонний provider. Неизвестный provider, collision или смена
любой оси continuity делают matching typed-unavailable. Текст запроса не
является ключом.

Для каждой стороны возвращаются exact window, calls, rows, execution/planning
time, block/temp/WAL deltas и rates с `dt_us`. Change вычисляется только при
двух `Value` operands. Plan evidence связывается по сохранённой provenance;
отсутствие плана не подменяется пустым plan. Listing не содержит plan text.

Пример одной строки:

```json
{
  "entity_id": "jSZ4bniITyML-oX0kJIDFYq47o0VlFUtRGgwIvNkjN4",
  "a": {
    "window": {"from_us": 0, "to_us": 100000000},
    "calls": {"kind": "value", "delta": "120", "rate_per_second": 1.2}
  },
  "b": {
    "window": {"from_us": 200000000, "to_us": 300000000},
    "calls": {"kind": "nodata", "nodata": "reset"}
  },
  "change": {
    "exec_ms_per_call": {
      "kind": "nodata",
      "reason": {
        "kind": "incomplete_operand",
        "params": {"side": "b"}
      }
    }
  },
  "plan": {
    "status": "unavailable",
    "reason": {
      "kind": "required_input_not_collected",
      "params": {"input_id": "query.plan"}
    }
  },
  "evidence_refs": []
}
```

Ответ содержит `returned`, `source_total` или typed unknown-total reason,
coverage и tail evidence отдельно для A/B. Sort: выбранная metric, затем
canonical entity ID. Comparison не утверждает причинность.

### 5.3. `pg_settings` compare

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Добавить выбор двух moments и typed объяснение границ materialization поверх last-known settings. | `GET /v1/compare/settings` выбирает ближайшие допустимые сохранённые снимки не позже A/B, возвращает requested/sample time, value/unit/source/pending restart и `added|removed|changed|unchanged`. | Segment boundary, absent old setting, extension setting, pending restart, partial segment, stable pagination и отсутствие server-side prose. | E |

Сравнение не читает live `pg_settings`. Если last-known snapshot нельзя
доказать в пределах выбранного fact set, сторона unavailable. File path
`sourcefile` является privileged literal detail и не попадает в listing.

### 5.4. Исторические schema problems

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Добавить полный catalog snapshot, finding episodes и structural fingerprints. | После tranche C UI показывает появление, изменение, исчезновение и uncertain gap для invalid indexes, unvalidated constraints, FK/index advisories. Finding ID включает rule revision и object episodes. | Rename/rewrite/drop-recreate, major upgrade, partial database, finding open/close/reopen, stable URL и evidence drilldown. | C API после catalog facts |

История не ретроспективно применяет новую rule revision к старым findings без
явного recompute marker. «Не найдено» при partial coverage не закрывает
episode.

### 5.5. URL, timezone и investigation context

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Добавить production URL state, одну IANA timezone и воспроизводимый sanitized context export. | URL хранит source, database/scope, A/B ranges, active view/tab, category/rule/entity, filters, sort, page/cursor, zoom, locale и один `tz` — каноническое IANA name. Sensitive literal search хранится только в local fragment; wire timestamps остаются UTC. | Property tests URL encode/decode, reload, back/forward, fragment roundtrip, invalid IANA fallback, DST boundary и EN/RU parity. | B, расширение в E |

Query parameters текущих HTTP routes не являются browser URL-state
implementation: отсутствуют client codec, reload/back-forward state, IANA
timezone и context export.

В приложении одновременно действует ровно одна IANA timezone. Компонент не
может иметь скрытый локальный timezone override. Абсолютные timestamps,
duration и server-provided values не смешиваются.

Действие «контекст расследования в буфер обмена» по умолчанию включает product
version, source/scope, UTC ranges и IANA timezone, evaluation/finding/entity
IDs, policy versions, безопасные enum-фильтры, completeness и sanitized
canonical URL. SQL, plan/log text, object definitions, paths, usernames и
literal filter values исключены; literal detail требует отдельного явного
opt-in, предупреждения и текущей redaction policy. Полный локальный URL state
может держать literal search только во fragment, который браузер не отправляет
HTTP-серверу; действие copy/share по умолчанию удаляет fragment.

### 5.6. OpenAPI-generated frontend

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Создать production frontend и generated client/models; существующий committed OpenAPI является только prerequisite. | Client и models генерируются только из committed OpenAPI. Product view models могут оборачивать generated types, но не повторяют wire enums вручную. | Clean generation diff, OpenAPI↔Rust registry tests, exhaustive category/rule/reason handling и CI failure при stale generated files. | B |

Текущие Rust-модули `ui::{catalog,data,handlers,heatmap}` и описанные в
OpenAPI server routes не содержат generated client/models, browser build или
production frontend и не меняют статус `UX-005`.

API следует действующему контракту
`docs/superpowers/implemented/specs/2026-07-21-i18n-machine-api-contract.md`:
`Accept-Language` не влияет на success/problem data, backend не возвращает
translation keys, `title`, `detail`, finding summary или product prose.
Frontend содержит полные EN/RU catalogs по stable IDs.

### 5.7. Log search

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Добавить text search, histogram/zoom, facets, event dedup, scan accounting и log-search cursor, связанный с body/facets/redaction. Существующий authenticated timeline cursor переиспользуется как primitive, но не считается готовым search contract. | `POST /v1/logs/search` работает только по stored facts. Bounded text в JSON body плюс allowlisted typed include/exclude facets; regex отсутствует. Histogram и rows закреплены на одном `fact_set_id`. | Histogram/rows consistency, zoom, include/exclude, identity dedup, cursor binding, gap/partial/scanned, redaction и worst-range benchmark. | E |

Текущий events count heatmap не принимает текст, facets или search cursor и
не является log search.

Response shape:

```json
{
  "fact_set_id": "CLQGhlwBfocgQJ6BH4W87_Mm-Ee3HWtiZNU1lRpf-kU",
  "range": {"from_us": 0, "to_us": 1000000},
  "histogram": {
    "bucket_us": 100000,
    "buckets": [
      {
        "from_us": 0,
        "to_us": 100000,
        "count": 4,
        "count_quality": "lower_bound"
      }
    ],
    "coverage": {
      "status": "partial",
      "gaps": [{"from_us": 700000, "to_us": 900000}]
    }
  },
  "rows": [],
  "dedup": {
    "mode": "event_identity",
    "returned_groups": 0,
    "retained_occurrences": 0
  },
  "scan": {
    "status": "partial",
    "segments_scanned": 32,
    "rows_scanned": 50000,
    "bytes_scanned": 8388608,
    "reason": {
      "kind": "scan_budget",
      "params": {"required": 40, "available": 32}
    }
  },
  "page": {
    "next_cursor": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
  }
}
```

Dedup выполняется по canonical event identity и typed fields, не по
rendered/localized message. Cursor является opaque authenticated state и
серверно связывает source, absolute range, keyed fingerprint нормализованного
body, include/exclude, dedup, sort, fact set и redaction revision; fingerprint
и raw text наружу не выдаются. Zoom меняет только absolute range и сбрасывает
cursor. Raw text передаётся только в body; reverse-proxy и application access
logs не записывают body или raw request target.

`count_quality=exact` допустимо только для bucket с полной source visibility,
полным scan coverage и без gap на всём его интервале. Иначе значение —
`lower_bound` или `unknown`; каждый ответ возвращает bounded histogram
coverage и gaps, поэтому непросмотренный интервал не выглядит нулевым.

Действие «Grafana range» помещает в буфер
`from=floor(from_us/1000)&to=ceil(to_us/1000)`. Backend не формирует
vendor-specific URL. Search UI явно показывает `partial`, scanned
segments/rows/bytes, gaps и dedup occurrence count.

### 5.8. Read-only external investigation interface

| Осталось | Target contract | Acceptance | Order |
| --- | --- | --- | --- |
| Добавить scoped deny-by-default RBAC, source/database isolation, audit и покрытие будущих Health/compare/log services. Новый transport не обязателен. | Существующий read-only HTTP либо отдельный adapter вызывает те же bounded application services, возвращает stable IDs, typed provenance/coverage и никогда не выполняет write/remediation methods. | RBAC deny-by-default, source/database isolation, audit, redaction и cursor/budget parity. | F |

Расширение внешнего доступа выполняется только после стабилизации
Health/compare/log APIs. Оно не получает прямой доступ к live PostgreSQL,
filesystem или внутренним reader типам и не обходит application admission.

## 6. Stable IDs, reasons и i18n

Additive contract получает `score_contract="health_score_v1"`, но не
переиспользует существующий `health_policy_version=1`, который уже обозначает
другую формулу. Первый implementation PR назначает
`health_policy_version=2`, `reduction_semantics_version=2` и несовместимую
`response_schema_version=3`; последующие policy changes увеличивают
`health_policy_version`.
`response_schema_version`, `rule_revision`, `formula_revision`,
`registry_contract_version`, `fact_set_id` и `evaluation_id` остаются
раздельными осями.

В target response schema `factor_set_id`, `fact_set_id`, `evaluation_id`,
`evidence_id`, `finding_id`, query/object/database episode IDs получают форму
`B64UrlSha256`: 43 символа unpadded base64url. Первый Health machine-contract PR
исправляет несовпадение текущего 22-символьного runtime `FactorSetId` с этим
OpenAPI contract. К IDs не добавляются текстовые prefix. Cursor использует
отдельный versioned authenticated
`OpaqueCursorV1`, base64url длиной `64..1024`; он не является durable ID.
Registry IDs (`rule_id`, `input_id`, `policy_id`) ограничены 96 ASCII
символами и pattern
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$`; revision хранится отдельным полем и
не кодируется в display text.

В новых моделях `response_schema_version=3` сырые 64-bit counters, bounds и byte
offsets передаются десятичными строками.
`U64Decimal` имеет pattern `^(0|[1-9][0-9]{0,19})$` и проверку значения
`<= 18446744073709551615`; `I64Decimal` — pattern
`^(0|-?[1-9][0-9]{0,18})$` и проверку диапазона signed 64-bit. Производные
counts, ограниченные API cap, имеют тип `SafeInteger` с максимумом
`9007199254740991`. Это несовместимое изменение не применяется молча к старым
response schemas. Generated frontend не преобразует decimal types в
JavaScript `number`; golden roundtrip включает `u64::MAX`, `i64::MIN` и
`i64::MAX`. Unix microseconds остаются signed integer, но target API
ограничивает поддерживаемый временной диапазон `SafeInteger`.

Канонический request contract:

| Endpoint | Параметры | Ограничения |
| --- | --- | --- |
| `GET /v1/health/score` | `source: U64Decimal`, `from/to: UnixMicrosSafe`, `scope: instance|database`, optional `database_episode_id: B64UrlSha256` | `[from,to)`, не более 31 суток; database ID обязателен только для database scope |
| `GET /v1/health/history` | `source`, `from/to`, `scope`, optional `database_episode_id`, `step: positive duration`, `limit: 1..2000`, optional `cursor: OpaqueCursorV1` | предел 31 сутки; cursor связан со всеми полями и версиями |
| `GET /v1/health/databases` | `source`, `from/to`, `limit: 1..100`, optional `cursor` | один interval; только instance caller scope |
| `GET /v1/health/evidence` | `source`, `evaluation_id`, `evidence_set_id`, `limit: 1..100`, optional `cursor` | IDs и cursor должны принадлежать одному fact set/redaction revision |
| `GET /v1/compare/queries` | `source`, `database_episode_id`, `a_from/a_to`, `b_from/b_to`, `metric`, `limit: 1..200`, optional `cursor` | оба полуоткрытых окна не более 24 h |
| `GET /v1/compare/settings` | `source`, `a_at/b_at: UnixMicrosSafe`, `limit: 1..2000`, optional `cursor` | moments входят в доступный fact set |
| `POST /v1/logs/search` | closed JSON body: `source`, `from/to`, `text`, typed `include/exclude`, `dedup`, `bucket_us`, `limit`, optional `cursor` | text не более 1024 UTF-8 bytes; до 32 facets, 256 buckets и 200 rows; unknown body field запрещён |

Имена `source`, `scope`, `database_episode_id`, `evaluation_id`,
`evidence_set_id`, `a_from`, `a_to`, `b_from`, `b_to`, `a_at`, `b_at`, `metric`,
`sort`, `bucket_us`, `text`, `include`, `exclude` и `dedup` добавляются в
closed `parameter` registry. `expected` дополняется
`u64_decimal`, `i64_decimal`, `b64url_sha256`, `opaque_cursor`, `scope`, `unix_micros`,
`metric`, `bounded_text`, `facet_list` и `dedup_mode`; `constraint` —
`database_required_for_scope`, `database_forbidden_for_instance_scope`,
`a_from_before_a_to`, `b_from_before_b_to`, `windows_within_limit` и
`moment_in_fact_set`; `resource` — `evidence_rows`, `critical_findings`,
`degradations`, `compare_entities`, `settings_rows`, `histogram_buckets`,
`facets`, `scan_segments`, `scan_rows`, `scan_bytes` и `object_rows`.
Изменения вносятся атомарно в Rust/OpenAPI/golden registries. Действующее
значение `query` остаётся местом malformed whole query и не переиспользуется
для log text.

Обязательные closed enums:

- category: `connections`, `performance`, `storage`, `replication`,
  `maintenance`, `mvcc_horizon`, `wal_checkpoints`, `locks`;
- category status: `available`, `unavailable`, `not_applicable`;
- response status: `complete`, `partial`, `unavailable`;
- state: `unknown`, `normal`, `degraded`, `critical`;
- finding class: `critical_policy_breach`, `degraded_policy_breach`;
- query compare metric: `calls_delta`, `rows_delta`,
  `exec_ms_per_call_delta`, `total_exec_time_delta`,
  `shared_read_blocks_delta`, `temp_written_blocks_delta`,
  `wal_bytes_delta`;
- log dedup: `none`, `event_identity`;
- collection failure class: `dns`, `authentication`, `connect_timeout`,
  `database_missing`, `server_rejected`, `other_bounded`;
- threshold provenance: `owner`, `postgresql_setting`, `initial_default`;
- evidence kind: `gauge`, `ratio`, `counter_delta`, `state`, `catalog_fact`;
- source total quality: `exact`, `lower_bound`, `unknown`.

Дополнения к действующему closed reason registry:

| `kind` | Закрытые `params` |
| --- | --- |
| `no_replicas` | `{}` |
| `standby_role` | `{}` |
| `scope_mismatch` | `required_scope`, `observed_scope` |
| `no_applicable_categories` | `scope` |
| `required_input_not_collected` | `input_id` |
| `privilege_denied` | `input_id` |
| `unsupported_server_major` | `input_id`, `server_major` |
| `source_partial` | `source_type_id`, `collected`, optional `source_total` |
| `coverage_gap` | `gap_count` |
| `invalid_interval` | `input_id` |
| `insufficient_samples` | `input_id`, `required`, `observed` |
| `zero_denominator` | `input_id` |
| `bounded_subset` | `input_id`, `returned`, optional `source_total` |
| `cycle_budget_deferred` | `source_type_id` |
| `pool_limit` | `limit` |
| `connect_failed` | `database_episode_id`, `failure_class` |
| `query_timeout` | `source_type_id`, `timeout_ms` |
| `excluded_by_owner` | `policy_id` |
| `unknown_version_value` | `input_id`, `server_major` |
| `redaction_policy` | `revision` |
| `incomplete_operand` | `side` |

Typed diff не дублируется reasons: `reset`, `gap`, `first_point`, `anomaly` и
`not_collected` остаются значениями существующего bounded `nodata` enum.
Существующий `scan_budget {required, available}` переиспользуется без второго
определения.

Новый reason добавляется одновременно в Rust registry, OpenAPI, golden
contract fixtures и оба UI-каталога. Params не содержат SQL, log text, object
definitions, filesystem paths, error chains или неограниченные коллекции.
Problem responses сохраняют существующие пять полей и отдельный closed
problem-code registry.

OpenAPI описывает reason как generator-compatible `oneOf`: каждая ветвь имеет
`additionalProperties=false`, `kind: const` и собственную exact params schema.
Finding/degradation params задаются аналогичным `oneOf` по `rule_id: const`;
`params: object` и свободные maps запрещены. Generated client получает
discriminated unions, а не теряет тип параметров.

UI требования:

- EN и RU полностью покрывают product-owned IDs; отсутствие key — CI error;
- critical/degradation и partial coverage выражены текстом и структурой, не
  только цветом;
- keyboard navigation, видимый focus, focus transfer в drilldown, contrast,
  screen-reader labels и table equivalent для графиков;
- literal PostgreSQL values не переводятся и не используются как шаблоны;
- locale не влияет на machine sort, decimal value, timestamp или cursor.

## 7. Safety, performance и collection contract

### 7.1. Поддерживаемые версии и privileges

Новые production paths поддерживают PostgreSQL 15, 16, 17 и 18. Major
определяет query/layout adapter; «один запрос с COALESCE для всех версий» не
заменяет BDD. Неизвестный будущий major отключает неподтверждённые поля с typed
reason, а не применяет ближайшую схему.

Каждый source документирует минимальные privileges и видимость:

- собственные sessions против чужих;
- `pg_read_all_stats`/`pg_monitor` либо более узкий grant;
- CONNECT к каждой базе;
- extension execute privilege для on-demand path;
- restricted catalog/source values.

Permission denial может сделать поле NULL, выборку partial или source
unavailable — в зависимости от PostgreSQL view. Эти случаи сохраняются
раздельно. Наличие строк не доказывает full visibility.

### 7.2. Work bounds

До implementation PR для каждого source/endpoint фиксируются:

```text
max_rows
max_response_bytes
max_materialized_bytes
max_query_bytes
max_segments
max_objects
max_work_units
max_wall_time
statement_timeout
lock_timeout
max_concurrency
cursor binding
```

Collector PR переиспользует уже настроенные owner budgets из
`bins/pg_kronika-collector/src/config.rs`: как минимум
`KRONIKA_PG_STATEMENT_TIMEOUT_MS`, `KRONIKA_PG_LOCK_TIMEOUT_MS`,
`KRONIKA_CYCLE_DB_BUDGET_MS` и соответствующие row caps. Новый source может
задать более строгий предел, но не обойти эти ограничения. Web endpoints
проходят существующий admission/scan budget вместо отдельного
неограниченного executor.

Начальные API hard caps:

| Операция | Начальный hard cap |
| --- | --- |
| Health history | существующие 31 сутки и 2000 точек |
| Health evidence | 100 rows, 1 MiB на ответ |
| Query compare | 200 entities; два окна не более 24 h каждое |
| Settings compare | 2000 rows |
| Log search | 200 rows, 256 histogram buckets, 1024 query bytes, 32 facets |
| Object inspector | один base object; отдельные пагинируемые collections |
| On-demand physical scan | один relation, concurrency 1 на target |

Текущие hard caps `UiSummary`/`EntitySeries` и трёх UI-data routes относятся
только к baseline раздела 12. Они не заменяют qualification каждого нового
target path и не доказывают bounds peak memory, concurrency и wall time для
web-index builder, поэтому `SAFE-001` остаётся частично реализованным.

Caps являются initial policy и могут быть ужесточены owner configuration.
Превышение request bound даёт существующий typed Problem; достижение scan/top-N
budget может дать partial success только с exact bounds и reason.

Catalog queries используют exact predicates и server-side typed parameters.
Запрещены unbounded recursive hierarchy, quadratic all-pairs, unrestricted
extension calls и full-catalog response. Expensive source работает в отдельном
scheduler slot; ошибка или timeout не задерживает запись готовых core
sections.

### 7.3. Stored-first

Обычный UI request читает сохранённые historical facts и не выполняет live
diagnostic SQL. Допустимы только явно описанные on-demand operations
object-refresh/physical scan с RBAC, admission и audit. Они асинхронны,
ограничены одним target/object и не входят в score текущего окна до
сохранения результата с provenance.

Health, compare и log APIs никогда не вызывают live PostgreSQL для заполнения
пропуска. Missing historical fact остаётся missing.

### 7.4. Reset, gap, partial и false zero

Для каждого результата допустимы только доказанные состояния:

- observed zero;
- value;
- reset;
- gap;
- not collected;
- partial;
- unavailable;
- not applicable.

Отсутствие section/row/coverage marker не эквивалентно нулю. Пустой результат
становится observed zero только при successful complete attempt с full
visibility и population semantics. Top-N row не даёт отрицательного вывода о
tail.

Cumulative inputs требуют predecessor в том же reset/boot/catalog episode и
используют typed diff. `stats_reset`, postmaster restart, OS boot, GUC gate,
extension version и collection cadence входят в continuity. Gauge не
вычитается.

### 7.5. Catalog change identity

Исторический object key:

```text
node identity
database episode
object class
catalog OID
observation episode
server major
```

Name — label, не identity. `relfilenode` — physical observation, не lifetime
key. Rename не создаёт новый object episode; доказанный drop/recreate создаёт.
OID reuse после observed absence не склеивается. Gap между присутствием до и
после даёт uncertain continuity. Rewrite хранится как изменение physical
identity. Partition child/parent и constraint/index ownership имеют собственные
episode refs.

### 7.6. Privacy и redaction

Query, plan, log text, object definitions, paths, usernames и application
labels могут содержать чувствительные значения.

- Listing/score/history используют opaque IDs и bounded typed measurements.
- Literal detail загружается лениво, permission-aware и с
  `available|redacted|truncated|privilege_denied` status.
- Redaction revision входит в cursor/evaluation/export context.
- Literal values не попадают в Problem params, cursor payload, request ID,
  metrics labels или backend-generated prose.
- Хеш для identity/search не публикуется как способ восстановления текста.
- Default investigation context исключает literals.

Retention и доступ к query/log text следуют общей storage/RBAC policy; новый
source не расширяет их срок хранения неявно.

### 7.7. Deterministic top-N и tail evidence

Каждый bounded список объявляет selection axes, sort direction и stable
tie-break. Ответ содержит:

```text
returned
source_total?
source_total_quality
limit
cutoff?
coverage
tail.status
tail.reason?
```

`source_total_quality=lower_bound|unknown` не отображается как exact percent.
Union нескольких axes хранит membership/axis provenance. Worst claim допустим
только для axis с доказанным coverage. Данные tail не восстанавливаются из
cutoff.

## 8. Verification и traceability

### 8.1. Обязательные уровни тестирования

**Score unit/property tests**

- exact weights, normalization и сумма effective weights;
- любое сочетание `available/unavailable/not_applicable`;
- replication no-replica и maintenance standby;
- `available_weight=0` даёт `null`;
- permutation invariance rules/categories;
- category maximum не уменьшается при усилении factor;
- monotonic penalty functions;
- clamp 0..100, fixed-point arithmetic и округление 0.1;
- critical ceiling `score = min(raw_score, 30.0)` и critical с
  `raw_score=null`;
- checksum rule: co-temporal `data_checksums=on` + Value delta, disabled,
  unknown setting и reset;
- owner/PostgreSQL/default provenance precedence;
- completeness и partial не становятся complete после redistribution.

**Golden machine-contract tests**

- exact JSON keys/enums/closed params;
- Rust registry ↔ OpenAPI;
- policy/rule/formula/evaluation IDs;
- отсутствие product prose и literal leakage;
- `Accept-Language` byte-equivalent semantics;
- cursor binding и redaction revision.

**Collector/source tests**

- query selection по major;
- rows/bytes/time/work limits и stable order;
- attempt/population coverage для empty, full, partial и failure;
- per-database pool state;
- privilege and timeout isolation;
- codec golden/backward read.

**Production-path BDD PostgreSQL 15–18**

Каждый новый stored source проходит настоящую запись, seal/read и API
projection на всех четырёх major. Обязательные сценарии:

- reset, gap, not collected и first point;
- checksums enabled/disabled/unknown и reset-aware failure delta;
- primary/standby;
- no replicas;
- privilege denied/restricted visibility;
- partial population и unknown total;
- top-N cutoff/tail;
- empty complete sample;
- drop/recreate/OID reuse и major upgrade boundary;
- version-specific columns/formulas.

Unit fixture без production collection/read path не закрывает BDD gate.

**API/UI**

- per-database isolation, point/detail consistency и bounded pagination;
- query/settings/log compare partial states;
- EN/RU catalog completeness;
- keyboard/focus/contrast/screen-reader/table equivalent;
- one-IANA-timezone and DST;
- URL encode/decode/reload/back-forward roundtrip;
- investigation/Grafana range clipboard;
- critical/degradation/completeness всегда видимы.

**Resource qualification**

- wall time, scanned segments/rows/bytes, peak RSS и response bytes;
- concurrent admission, cancel и circuit breaker;
- large catalogs/partition trees;
- two-window compare worst case;
- log histogram/search worst range;
- optional physical scan load guard.

Benchmark artifact фиксирует dataset, PG major, limits, warm/cold conditions и
exact head. Нельзя утверждать production bound только по среднему времени.

### 8.2. Traceability matrix

| ID | Статус main | Остаток | Target contract | Acceptance | Order |
| --- | --- | --- | --- | --- | --- |
| `HS-001` kernel | Частично | Additive formula, target scale/categories и real extractors | Additive 0–100, восемь weights, max rule penalty, fixed-point | Unit/property formula suite | A |
| `HS-002` availability | Частично | Category availability, completeness и universal persisted source coverage | weight=0 для unavailable/N/A, redistribution, typed reason, completeness | All state combinations, no false zero | A + C coverage |
| `HS-003` ceiling | Частично | Шесть catastrophic rules, separate findings и ceiling 30 | Fact/policy provenance и critical ceiling | Golden critical fixtures, null-score case | A; sequence rule activates in C |
| `HS-004` history | Частично | Target `/v1/health/{score,history,databases,evidence}` services; generic view summary/heatmap остаются baseline | Score/category history, per-db, evidence refs | Point/detail identity, database isolation | B |
| `DATA-001` diff | Частично | Подключить все factors к canonical typed window path | Только window diff для cumulative inputs | Reset/gap/first/not-collected matrix | A |
| `DATA-002` coverage | Частично | Universal persisted attempt/population/database outcomes; UI-index quality не заменяет collector coverage | Universal attempt/population/database markers | Empty/full/partial/failure BDD | C foundation |
| `DATA-003` autovacuum | Частично | Полный effective reloptions/eligibility/backlog/off inventory | PG15–18 reloption-aware vacuum/analyze contracts | Major BDD и exact formula golden | C |
| `DATA-004` sequence | Будущее | Весь sequence source | Direction/cache/cycle/ownership/type bounds | Arithmetic property + PG15–18 BDD | C |
| `DATA-005` horizon | Частично | Total/tail, global clamp и полный drilldown | Расширенная coverage существующей model | DB/table/TOAST/pool BDD | C |
| `DATA-006` worst tables | Частично | Guaranteed full-or-explicit-partial dead/HOT/newpage axes; текущий tables heatmap ранжирует уже retained rows | Complete-or-partial boundary population, typed interval diff и deterministic axes | Outside-old-axis/reset/zero-denominator fixtures | C |
| `DATA-007` schema | Частично | Full structural catalog/history и episodes | Index/FK/constraint fingerprints и episodes | Structural golden + major BDD | C |
| `DATA-008` progress | Частично | Create-index/analyze/cluster/basebackup sources | Четыре versioned stored sources | Phase/visibility/PID/gap BDD | D |
| `DATA-009` inspector | Частично | Bounded exact-object projection и async refresh; UI projection catalog описывает views, не PostgreSQL objects | Stored-first exact-object inspector | Caps, pagination, RBAC, recreate | D |
| `DATA-010` physical | Будущее | Весь optional physical evidence path | Explicit optional `pgstattuple_approx` | Present/absent/privilege/load BDD | D |
| `UX-001` query A/B | Частично | Arbitrary A/B windows, matching, domain API/cursor/UI; single-range heatmap остаётся baseline | Two stored windows, typed operands, plan/buffer evidence | Diff/privacy/cursor/resource suite | E |
| `UX-002` settings | Частично | Exact-moment A/B resolver, change states и UI | Exact stored moments and change enum | Segment/gap/extension fixtures | E |
| `UX-003` schema history | Частично | Structural finding episodes и workflow | Open/close/reopen/uncertain findings | Rename/recreate/upgrade fixtures | C API |
| `UX-004` state | Частично | Browser URL/timezone/context; HTTP query params не являются client state | Full URL state, one IANA timezone, bounded context | Roundtrip/DST/clipboard/accessibility | B/E |
| `UX-005` client | Будущее | Весь generated frontend client/models/build и production frontend; server-side Rust UI data/OpenAPI остаются baseline | Generated client/models only | Generation clean + registry parity | B |
| `UX-006` logs | Частично | Search body/facets/histogram/dedup/scanned и search-specific cursor; events heatmap не является search | Histogram/zoom/facets/dedup/cursor/partial/scanned | Same fact set, budget and redaction | E |
| `EXT-001` external | Частично | Scoped RBAC, audit, isolation и будущие service surfaces | Same bounded services, typed provenance, read-only | RBAC/audit/isolation/parity | F |
| `SAFE-001` bounds | Частично | Qualification каждого нового source/endpoint и peak-memory/admission/wall-time web-index builder; текущие format/read/request/response caps остаются baseline | Rows/bytes/time/work/concurrency everywhere | Resource qualification | all |
| `SAFE-002` identity | Частично | Persisted database/object episodes и lifecycle semantics | Observation episodes and uncertain gaps | Lifecycle property/BDD | C |
| `SAFE-003` privacy | Частично | Lazy literals, redaction revision, opaque listings/exports | Lazy literals, redaction revision, opaque IDs | Leakage/security fixtures | B–F |

Traceability row считается закрытой только ссылками на implementation commit,
tests и qualification artifact. Статус документа сам по себе не является
реализацией.

## 9. Tranches и границы implementation PR

### A. Score kernel на существующих stored data

Отдельные PR:

1. `health_score_v1` domain types, policy/rule registry и версия 2;
2. canonical extractors для существующих inputs и typed coverage adapters;
3. target formula/ceiling и property/golden qualification.

Gate: формула, redistribution, typed unavailability, source window,
raw evidence refs, critical ceiling и no-false-zero проходят тесты. API/UI и
новые collectors не входят.

### B. API, history, drilldown и UI

Зависит от A. Отдельные PR:

1. score/detail/history/per-database/evidence services и OpenAPI;
2. generated client/models;
3. EN/RU UI, accessibility, URL/IANA timezone и investigation context.

Gate: machine contract neutral, critical/degradation/partial видимы, exact
history/detail identity, browser resource bounds. Score остаётся честно
partial до tranche C.

### C. Coverage и catalog sources

Зависит от стабильных IDs A/B. Порядок отдельных PR:

1. universal collection attempt, per-database и population coverage;
2. reloption-aware autovacuum/autoanalyze;
3. sequence/identity exhaustion;
4. horizon coverage и worst dead/HOT/newpage axes;
5. structural schema catalog;
6. finding history и API projections.

Каждый новый logical type имеет собственный registry/docs/codec/collector
commit set и PG15–18 BDD gate. Большой combined collector PR не допускается.

### D. Progress, inspector и physical evidence

Зависит от catalog identity C. Порядок:

1. четыре новых progress stored types (create-index, analyze, cluster,
   basebackup);
2. stored-first object inspector;
3. asynchronous bounded refresh;
4. optional `pgstattuple_approx`.

Gate: RBAC, admission, cancel, load guard, no arbitrary live query и resource
qualification.

### E. Compare и log UX

Query A/B и settings compare зависят от typed diff/coverage A–C. Log UX может
разрабатываться параллельно, но публикуется с теми же cursor/privacy/budget
правилами и переиспользует authenticated timeline cursor primitive. Отдельные
PR для compare API, log search API и UI.

Gate: две stored windows, plan/buffer provenance, all nodata states,
histogram/rows fact-set identity, partial/scanned, URL roundtrip и privacy.

### F. External read-only investigation

Зависит от стабильных application services A–E. Отдельный PR вводит scoped
RBAC, audit и source/database isolation для существующего read-only HTTP.
Дополнительный transport появляется только при подтверждённой необходимости;
write/remediation methods не входят.

Каждая tranche оставляет отдельные implementation PR и acceptance gates.
Исследовательский PR не содержит попытки реализовать production code.

## 10. Non-goals и отклонённые варианты

- Нет внешнего опционального PostgreSQL snapshot store.
- Нет второго контура метрик как источника истины; PgKronika вычисляет
  историю из собственных сохранённых фактов.
- Нет набора несвязанных generic pages: каждый экран реализует описанный
  investigation workflow и stable URL.
- Нет raw free-form `reason`/`context`, backend English prose или translation
  keys в machine API.
- Нет готовых `ALTER SYSTEM`, terminate, `DROP` и других
  destructive/remediation SQL.
- Нет claim `safe_to_drop` без long-horizon workload, ownership, dependency,
  partition и coverage evidence; даже при таком evidence интерфейс остаётся
  advisory.
- Нет regex index equivalence.
- Нет verdict «missing index» из одного `seq_scan/idx_scan`.
- Нет unbounded catalog scan, hierarchy walk или extension call.
- Нет arbitrary live diagnostic query на каждый UI request.
- Нет предположения, что отсутствие строки равно нулю.
- Нет единственного score как sole product status: рядом обязательны source
  state, completeness, critical findings, degradations и evidence.
- Нет скрытой causal интерпретации query A/B или schema advisory.
- Нет автоматической установки extensions.

## 11. Официальные основания PostgreSQL 15–18

Формулы и catalog semantics сверяются по version-specific официальной
документации:

- Routine vacuum/autovacuum:
  [PG15](https://www.postgresql.org/docs/15/routine-vacuuming.html),
  [PG16](https://www.postgresql.org/docs/16/routine-vacuuming.html),
  [PG17](https://www.postgresql.org/docs/17/routine-vacuuming.html),
  [PG18](https://www.postgresql.org/docs/18/routine-vacuuming.html).
- Table/TOAST storage parameters:
  [PG15](https://www.postgresql.org/docs/15/sql-createtable.html),
  [PG16](https://www.postgresql.org/docs/16/sql-createtable.html),
  [PG17](https://www.postgresql.org/docs/17/sql-createtable.html),
  [PG18](https://www.postgresql.org/docs/18/sql-createtable.html).
- Vacuum/failsafe settings:
  [PG15](https://www.postgresql.org/docs/15/runtime-config-client.html),
  [PG18](https://www.postgresql.org/docs/18/runtime-config-vacuum.html).
- Sequence catalogs and view:
  [PG15 `pg_sequence`](https://www.postgresql.org/docs/15/catalog-pg-sequence.html),
  [PG18 `pg_sequence`](https://www.postgresql.org/docs/18/catalog-pg-sequence.html),
  [PG15 `pg_sequences`](https://www.postgresql.org/docs/15/view-pg-sequences.html),
  [PG18 `pg_sequences`](https://www.postgresql.org/docs/18/view-pg-sequences.html).
- Core catalogs:
  [`pg_class`](https://www.postgresql.org/docs/18/catalog-pg-class.html),
  [`pg_attribute`](https://www.postgresql.org/docs/18/catalog-pg-attribute.html),
  [`pg_index`](https://www.postgresql.org/docs/18/catalog-pg-index.html),
  [`pg_constraint`](https://www.postgresql.org/docs/18/catalog-pg-constraint.html),
  [`pg_depend`](https://www.postgresql.org/docs/18/catalog-pg-depend.html),
  [`pg_inherits`](https://www.postgresql.org/docs/18/catalog-pg-inherits.html),
  [`pg_database`](https://www.postgresql.org/docs/18/catalog-pg-database.html).
- Progress views:
  [PG15](https://www.postgresql.org/docs/15/progress-reporting.html),
  [PG16](https://www.postgresql.org/docs/16/progress-reporting.html),
  [PG17](https://www.postgresql.org/docs/17/progress-reporting.html),
  [PG18](https://www.postgresql.org/docs/18/progress-reporting.html).
- Monitoring visibility and predefined roles:
  [statistics views](https://www.postgresql.org/docs/18/monitoring-stats.html),
  [predefined roles](https://www.postgresql.org/docs/18/predefined-roles.html).
- Optional physical evidence:
  [PG15 `pgstattuple`](https://www.postgresql.org/docs/15/pgstattuple.html),
  [PG18 `pgstattuple`](https://www.postgresql.org/docs/18/pgstattuple.html).

Version-specific links являются основанием SQL/data contracts. Числовые
health weights, product thresholds, penalties, caps и UI decisions остаются
явно versioned политикой PgKronika.

## 12. Реализовано на current main

Baseline ниже объясняет доступные building blocks и не является активной
траншей. Ни одна строка не закрывает целиком соответствующий target ID.

| ID | Возможность | Production evidence | Test/BDD/docs evidence |
| --- | --- | --- | --- |
| `BASE-H01` | Strict factor coverage и health kernel 0–1; event floor; честный numeric `null` при неполном continuous input | `crates/kronika-analytics/src/overview/health.rs`, `crates/kronika-analytics/src/overview/health_line.rs`, `bins/pg_kronika-web/src/overview/health.rs` | unit/property tests в analytics-модулях; `bins/pg_kronika-web/src/tests/overview_timeline.rs` |
| `BASE-H02` | Bounded `/v1/timeline/health` | `bins/pg_kronika-web/src/lib.rs`, `bins/pg_kronika-web/src/overview/handlers.rs` | `bins/pg_kronika-web/src/tests/overview_timeline.rs`, `bins/pg_kronika-web/openapi.json` |
| `BASE-H03` | Typed diff: value/reset/gap/first/anomaly/not-collected | `crates/kronika-analytics/src/diff/pair.rs`, `crates/kronika-reader/src/query/diff.rs`, `crates/kronika-reader/src/query/gating.rs` | `bins/pg_kronika-web/src/tests/version_diff.rs`, `bins/pg_kronika-web/src/tests/anomalies.rs` |
| `BASE-H04` | Частичные collection/snapshot coverage `1_023`/`1_038` | `crates/kronika-registry/src/codec/collection_coverage.rs`, `crates/kronika-registry/src/codec/snapshot_coverage.rs`, `bins/pg_kronika-collector/src/coverage.rs` | `crates/kronika-bdd/features/collection_coverage.feature`, `docs/type-registry/semantics.md` |
| `BASE-H05` | Table/index stats, bounded freeze horizon и vacuum progress | `crates/kronika-source-pg/src/user_tables.rs`, `crates/kronika-source-pg/src/user_indexes.rs`, `crates/kronika-source-pg/src/incident_gauges.rs`, `crates/kronika-source-pg/src/progress_vacuum.rs`, соответствующие codecs и `bins/pg_kronika-collector/src/{pool_sources.rs,main_sources.rs,buffering.rs}` | `crates/kronika-bdd/features/user_tables.feature`, `crates/kronika-bdd/features/pg_stat_progress_vacuum.feature`, `docs/type-registry/postgresql.md` |
| `BASE-H06` | Stored statements/plans/settings и generic section diff | `crates/kronika-source-pg/src/statements.rs`, `crates/kronika-source-pg/src/store_plans.rs`, `crates/kronika-source-pg/src/settings.rs`, `bins/pg_kronika-web/src/handlers/v1.rs` | `crates/kronika-bdd/features/pg_stat_statements.feature`, `crates/kronika-bdd/features/pg_store_plans.feature`, `crates/kronika-bdd/features/pg_settings.feature`, `bins/pg_kronika-web/src/tests/version_diff.rs` |
| `BASE-H07` | Typed logs, fact-set-bound timeline и HMAC-authenticated cursor | `crates/kronika-source-log/src`, `bins/pg_kronika-web/src/overview/cursor.rs`, `bins/pg_kronika-web/src/overview/handlers.rs` | `crates/kronika-bdd/features/pg_log.feature`, `crates/kronika-bdd/features/timeline_overview.feature`, `bins/pg_kronika-web/src/tests/overview_timeline.rs` |
| `BASE-H08` | GET-only machine API, Basic Auth и действующие collector/web limits | `bins/pg_kronika-web/src/lib.rs`, `bins/pg_kronika-web/src/auth.rs`, `bins/pg_kronika-collector/src/config.rs`, `crates/kronika-source-pg/src/pool.rs` | `bins/pg_kronika-web/src/tests/auth_static.rs`, `bins/pg_kronika-web/src/tests/overview_admission.rs`, tests in `bins/pg_kronika-collector/src/config.rs` |
| `BASE-H09` | Server-side UI-data foundation: единый реестр девяти проекций, OVF summary/top-K series, selective reads и hard-capped catalog/summary/heatmap responses | `crates/kronika-analytics/src/web_projection.rs`, `crates/kronika-reader/src/overview/web_index/{build,read,series,summary}.rs`, `bins/pg_kronika-web/src/ui/{catalog,data,handlers,heatmap}.rs` | `crates/kronika-analytics/tests/web_projection.rs`, web-index/snapshot tests в `kronika-reader`, `bins/pg_kronika-web/src/tests/{ui_catalog,ui_data}.rs`, `bins/pg_kronika-web/openapi.json` |

`BASE-H09` не содержит target Health services, production browser frontend,
URL/IANA/context state или generated client.
