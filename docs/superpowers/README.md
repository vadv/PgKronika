# Проектные записи PgKronika

Датированные записи разделены по состоянию работы. Они сохраняют решения и
историю, но не переопределяют production-код, тесты и действующую справку.

| Каталог | Смысл | Specs | Plans |
| --- | --- | ---: | ---: |
| [`specs/`](specs/), [`plans/`](plans/) | Активные `PARTIAL`/`FUTURE`: есть точный незакрытый объём | 17 | 4 |
| [`implemented/specs/`](implemented/specs/), [`implemented/plans/`](implemented/plans/) | Полностью реализованные контракты и планы | 5 | 13 |
| [`archive/specs/`](archive/specs/), [`archive/plans/`](archive/plans/) | Заменённые или отклонённые варианты | 1 | 1 |

В количества входят только датированные документы, без этого README.

## Активные спецификации

| Документ | Статус | Точный остаток |
| --- | --- | --- |
| [`pg_stat_user_tables`](specs/2026-06-29-pg-stat-user-tables-design.md) | `PARTIAL` | V1/V2 codec round-trips; live PostgreSQL 15/16/18 BDD с двумя базами, type IDs и dictionary names; согласование старой PG14 строки с матрицей 15-18. |
| [`pg_locks` wait tree](specs/2026-07-01-pg-locks-wait-tree-design.md) | `PARTIAL` | Независимый от ошибок/лимита activity waiter signal, over-cap regression, live PostgreSQL 15/16/18 BDD и согласование старой PG14 строки. |
| [`pg_kronika-web` production readiness](specs/2026-07-12-web-prod-readiness-design.md) | `PARTIAL` | Fail-fast validation неверного `KRONIKA_WEB_LOG` и startup-тест. |
| [`kronika-diff`](specs/2026-07-14-kronika-diff-design.md) | `PARTIAL` | Передача `SnapshotFull`/`Changed` в fold, `FirstPoint` после исчезновения snapshot row и две регрессии. |
| [Incident lenses](specs/2026-07-16-kronika-incident-lenses-design.md) | `PARTIAL` | 24 strict `EntityJoin`, planning/period/clock/attribution/role inputs, route-level PostgreSQL 15-18 BDD и load/RSS qualification. |
| [Incident implementation](specs/2026-07-17-kronika-incident-implementation.md) | `PARTIAL` | I5/§8 period/clock, P/I/D, joins, planning и attribution; §9 route-level matrix и load/RSS artifact. |
| [Overview index and timeline](specs/2026-07-22-overview-index-timeline-api.md) | `PARTIAL` | Четыре решения §23: calibration, deployment limits, maintenance/topology и UI; согласование flat-scan текста с UTC layout. |
| [PGM size reduction](specs/2026-07-26-pgm-size-reduction-research.md) | `PARTIAL` | Полный PG15-18 producer→restart→consumers→OVF сценарий, ext4/XFS crash/fsync/strace и natural-corpus size/RSS/I/O qualification. |
| [Segment directory layout](specs/2026-07-26-segment-directory-layout-research.md) | `PARTIAL` | ext4/XFS power-loss, PG15-18 layout/GC/demo, multi-process и 1-day…5-year resource matrices; согласование quarantine grammar. |
| [`pg_kronika-dump`](specs/2026-07-27-dump-design.md) | `PARTIAL` | PG15-18 torn-journal lifecycle: collector readiness, сохранение улики, следующее окно и web refresh с quarantine evidence. |
| [Diagnostics metric gaps](specs/2026-07-27-metrics-gap-design.md) | `PARTIAL` | Typed deadlock extraction (`T1-5`), auto_explain parsing/storage (`T3-3`) и остальные 24 active IDs. |
| [Retention](specs/2026-07-27-retention-design.md) | `PARTIAL` | Orphan OVF accounting, честный hourly-rescan контракт, dump FS totals и fixed/auto/concurrent-reader live BDD. |
| [Entity-series OVF blocks](specs/2026-07-28-entity-series-block-design.md) | `PARTIAL` | Cross-segment predecessor, multi-input projections, per-view `resource_limited` budgets и полная resource/ranking qualification. |
| [Diagnostics roadmap](specs/2026-07-28-diagnostics-roadmap-design.md) | `PARTIAL` | Десять dependency bands: score/services, coverage, frontend, core facts, catalogs, progress/inspector, product actions, Tier 3 и external access. |
| [Health Score diagnostics](specs/2026-07-28-health-score-diagnostics-design.md) | `PARTIAL` | 0-100 scoring/honesty, Health services, universal coverage, catalog/progress/inspector/compare/settings/log/client work и qualification/RBAC/audit. |
| [Full UI Web API](specs/2026-07-28-web-ui-api-design.md) | `PARTIAL` | Context/frame/entity/storage routes, predecessor-aware frames, filtering/detail/history, three bounded caches и N=96/1 440 qualification. |
| [Passive instance metadata](specs/2026-07-29-passive-instance-metadata-design.md) | `FUTURE` | Полное удаление node label и всех аналитических зависимостей от `instance_metadata`; секция остаётся только справочной. |

## Активные планы

| Документ | Статус | Точный остаток |
| --- | --- | --- |
| [Web API BDD](plans/2026-07-10-web-api-bdd.md) | `PARTIAL` | T7 live HTTP matrix: range/order/totals/cursor/batch/gaps и representative PostgreSQL/OS multi-row sections. |
| [Machine API routes](plans/2026-07-21-machine-api-route-contract.md) | `FUTURE` | Единый `/v1` method/path/query manifest, двусторонний Router/OpenAPI contract и resource-specific `Allow`. |
| [Analysis contracts](plans/2026-07-22-analysis-remaining-contracts.md) | `PARTIAL` | Typed identity/relations/coverage; 6 adapter-first, 1 shared-snapshot, 8 partial-input и 9 producer/narrowing требований; diff completeness и browser contract. |
| [Web-index producer and consumers](plans/2026-07-28-web-index-producer-consumers-implementation.md) | `PARTIAL` | Bounded degradation, stable error/OpenAPI contract, real statements producer proof, multi-segment top-K correctness и structural/BDD qualification. |
