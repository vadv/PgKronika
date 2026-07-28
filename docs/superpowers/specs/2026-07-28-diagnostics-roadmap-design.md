# Оставшиеся работы по диагностике PgKronika

Дата: 2026-07-28. Статус: верхнеуровневая дорожная карта для серии
implementation PR. Текущее состояние сверено с `origin/main` на
`0bba2d02901b88792f35b801c2c9cc65bdcf5352`.

## Что ещё предстоит

Работы идут по одному пути данных:

`versioned stored fact → typed coverage/diff → findings и Health Score →
bounded machine API/UI`.

Зависимость сильнее номера tier. Внутри одного dependency band сохраняется
порядок профильной спецификации.

1. **Score foundation (A).** Завершить `HS-001..003`, `DATA-001`,
   `SAFE-001` и `SAFE-003`: добавить additive score 0–100, восемь категорий,
   canonical extractors, перераспределение доступных весов, completeness и
   critical ceiling. Уже работающие strict coverage, typed diff и event floor
   не реализуются повторно.
2. **Machine contract (начало B).** Завершить `HS-004`, `UX-004` и
   `UX-005`: зафиксировать score/detail/history/per-database/evidence
   services, согласовать runtime IDs с OpenAPI, затем сгенерировать client.
3. **Universal coverage.** Завершить `DATA-002`: сохранять attempt,
   population и per-database outcome для каждого используемого PostgreSQL/OS
   source. Все последующие sources используют этот контракт.
4. **Health UI (остаток B).** Поставить EN/RU UI, accessibility, stable URL,
   одну IANA timezone и bounded investigation context. До завершения coverage
   UI явно показывает partial/unavailable.
5. **Core facts.** Выполнить оставшиеся T1 gaps в порядке
   `T1-1 → T1-6 → T1-5 → T1-2 → T1-3 → T1-7 → T1-8 → T1-9`.
   `T1-4` поставляется в band D вместе с остальными progress sources.
6. **Catalog foundation (C).** `DATA-003` → единый sequence source
   `T2-7`/`DATA-004` → `DATA-005` → `DATA-006` → `DATA-007`/`SAFE-002` →
   finding history `UX-003`. Каждый шаг сохраняет population/tail и
   observation episodes.
7. **Progress и object evidence (D).** Единый scope `T1-4`/`DATA-008`,
   включая COPY → `DATA-009` stored-first inspector → асинхронный bounded
   refresh → опциональный `DATA-010`.
8. **Оставшиеся Tier 2 и product actions (E).** Сначала общий inventory
   расширений `T2-10`, затем зависящие от него extension sources; остальные
   T2 идут в локальном порядке каталога. Параллельно после готовности typed
   coverage/identity можно поставлять `UX-001`, `UX-002` и остаток
   `UX-006`.
9. **Tier 3.** Поставлять только после inventory расширений, work bounds и
   production-path fixture. В `T3-3` остаётся только auto_explain-specific
   parsing и хранение: generic multiline continuation и blob storage уже
   являются baseline.
10. **External access (F).** Завершить `EXT-001` поверх стабильных bounded
    services A–E: scoped RBAC, audit, isolation и parity с HTTP. Новый
    transport необязателен, если эти свойства обеспечивает существующий
    read-only HTTP-контракт.

Сводка 50 активных IDs: `implemented=0`, `partial=23`, `future=27`,
`superseded/rejected=0`. Реализованные prerequisites вынесены в нижний
baseline и не считаются активными задачами.

Подробные контракты:

- [`2026-07-27-metrics-gap-design.md`](2026-07-27-metrics-gap-design.md)
  владеет только оставшимися линзами, cadence, source semantics и локальным
  приоритетом T1–T3;
- [`2026-07-28-health-score-diagnostics-design.md`](2026-07-28-health-score-diagnostics-design.md)
  владеет формулой Health Score, envelope новых фактов, catalog identity,
  API/UI, safety и приёмкой A–F.

Эта карта владеет общим порядком, зависимостями и definition of done. Формулы,
схемы строк и полные матрицы приёмки остаются в подробных документах.

## Разрешение пересечений

