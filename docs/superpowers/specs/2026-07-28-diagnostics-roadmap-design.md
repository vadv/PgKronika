# Единая программа диагностики PgKronika

Дата: 2026-07-28. Статус: верхнеуровневая дорожная карта для серии
implementation PR. Текущее состояние сверено с `origin/main` на
`0bba2d02901b88792f35b801c2c9cc65bdcf5352`.

## Назначение

Программа развивает один путь данных:

`versioned stored fact → typed coverage/diff → findings и Health Score →
bounded machine API/UI`.

Health Score остаётся производной проекцией сохранённых фактов. Он не заменяет
состояние источников, completeness, critical findings, degradations и raw
evidence. Обычный запрос расследования читает историю и не заполняет пробел
live-запросом к PostgreSQL.

Подробные контракты разделены по одной работе на документ:

- [`2026-07-27-metrics-gap-design.md`](2026-07-27-metrics-gap-design.md)
  владеет каталогом недостающих линз, их cadence, source semantics и локальным
  приоритетом T1–T3;
- [`2026-07-28-health-score-diagnostics-design.md`](2026-07-28-health-score-diagnostics-design.md)
  владеет формулой и версиями Health Score, общим envelope новых фактов,
  catalog identity, API/UI, safety и подробной приёмкой A–F.

Эта дорожная карта владеет только общим порядком, зависимостями, разрешением
пересечений и definition of done. Формулы, схемы строк и полные матрицы
приёмки остаются в подробных документах. При расхождении порядка применяется
эта карта; при расхождении точного контракта применяется профильная
спецификация и текущий код с тестами.

## Проверенная исходная точка

- В коде уже есть строгий health kernel и event-driven health line, но
  обязательный continuous factor не покрыт, поэтому текущая линия намеренно
  возвращает `null`, а не числовую оценку 0–100.
- `snapshot_coverage`, `collection_coverage`, typed diff и bounded top-N
  accounting уже существуют, но attempt/population/per-database coverage пока
  не универсальна.
- Из progress views сохраняется `pg_stat_progress_vacuum`, включая PG18
  adapter; остальные перечисленные progress sources ещё не реализованы.

Следовательно, target score, новые линзы и новые API являются планом, а не
описанием уже работающего поведения.

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

## Единый порядок implementation PR

Зависимость сильнее номера tier. Внутри одного dependency band сохраняется
порядок профильной спецификации.

1. **Score foundation (A).** Domain types, policy/rule registry,
   `health_policy_version=2`, kernel и extractors на существующих stored
   facts; затем property/golden gates. Результат может быть partial, но не
   выдаёт ложный ноль.
2. **Machine-contract foundation (начало B).** Score/history/evidence services,
   stable IDs и Rust/OpenAPI contract. Это фиксирует внешние identities до
   расширения хранения.
3. **Universal coverage (`DATA-002`).** Attempt, population и per-database
   outcomes для empty/full/partial/failure. Все последующие новые sources
   используют этот фундамент.
4. **Завершение B.** Per-database drilldown, generated client, EN/RU UI,
   accessibility, URL/timezone и bounded investigation context. До расширения
   coverage UI явно показывает partial.
5. **Tier 1 без catalog-зависимостей.** T1-1 → T1-6 → T1-5 → T1-2 → T1-3 →
   T1-7 → inode/`relpersistence`/NOTIFY части T1-8 → T1-9. Каждый новый
   source следует universal coverage; расширения существующих sources не
   заявляют полноту сверх сохранённого population marker.
6. **Coverage и catalog sources (остальная C).** Reloption-aware
   autovacuum/autoanalyze → единый sequence source T2-7/`DATA-004` →
   horizon/worst-table coverage → structural schema catalog, включая
   invalid-index scope T1-8 → finding history/API.
7. **Progress и object evidence (D).** Единый progress scope
   T1-4/`DATA-008`, включая COPY → stored-first inspector → asynchronous
   bounded refresh → optional `pgstattuple_approx`.
8. **Оставшийся Tier 2.** Сохраняется локальный порядок каталога; общая
   log-parser/redaction инфраструктура предшествует отдельным log-lens PR.
   Compare и log UX из E могут идти параллельно после готовности их typed
   coverage, identity, privacy и cursor dependencies.
9. **Tier 3.** Только после extension inventory, work bounds и нужной
   production-path test fixture.
10. **External read-only investigation (F).** Только поверх стабильных
    bounded application services A–E, с отдельными RBAC и audit.

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
