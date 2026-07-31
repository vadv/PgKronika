# Проектные записи PgKronika

Датированные записи сохраняют решения и историю, но не переопределяют
production-код, тесты и действующую справку. Канонический статус указан ниже;
физическое размещение части завершённых записей временно отстаёт от статуса и
само по себе не возвращает работу в backlog.

| Канонический статус | Смысл | Specs | Plans |
| --- | --- | ---: | ---: |
| Active | `PARTIAL`/`FUTURE`: есть точный незакрытый объём | 18 | 5 |
| Implemented | Полностью реализованные контракты и планы, включая ещё не перемещённые записи | 13 | 23 |
| [`archive/specs/`](archive/specs/), [`archive/plans/`](archive/plans/) | Заменённые или отклонённые варианты | 3 | 1 |

В количества входят только датированные документы, без этого README.
Статус production baseline сверен с кодом на
[`0fbc3dff79a12496d9c1ca16f7208a96da880fa3`](https://github.com/vadv/PgKronika/commit/0fbc3dff79a12496d9c1ca16f7208a96da880fa3)
(2026-07-31); новый remaining
contract и plan добавлены поверх этого SHA. На base было 17 active specs и
4 active plans; эта правка сохраняет v6 design `PARTIAL`, консолидирует его
actionable checklist в новом remaining contract и добавляет новый plan, поэтому
итог — 18/5. Перемещение файлов не входит в эту правку.

## Реализованный и консолидированный web baseline

| Документы, физически оставшиеся в active-каталогах | Канонический статус | Основание |
| --- | --- | --- |
| [`OpenAPI frontend codegen`](specs/2026-07-31-openapi-frontend-codegen-design.md) | `IMPLEMENTED` | [PR #150](https://github.com/vadv/PgKronika/pull/150): generated client, Spectral/freshness checks. |
| [`manual demo smoke`](plans/2026-07-29-manual-demo-api-smoke-workflow-implementation.md), [`OpenAPI Swagger`](plans/2026-07-29-openapi-swagger-implementation.md), [`typed multifile OpenAPI`](plans/2026-07-29-typed-multifile-openapi-implementation.md), [`numeric thresholds`](plans/2026-07-30-complete-numeric-threshold-catalog.md), [`section coverage`](plans/2026-07-30-section-collection-coverage.md), [`threshold frame`](plans/2026-07-30-threshold-frame-integration.md), [`typed threshold catalog`](plans/2026-07-30-typed-threshold-catalog-implementation.md), [`v6 scaffold`](plans/2026-07-30-web-ui-v6-scaffold.md), [`v6 summary/heatmap`](plans/2026-07-31-web-ui-v6-summary-heatmap.md) | `IMPLEMENTED` | Соответствующие production paths и gates присутствуют на проверенном baseline; PR #150 завершил v6/codegen/static tranche. |
| [`Web UI v6 design`](specs/2026-07-30-web-ui-v6-design.md) | `PARTIAL`, checklist consolidated | Реализованная часть — baseline PR #150; единственный checklist owner остатка — новый remaining contract ниже. |

## Активные спецификации

| Документ | Статус | Точный остаток |
| --- | --- | --- |
| [`pg_stat_user_tables`](specs/2026-06-29-pg-stat-user-tables-design.md) | `PARTIAL` | V1/V2 codec round-trips; live PostgreSQL 15/16/18 BDD с двумя базами, type IDs и dictionary names. |
| [`pg_locks` wait tree](specs/2026-07-01-pg-locks-wait-tree-design.md) | `PARTIAL` | Независимый от ошибок/лимита activity waiter signal, over-cap regression, live PostgreSQL 15/16/18 BDD; doc-долг: `docs/type-registry/postgresql.md:585` утверждает live-матрицу PG 14-18 (фактически только PG17). |
| [`pg_kronika-web` production readiness](specs/2026-07-12-web-prod-readiness-design.md) | `PARTIAL` | Fail-fast validation неверного `KRONIKA_WEB_LOG` и startup-тест; doc-долг: устаревшее утверждение спеки про реализованный RFC 9457 (ошибки теперь `{code, params}`, #138). |
| [`kronika-diff`](specs/2026-07-14-kronika-diff-design.md) | `PARTIAL` | Передача `SnapshotFull`/`Changed` в fold, `FirstPoint` после исчезновения snapshot row и две регрессии. Крейт слит в `kronika-analytics` (`d302725`), контракт актуален. |
| [Incident lenses](specs/2026-07-16-kronika-incident-lenses-design.md) | `PARTIAL` | 24 strict `EntityJoin`, `track_planning` gate, runtime period/clock-domain provenance, P/I/D-окна, per-entity attribution, route-level PostgreSQL 15-18 BDD и load/RSS qualification. |
| [Incident implementation](specs/2026-07-17-kronika-incident-implementation.md) | `PARTIAL` | I5/§8 второй срез (period/clock, P/I/D, joins, planning, attribution); §9 route-level matrix и load/RSS artifact. Зафиксировано отклонение: handler подставляет `ClockRelation::Simultaneous` без reader-provenance. |
| [Overview index and timeline](specs/2026-07-22-overview-index-timeline-api.md) | `PARTIAL` | §23.1 calibration, §23.2 deployment limits, §23.3 maintenance/topology; §23.4 UI развивается под спекой `web-ui-api`; doc-долг: flat-scan текст (§10.1, §12.6) противоречит production UTC-layout. |
| [PGM size reduction](specs/2026-07-26-pgm-size-reduction-research.md) | `PARTIAL` | Smoke-paths section/diff/anomaly/incident внутри producer→restart→web→OVF сценария (остов уже закрыт `timeline_web_lifecycle` BDD), ext4/XFS crash/fsync/strace, natural-corpus gates. |
| [Segment directory layout](specs/2026-07-26-segment-directory-layout-research.md) | `PARTIAL` | ext4/XFS power-loss evidence, PG15-18 layout/GC/demo матрица, multi-process (частично закрыт lifecycle-BDD), resource matrices 1d…5y; doc-долг: quarantine grammar reconciliation (strict-stop текст против tolerant bounded quarantine в коде). |
| [`pg_kronika-dump`](specs/2026-07-27-dump-design.md) | `PARTIAL` | PG15-18 torn-journal lifecycle: collector readiness, сохранение улики, следующее окно, web refresh с quarantine evidence + quarantine-specific web-тест. |
| [Diagnostics metric gaps](specs/2026-07-27-metrics-gap-design.md) | `PARTIAL` | 26 active IDs; typed deadlock extraction (`T1-5`) и auto_explain (`T3-3`) — partial, остальные открыты. |
| [Retention](specs/2026-07-27-retention-design.md) | `PARTIAL` | Orphan OVF bytes в fixed-mode счётчике, синхронизация текста спеки с реальным hourly rescan, dump whole-tree/statvfs totals, fixed/auto/concurrent-reader live BDD. |
| [Entity-series OVF blocks](specs/2026-07-28-entity-series-block-design.md) | `PARTIAL` | Cross-segment predecessor, multi-input projections (builder гейтует `requires.len() != 1`), per-view `resource_limited` budgets (сейчас лимит валит всю сборку), полная resource/ranking qualification. |
| [Diagnostics roadmap](specs/2026-07-28-diagnostics-roadmap-design.md) | `PARTIAL` | Десять dependency bands целиком: score/services, coverage, frontend, core facts, catalogs, progress/inspector, product actions, Tier 3 и external access. |
| [Health Score diagnostics](specs/2026-07-28-health-score-diagnostics-design.md) | `PARTIAL` | Все 24 target IDs: 0-100 scoring/honesty, Health services, universal coverage, catalog/progress/inspector/compare/settings/log/client work и qualification/RBAC/audit. |
| [Full UI Web API](specs/2026-07-28-web-ui-api-design.md) | `PARTIAL` | `GET /v1/ui/context`, `GET /v1/entity/{view}/{entity}` (point+history), `GET /v1/storage`, три byte-accounted cache (metadata/EntitySeries/PGM projection), N=96/1 440 qualification для оставшихся consumers (frame покрыт). |
| [Web UI v6](specs/2026-07-30-web-ui-v6-design.md) | `PARTIAL` | Архитектурный контекст сохранён; весь actionable remainder и исправления single-root/time/truth semantics принадлежат remaining contract, не отдельному backlog. |
| [Оставшийся контракт Web UI](specs/2026-07-31-web-ui-remaining-contract.md) | `PARTIAL` | P0: EntitySeries truth/qualification → honest browser summary/heatmap → Context+Frame; затем bounded P1 closure и backend-gated P2. |

## Активные планы

| Документ | Статус | Точный остаток |
| --- | --- | --- |
| [Web API BDD](plans/2026-07-10-web-api-bdd.md) | `PARTIAL` | T7 live HTTP matrix: range/order/totals/cursor/batch/gaps и representative PostgreSQL/OS multi-row sections. |
| [Machine API routes](plans/2026-07-21-machine-api-route-contract.md) | `FUTURE` | Единый `/v1` method/path/query manifest, двусторонний Router/OpenAPI contract и resource-specific `Allow`. |
| [Analysis contracts](plans/2026-07-22-analysis-remaining-contracts.md) | `PARTIAL` | Typed identity/relations/coverage; 6 adapter-first, 1 shared-snapshot, 8 partial-input и 9 producer/narrowing требований; diff completeness и browser contract. |
| [Web-index producer and consumers](plans/2026-07-28-web-index-producer-consumers-implementation.md) | `PARTIAL` | Bounded degradation, stable error/OpenAPI contract, real statements producer proof, multi-segment top-K correctness и structural/BDD qualification. |
| [Оставшийся Web UI — план реализации](plans/2026-07-31-web-ui-remaining-implementation.md) | `FUTURE` | Три последовательных P0 implementation PR, bounded P1 follow-ons, exact gates, stop/rollback и P2 entry criteria. |