| Область | Единое решение |
| --- | --- |
| Health Score и каталог линз | Исключение Health Score из scope каталога линз не является запретом продукта. Каталог определяет факты; Health-спецификация определяет их версионированную проекцию. |
| Coverage | Строгий envelope Health-спецификации применяется ко всем новым линзам. Пустой результат означает наблюдаемый ноль только после полного успешного attempt; иначе сохраняется typed `partial`, `not_collected`, `unavailable` или `not_applicable`. |
| Sequence exhaustion | T2-7 и `DATA-004`/`HS-003` закрывает один source/type. T2-7 задаёт место в каталоге и cadence, а раздел 4.2 Health-спецификации — identity, арифметику, privileges, coverage и приёмку. |
| Progress | T1-4 и `DATA-008` образуют одну серию: четыре общих view следуют строгому identity/coverage контракту Health-спецификации, а scope дополняется `pg_stat_progress_copy`. Существующий vacuum source не переделывается без отдельного подтверждённого gap. |
| Index states | Гарантированная operational axis для invalid index допустима как раннее расширение существующей линзы, но не считается полным каталогом. Исторические invalid/not-ready/not-live states, constraints и fingerprints владеет `DATA-007`; параллельная schema model не создаётся. |
| XID/MXID horizon | T1-7 дополняет существующие replication facts и становится входом диагностики. Полноту database/table/TOAST horizon, tail и object episodes владеет `DATA-005`; отдельная horizon model не создаётся. |
| Bloat evidence | T2-8 — дешёвая периодическая SQL-оценка. `DATA-010` — явно запрошенное policy-gated physical evidence для одного объекта. Эти факты не подменяют друг друга. |
| Logs | Новые log lenses расширяют stored evidence. Search/compare UI читает те же факты; общие multiline, redaction, privacy, cursor и scan-budget контракты реализуются один раз. |

Одна реализация закрывает все относящиеся к ней `T*`, `HS-*`, `DATA-*`,
`UX-*`, `SAFE-*` и `EXT-*` IDs. Один logical source/type поставляется одним
PR; общая инфраструктура получает отдельный предшествующий PR.

## Traceability и definition of done

Каждый implementation PR:

1. называет пункт этой карты и все закрываемые IDs подробных спецификаций;
2. связывает ID с implementation commit, тестами и qualification artifact;
3. синхронно обновляет registry/type docs и README владельца контракта;
4. сохраняет typed reset/gap/partial/privilege outcomes, population tail и
   object/reset/boot identity без false zero;
5. фиксирует `rows`, `bytes`, `time`, `work`, `concurrency` и peak-memory
   bounds до материализации данных внешнего размера;
6. для нового stored source проходит unit/golden и production-path BDD на
   PostgreSQL 15–18, включая empty, failure и version-specific cases;
7. для API/UI проходит Rust↔OpenAPI parity, privacy/redaction, cursor/resource
   bounds, EN/RU и accessibility gates;
8. завершает repository gates и exact-head CI без неподтверждённого
   исключения.

Статус traceability row меняется на `verified` только при наличии всех трёх
ссылок: implementation, tests и qualification. Слияние документа само по себе
не закрывает ни один implementation ID.

## Реализовано на current main

Это evidence-backed baseline, а не активный план.

| ID | Возможность | Production evidence | Test/BDD/docs evidence |
| --- | --- | --- | --- |
| `BASE-001` | Strict health/factor kernel, event floors и честный `null` при неполном continuous input | `crates/kronika-analytics/src/overview/health.rs`, `crates/kronika-analytics/src/overview/health_line.rs`, `bins/pg_kronika-web/src/overview/health.rs` | unit/property tests в тех же analytics-модулях; `bins/pg_kronika-web/src/tests/overview_timeline.rs` |
| `BASE-002` | Typed diff с reset/gap/first/not-collected | `crates/kronika-analytics/src/diff/pair.rs`, `crates/kronika-reader/src/query/diff.rs` | `bins/pg_kronika-web/src/tests/version_diff.rs`, `bins/pg_kronika-web/src/tests/anomalies.rs` |
| `BASE-003` | Частичные collection/snapshot coverage и bounded top-N accounting | `crates/kronika-registry/src/codec/collection_coverage.rs`, `crates/kronika-registry/src/codec/snapshot_coverage.rs`, `bins/pg_kronika-collector/src/coverage.rs` | `crates/kronika-bdd/features/collection_coverage.feature`, `docs/type-registry/semantics.md` |
| `BASE-004` | Vacuum progress, включая PG18 `delay_time` | `crates/kronika-source-pg/src/progress_vacuum.rs`, `crates/kronika-registry/src/codec/pg_stat_progress_vacuum.rs`, `bins/pg_kronika-collector/src/main_sources.rs` | `crates/kronika-bdd/features/pg_stat_progress_vacuum.feature`, `docs/type-registry/postgresql.md` |
| `BASE-005` | Bounded GET machine API с Basic Auth и authenticated timeline cursor | `bins/pg_kronika-web/src/lib.rs`, `bins/pg_kronika-web/src/auth.rs`, `bins/pg_kronika-web/src/overview/cursor.rs` | `bins/pg_kronika-web/src/tests/overview_timeline.rs`, `bins/pg_kronika-web/src/tests/overview_admission.rs`, `bins/pg_kronika-web/src/tests/auth_static.rs`, `bins/pg_kronika-web/openapi.json` |
