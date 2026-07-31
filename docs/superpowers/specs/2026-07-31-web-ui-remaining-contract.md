# Оставшийся контракт Web UI

Дата: 2026-07-31.

**Статус: PARTIAL.**

Контракт сверен с `main` на
[`0fbc3dff79a12496d9c1ca16f7208a96da880fa3`](https://github.com/vadv/PgKronika/commit/0fbc3dff79a12496d9c1ca16f7208a96da880fa3).
Он задаёт только ещё не реализованный web/UI scope и его обязательный порядок.
Исполнимые шаги находятся в
[`2026-07-31-web-ui-remaining-implementation.md`](../plans/2026-07-31-web-ui-remaining-implementation.md).

## Назначение и источники истины

Этот документ отвечает на один вопрос: какое поведение должно быть получено
следующими web/UI-траншами. Он не повторяет wire schema, формат OVF или
инструкции исполнителю.

Детальные входные контракты остаются прежними:

- формат и истинность `EntitySeries` —
  [`2026-07-28-entity-series-block-design.md`](2026-07-28-entity-series-block-design.md);
- catalog, summary, heatmap, frame, context, entity и storage API —
  [`2026-07-28-web-ui-api-design.md`](2026-07-28-web-ui-api-design.md);
- компоновка и взаимодействия UI —
  [`2026-07-30-web-ui-v6-design.md`](2026-07-30-web-ui-v6-design.md);
- timeline и incident semantics —
  [`2026-07-22-overview-index-timeline-api.md`](2026-07-22-overview-index-timeline-api.md),
  [`2026-07-16-kronika-incident-lenses-design.md`](2026-07-16-kronika-incident-lenses-design.md)
  и
  [`2026-07-17-kronika-incident-implementation.md`](2026-07-17-kronika-incident-implementation.md);
- поздние Health-сценарии —
  [`2026-07-28-health-score-diagnostics-design.md`](2026-07-28-health-score-diagnostics-design.md);
- startup validation —
  [`2026-07-12-web-prod-readiness-design.md`](2026-07-12-web-prod-readiness-design.md);
- live HTTP qualification, analysis narrowing и web-index delivery —
  [`2026-07-10-web-api-bdd.md`](../plans/2026-07-10-web-api-bdd.md),
  [`2026-07-22-analysis-remaining-contracts.md`](../plans/2026-07-22-analysis-remaining-contracts.md)
  и
  [`2026-07-28-web-index-producer-consumers-implementation.md`](../plans/2026-07-28-web-index-producer-consumers-implementation.md).

При конфликте старой проектной формулировки с кодом, тестами и более поздним
реализованным контрактом действует порядок из [`docs/README.md`](../../README.md).
Этот документ фиксирует известные разрешения конфликтов ниже.

## Реализованный baseline

На проверенном `main` уже есть React/Vite/strict-TypeScript SPA, catalog-driven
вкладки, минимальные summary и heatmap, EN/RU-каталоги, сгенерированный из
OpenAPI типизированный клиент, воспроизводимый static tarball, cache policy и
frontend/OpenAPI/static CI-гейты. Это baseline
[PR #150](https://github.com/vadv/PgKronika/pull/150), а не будущая работа.

Планы scaffold и summary/heatmap и дизайн OpenAPI-кодогенерации не входят в
очередь повторно. Их наличие не доказывает выполнение оставшихся требований
этого контракта.

## Разрешённые противоречия действующих документов

1. **Runtime single-root.** Параметр и selector `source` не поддерживаются.
   Старые `source` clauses в UI-дизайне и API-примерах не разрешают вернуть
   multi-root. Канонический контракт зафиксирован в
   `2026-07-29-single-root-terminology-cleanup-design.md` и текущих handlers.
2. **Machine API языконейтрален.** Ошибка имеет JSON-форму `{code, params}`.
   Старые ссылки на RFC 9457 и локализованный server prose не являются
   remaining scope.
3. **Время и cursor зависят от endpoint.** Timeline range может достигать
   31 суток; heatmap, frame и incidents ограничены 24 часами; будущая entity
   history — 6 часами. Post-restart `cursor_expired` доказан для timeline
   cursor. Frame cursor самодостаточен и имеет собственные 400/410 semantics.
4. **Retained не означает global.** `frame.matched` точен внутри сохранённой
   projection выбранного snapshot. `ranking.exact` точен внутри сохранённых
   series и не доказывает полноту исходной PostgreSQL population. Collection
   `N/M` и read state показываются отдельно.
5. **Statement text штатно отсутствует.** Collector вызывает
   `pg_stat_statements(false)` и хранит `query = NULL`. Query identity строится
   по typed fields, а не по SQL. Plan text остаётся optional и bounded.
6. **Две оси доступности не смешиваются.** Catalog availability
   (`available`, `gated`, `not_collected`, `unsupported_type`) и literal
   disclosure (`available`, `redacted`, `truncated`, `privilege_denied`,
   `not_collected`) имеют разные причины и должны быть согласованы отдельным
   composition contract до реализации detail.
7. **Continuation и share state различаются.** Transport cursor живёт только
   в in-memory query state. URL/history/share сохраняют durable absolute
   range, whitelisted typed facets, sort и focus. Свободный `q` остаётся
   transient и исключается из address/history/share; при 410 UI сбрасывает
   continuation, запрашивает первую страницу того же intent и явно сообщает
   об этом.

## Обязательный порядок

| Порядок | Транш | Приоритет | Размер | Условие старта |
| ---: | --- | --- | --- | --- |
| 1 | Истинный `EntitySeries` и qualification | P0 | L | текущий projection registry и OVF baseline |
| 2 | Честное browser state и truthful summary/heatmap | P0 | M | транш 1 принят |
| 3 | Context API + catalog-driven Frame/TableView | P0 | L | транш 2 принят |
| 4 | Entity/history/storage privacy-first | P1 | L | context/frame стабилен |
| 5 | Timeline/cursor/incidents | P1 | L | единая time model из транша 2 |
| 6 | Интегрированная EN/RU/a11y/mobile/browser qualification | P1 | M | выполняется внутри 2–5, затем закрывается общей матрицей |
| 7 | OpenAPI/runtime bounds и `KRONIKA_WEB_LOG` | P1 | S | может идти после 2, не меняя product scope |
| 8 | Live HTTP BDD T7 | P1 | M | существующий collector→reader→HTTP harness |
| 9 | Mapping/narrowing Overview/Health/Index | P1 | S | IA траншей 2–5 определена |
| 10 | Health/Compare/Settings/Log | P2 | L на journey | только после соответствующего bounded backend contract |

Транши 1–3 являются отдельными цельными implementation PR. Их нельзя
разбивать на PR по одному component, endpoint или test fixture: внутри каждого
транша format/API/UI/tests/qualification меняются как один проверяемый
контракт.

## Общие инварианты

- Missing, observed zero, gap, gated, unavailable revision, resource limit и
  active tail не взаимозаменяемы.
- Client не вычисляет формулы, collection completeness, joins, severity или
  причинность. Он отображает typed server facts и их provenance.
- Machine values, timestamps, sort keys, cursors, units и error codes не
  зависят от locale. Переводы и `Intl`-форматирование принадлежат UI.
- Sensitive/heavy literals — SQL, plan/log text, definitions, paths, secrets и
  secret-bearing filters — не входят во frame, canonical share URL, cursor,
  error params, request IDs или metric labels. Bounded identity/display labels
  допустимы в listing только verbatim, с честными visibility/provenance и без
  превращения текста в identity.
- Каждый reader/writer/cache/builder path имеет byte-accounted bound до
  allocation или materialization. Превышение даёт typed degradation, а не OOM
  и не молчаливое усечение.
- Все визуальные выводы имеют текстовый или табличный эквивалент и не зависят
  только от цвета.
- `/readyz` означает готовность reader/process, а не свежесть collector data.
  Свежесть определяется factual timeline timestamp и quality.
- Optional Basic Auth остаётся transport boundary: UI не хранит credentials,
  не добавляет login/logout/session и не включает auth material в URL, cache
  key или share. TLS deployment requirement остаётся у production-readiness
  contract.

## Транш 1. Истинный `EntitySeries` и qualification

**User journey.** Оператор открывает сутки, пересекающие несколько sealed
segments. Допустимая counter delta на границе учитывается ровно один раз, gap
остаётся разрывом, reset не становится отрицательной нагрузкой, а Activity
CPU/I/O появляется только для доказанной backend identity.

**Текущее доказательство разрыва.** В
`crates/kronika-reader/src/overview/web_index/build.rs` функция
`evaluate_deltas` начинает каждую группу с `previous = None`, а
`build_view_series` гейтует metric при `requires.len() != 1` и публикует
`IndexStatus::Complete`. `ACTIVITY_METRICS` в
`crates/kronika-analytics/src/web_projection.rs` требует `activity + process`
для CPU/I/O; `activity_view` в `bins/pg_kronika-web/src/ui/catalog.rs`
объявляет join `(pid, backend_start=starttime)`.

**Scope.**

- cross-segment predecessor для normal/reset/gap/absent;
- запрет bridge через доказанный gap и запрет delta при неизвестном reset;
- точный temporal join Activity↔process по typed identity и provenance;
- per-view/per-metric `resource_limited` вместо whole-build abort;
- точная revision coupling policy: `metric_revision` меняется при formula,
  reset, aggregation или unit semantics; `view_revision` — при identity, join
  или required inputs; `block_revision` — только при несовместимом wire layout;
- согласованное изменение writer, reader и API presence semantics;
- correctness, size, RSS, reads, decompressions и ranking evidence для
  N=96 и N=1440.

**Границы.** Транш не материализует все сущности суток, не обещает global
daily top и не исправляет данные на клиенте. Несохранённая пара остаётся
missing, а не zero.

**Ресурсы и privacy.** Сохраняются bounds основной спеки: 64 МиБ decoded
source rows, 32 МиБ дополнительной памяти builder одного view, не более
100 МиБ общего дополнительного peak, top-K 64, identity 256 bytes и label
160 bytes; stored и decoded `EntitySeries` одного view не превышают 256 КиБ.
Join не использует свободный текст, PID без start identity или query text.

**Деградация и наблюдаемость.** Ограничение одной metric/view не запрещает
публикацию остальных. Qualification публикует block/view bytes, writer/reader
peak RSS, positional reads, decompressions и ranking error. Нельзя выставить
`exact=true`, если верхняя граница кандидата не доказана.

**Acceptance.** Golden/property tests покрывают normal/reset/gap/no
predecessor, churn/end leader, invisible candidate и
`lower <= truth <= upper`; sealed и active paths дают одинаковый результат на
одном logical input. Activity positive/negative fixtures доказывают
same-snapshot/co-temporal join по полной typed identity; duplicate, missing,
nearest-time и PID-only кандидаты остаются gated. All-cap fixtures оставляют
соседние views доступными; N=96/1440 artifacts проходят заявленные
memory/I/O/size bounds. Старые wrongly-complete counter metrics становятся
`unavailable_revision` через `metric_revision` bump; background rebuild и
migration не входят в PR 1. `view_revision` меняется только при изменении
identity, join или required inputs.
Size evidence отдельно покрывает 96×15m, 1440×1m, size-seal production fixture
и максимальный statements union top-K. Отчёт проверяет targets: median stored
selected view ≤24 КиБ, все web-блоки ≤10% PGM на production fixture и суточные
web-блоки ≤10 МиБ при штатном seal. Эти targets не применяются как hard gate к
adversarial 1440×1m; target miss требует сокращения labels/materialized metrics
либо остановки, но не ослабления hard caps, K или `exact_score`.

**Stop/rollback.** Работа останавливается до публикации, если хотя бы один
memory cap не резервируется заранее, revision не меняется вместе с семантикой
или boundary truth нельзя доказать fixture. Writer и reader поставляются
вместе. Rollback использует предыдущий binary с совместимыми независимыми
block/view/metric revisions; недоказанное сочетание revision не читается.

## Транш 2. Честное browser state и truthful summary/heatmap

**User journey.** Пользователь открывает deep-link на конкретный replay,
меняет metric и baseline, проходит Back/Forward и получает тот же absolute
range. Summary и heatmap относятся к одному `at/span`; UI отдельно показывает
source coverage, ranking bounds и причины отсутствия данных.

**Текущее доказательство разрыва.** `web/src/App.tsx` читает hash один раз,
фиксирует range от `Date.now()`, передаёт `state.at` только summary и оставляет
`onSelectEntity` пустым. `web/src/state/url.ts` хранит только
`source/view/at`. `TabBar` не передаёт `summary.collection`, а
`HeatmapStrip` не отображает unit, score bounds, `ranking.exact`,
`unseen_upper` и отдельные quality axes. `useCatalog` имеет
`staleTime: Infinity`; общий `apiGet` не обрабатывает 304 как cache hit.
`useSummary` и `useHeatmap` преобразуют decimal timestamps через `Number`.

**Scope.**

- удалить fake `source`; ввести precision-safe canonical codec
  `at/span/baseline/live/view/metric` и navigation subscription;
- вывести `summary.at_us` и heatmap range из одного state;
- реализовать heatmap baseline вторым independently cached запросом и merge
  только по opaque `entity`, совпадающим view/metric revisions, `bucket_count`,
  bucket width/span и relative bucket-index alignment; absolute `from_us/to_us`
  различаются на baseline offset;
- показать mechanical baseline why: current, baseline, delta, unit и обе
  quality/provenance стороны, без client classification; сохранить стабильную
  legend/scale domain;
- показывать collection `N/M`, `read_state`, `visibility`, exact/approx,
  `unseen_upper`, gap/null/gated/unavailable/resource-limited/active-tail и
  observed zero как разные состояния;
- использовать row unit/score bounds и общие EN/RU `Intl` formatters;
- добавить localized loading/error/empty/degraded states, keyboard navigation,
  focus management, screen-reader/table equivalent и mobile triage;
- реализовать cache-aware catalog 200/304 и реальный refresh trigger;
- добавить smoke против packaged `pg_kronika-web`, а не только source-tree
  component tests.

**Границы.** Table baseline-Δ не входит: пагинация не сохраняет один entity
set и требует отдельной API amendment. Новые production locales, client-side
joins и client-side full search не входят.

**Data/API/format.** Timestamp query сериализуется decimal string, внутри
browser arithmetic использует `BigInt`; уменьшение до milliseconds допустимо
только на display boundary. Baseline delta существует лишь при совпавших
revisions, `bucket_count`, bucket width/span и relative index alignment;
absolute ranges ожидаемо сдвинуты. Missing с любой стороны означает «нет
delta», не zero.
Текущие `HeatmapResponse`/`HeatmapRow` в
`bins/pg_kronika-web/src/ui/heatmap.rs` не раскрывают view/metric revisions,
поэтому транш добавляет typed `view_revision` и `metric_revision` в Rust DTO,
OpenAPI и generated client и включает их в cache/merge identity.
Каждый quality fact показывается на своей server-defined гранулярности:
response-level gap/gate не приписывается отдельной null cell. Если различие
cell-level причин требуется для acceptance, транш добавляет minimal typed
missing-reason field в Rust DTO/OpenAPI/generated client; UI его не выводит из
соседних данных.

Machine API остаётся locale-neutral. `summary.collection` описывает point
coverage выбранного snapshot, а не range-wide coverage heatmap.
`ranking.exact`/`unseen_upper` доказывают порядок только retained series, не
полноту несохранённых PostgreSQL rows. Catalog refresh должен увидеть
`gated → available` в той же SPA session. Copy/share из live режима фиксирует
absolute `at/range`, чтобы ссылка воспроизводила экран.

**Ресурсы и privacy.** UI поддерживает hard maximum 64 × 256 heatmap cells без
неограниченного DOM/state growth. Canonical share исключает credentials,
continuation cursor, query/plan/log text и sensitive literal filter.

**Деградация и наблюдаемость.** Stable `{code, params}` получает конкретное
EN/RU presentation; unknown code имеет безопасный fallback. Missing/null reason
не превращается в zero. Browser smoke сохраняет response/status evidence для
normal, partial, gap и down fixtures.

**Acceptance.** Codec property tests покрывают reload/Back/Forward, invalid
input, DST и precision boundary. Shift+click воспроизводит baseline-Δ для того
же entity/revision и совместимой relative grid со сдвинутым absolute range, а
mechanical why показывает оба операнда и quality. Catalog conditional 200/304
и refresh доказаны. Обе локали,
темы, keyboard-only path, accessible equivalent и viewport `<760px` проходят
component/browser tests на packaged binary.

**Stop/rollback.** Транш не принимается, если summary и heatmap могут показать
разные времена, 304 превращается в error, quality state теряется или browser
fixture не воспроизводит deep-link. Rollback возвращает предыдущий embedded
asset bundle; backend wire contract не расширяется несовместимо.

## Транш 3. Context API и catalog-driven Frame/TableView

**User journey.** Из heatmap пользователь выбирает snapshot/entity, получает
server-sorted frame, фильтрует `q`, листает страницы, видит verdict/why,
neighbors и factual database/role/replication context, затем возвращается к
предыдущему URL state.

**Текущее доказательство разрыва.** `api_docs::configured` в
`bins/pg_kronika-web/src/api_docs.rs` регистрирует frame, но не
`/v1/ui/context`. Тесты `ui_frame` доказывают девять views и отсутствие lazy
cells. Во frontend нет frame hook/TableView/Toolbar; entity selection остаётся
no-op. `frame::query` считает `matched` внутри сохранённых rows, а
`FrameQuality` не несёт collection provenance.

**Scope.**

- bounded `GET /v1/ui/context?at=<decimal i64>` без `source`, с полным logical
  database list независимо от active view;
- generated client aliases/hooks для context и frame;
- один TableView для девяти catalog views, columns/presets только из catalog;
- server `q/sort/page`, `matched`, neighbors и verdict/why;
- URL state и endpoint-specific 400/410 recovery;
- summary `N/M` рядом с retained `matched`.

**Границы.** Нет client filtering загруженной страницы, table baseline-Δ,
lazy query/plan text, догадок о DB list из текущей страницы или формул,
зашитых отдельно от catalog.

**Ресурсы и privacy.** Сохраняются frame hard maximum 1 МиБ, server bounds на
filter/limit/cursor, максимум один точный PGM и второй predecessor PGM.
До handler код contract фиксирует для context reservation и hard maximum
512 КиБ encoded response. В пределах cap список полный; превышение даёт typed
`response_too_large`, UI показывает context unavailable и не получает
усечённый список. Partial context требует отдельного deterministic pagination
и omitted-count amendment.
Listing не содержит sensitive/heavy literals; bounded verbatim identity и
display labels разрешены контрактом frame. Свободный `q` не входит в sanitized
share по умолчанию.

**Деградация и наблюдаемость.** `matched` подписан как retained-snapshot count,
а collection completeness берётся из summary. Malformed/query-mismatched
cursor и unavailable snapshot имеют разные presentation/recovery. Retry
сохраняет canonical range/filter/sort и сбрасывает только continuation.
Context qualification фиксирует row count, encoded bytes и cap outcome без
database names.

**Acceptance.** Все девять views проходят один TableView test matrix; context
возвращает полный database list в пределах 512 КиБ и явный
`response_too_large` без rows при превышении; server ordering/filtering/two-page
continuation не даёт дублей; lazy fields отсутствуют в frame и search;
Back/Forward и 400/410 recovery проверены packaged-browser fixture.

**Stop/rollback.** Нельзя выпускать TableView, если client вычисляет `matched`,
context зависит от rows страницы или continuation silently меняет snapshot.
Endpoint additive; rollback скрывает новый surface и возвращает прежний UI,
не меняя stored format.

## Общий delivery contract для P1

Каждый P1-транш наследует следующие требования; его раздел ниже задаёт только
specific delta.

- **Journey и границы:** PR закрывает один end-to-end путь из factual response
  до evidence surface. Не заявленные соседние surfaces остаются gated, без
  placeholder tab или client inference.
- **Dependencies и format:** API-изменение проходит Rust DTO → OpenAPI →
  generated TypeScript → consumer test в одном PR. Cursor/time/revision rules
  остаются endpoint-specific; stored format меняется только вместе с
  writer/reader compatibility policy.
- **Ресурсы и privacy:** request/response/cache reservation выполняется до
  materialization. Sensitive/heavy literals lazy, permission-aware, отсутствуют
  в URL/cursor/errors/log fields/metrics; bounded labels не становятся identity.
- **Failures и observability:** `{code, params}`, quality, coverage, skipped,
  cap и restart states получают отдельное EN/RU presentation. Qualification
  фиксирует exact SHA, bounds и outcome; runtime logs содержат только bounded
  low-cardinality fields без literals. Новые route metrics не подразумеваются.
- **Tests и acceptance:** focused RED/GREEN, OpenAPI/client freshness при wire
  change, EN/RU/a11y/responsive states и packaged-browser либо live HTTP fixture
  для соответствующего journey обязательны на exact head.
- **Stop и rollback:** недоказанная provenance, reservation, privacy или
  closed degraded semantics блокирует surface. Additive route/surface можно
  скрыть совместно; несовместимые wire/storage части не откатываются раздельно.

## P1. Entity, history и storage privacy-first

**Journey и scope.** Из frame пользователь открывает point/history dock с
typed identity, coverage и optional related entities; storage popover показывает
PGM/OVF/journal/quarantine bytes, filesystem free space и bounded write-rate.
Реализуются `/v1/entity/{view}/{entity}` и `/v1/storage`, три byte-accounted
cache, history до 32 PGM, 6 часов и 2000 snapshots.

**Evidence/dependencies.** Endpoints отсутствуют в `api_docs::configured`;
`statements_query` в `crates/kronika-source-pg/src/statements.rs` использует
`pg_stat_statements(false)` и `NULL::text AS query`. Старт после транша 3 и
истинного predecessor contract.

**Invariants/failures.** Listing identity-first; related entities только из
stored provenance. Collection availability и literal disclosure остаются
двумя согласованными осями. History/cache/response reservations выполняются до
decode/allocation; gap/reset/not-collected не становятся пустой строкой.

**Tests/acceptance.** Default statement detail показывает `not_collected`, а
не пустой SQL; plan text различает available/redacted/truncated/privilege
states; stored text рендерится только как plain React text без HTML/Markdown и
`dangerouslySetInnerHTML`, сохраняет исходное содержимое и не попадает в
URL/cursor/error/metrics. Concurrent cache,
cancellation, oversized entry, gap/reset и response-bound tests проходят.

**Stop/rollback/size.** До composition amendment двух availability axes и
byte-accounted cache qualification endpoint не публикуется. Additive routes
могут быть отключены вместе с dock без изменения frame. Размер L, после
транша 3.

## P1. Timeline, cursor и incidents

**Journey и scope.** В replay пользователь видит factual latest time,
gap/restart/stale/down markers, открывает incident с findings и coverage и
возвращается к range/focus. Timeline continuation и frame continuation
обрабатываются по своим contracts.

**Evidence/dependencies.** Timeline/anomaly/incident routes существуют, UI их
не вызывает. `web_lifecycle` доказывает post-restart expiry timeline events
cursor. Incident handler передаёт `ClockRelation::Simultaneous`; engine
считает relation описательной, не causal. Active incident specs оставляют
period/clock provenance, strict joins и qualification незавершёнными.

**Invariants/failures.** UI сначала показывает `analysis_status`, catalog,
capabilities, completeness, skipped и coverage. Finding называется гипотезой;
только stored `blocked_by` разрешает directional edge. `/readyz` не становится
data freshness. Transport cursor остаётся in-memory; durable URL/history/share
сохраняют absolute intent и удаляют continuation.

**Tests/acceptance.** PGM fixture покрывает gap, stale/down, event, restart и
incident focus. Timeline post-restart 410 и frame 400/410 имеют разные тексты и
recovery. Incident не показывает root cause или causal arrow без provenance;
mobile triage оставляет health/incidents/findings.

**Stop/rollback/size.** Directional/causal UX блокируется до runtime clocks и
strict joins. Текущие limited findings можно показывать read-only. UI surfaces
additive и могут быть скрыты без изменения API. Размер L, после общей time
model.

## P1. Интегрированная product qualification

**Journey и scope.** Один investigation journey проходит одинаково на EN/RU,
клавиатурой и screen reader, в dark/light и desktop/mobile modes. Это не
финальный cosmetic PR: каждый feature tranche 2–5 приносит свои strings,
formatters, focus/states и browser fixtures; затем общая матрица закрывает
пропуски.

**Invariants.** `<html lang>` синхронизирован; одна IANA timezone; wire time
UTC; `Accept-Language` не меняет machine response и не добавляет language
`Vary`; состояние не передаётся только цветом; loading/error/empty/partial
явны.

**Tests/acceptance.** Unit/property tests для formatter/URL/DST, runtime a11y,
keyboard focus, EN/RU overflow, visual regression и browser E2E против
packaged binary. Full journey включает deep-link, partial/gap/down, baseline,
frame, incident focus и mobile triage.

**Stop/rollback/size.** Feature tranche не принят без собственной EN/RU/a11y
и browser evidence. Общая closure — M; rollback не отключает accessibility или
одну locale отдельно.

## P1. OpenAPI/runtime и startup contract

**Journey и scope.** UI не формирует параметры, которые schema разрешает, а
runtime отвергает; timestamp не округляется. Оператор с неверным
`KRONIKA_WEB_LOG` получает non-zero exit до bind.

**Evidence.** OpenAPI frame `limit` и heatmap `buckets/top` не выражают runtime
bounds; часть closed states генерируется как `string`; TypeScript int64 query
имеет тип `number`. `init_tracing` в `bins/pg_kronika-web/src/main.rs`
молча заменяет неверный filter на `info`.

**Acceptance.** Schema/runtime positive и negative bounds совпадают; выбранный
timestamp representation проходит precision round-trip; closed/open enum
policy тестируется; invalid log filter падает до bind, valid запускается без
утечки secret. Размер S, после стабилизации state contract.

**Stop/rollback.** Нельзя закрывать enum, который объявлен extensible, или
менять timestamp wire form без синхронного generated-client migration.

## P1. Live HTTP BDD T7

**Journey и scope.** Две страницы live HTTP на границе segments возвращают
каждую строку один раз в стабильном порядке; range, totals, batch и gaps
соответствуют живому source. Покрываются multi-row activity, оба layouts
`pg_store_plans` и одна OS multi-scope section.

**Evidence/dependencies.** Точный остаток задан в
[`2026-07-10-web-api-bdd.md`](../plans/2026-07-10-web-api-bdd.md), T7. Он
использует существующий collector→reader→HTTP harness и live PostgreSQL
oracle; browser fixtures его не заменяют.

**Acceptance/resources.** Reusable steps не размножаются на каждую metric;
matrix публикует runtime и проходит поддерживаемые PostgreSQL majors. Нет
дублей/пропусков cursor page, неверных exact `/v1/segments` row totals или
bridge через gap.
Размер M; stop condition — flaky/sleep-based oracle или неприемлемый CI budget
без измеренного разбиения.

## P1. Mapping/narrowing Overview, Health и Index

**Journey и scope.** Пользователь видит одну непротиворечивую IA, а не
дублирующие Overview/Index tabs рядом с timeline/summary и catalog `indexes`.
Active analysis plan требует либо реальные surfaces с partial/error/coverage
browser evidence, либо явное narrowing.

**Точное mapping/narrowing.**

| Старое имя | Канонический путь | Граница |
| --- | --- | --- |
| Overview | существующие timeline overview/health/events + v6 summary/heatmap/spine | отдельная дублирующая вкладка не создаётся; полный Overview требует coverage/error journey |
| Index | catalog view `indexes` | это retained-stat view, не полный structural `DATA-007` |
| Health timeline | factual timeline health markers | не равен Health Score |
| Health dashboard | будущий score/services surface | gated до `HS-001..004` и bounded services contract |

PR #150 уже закрыл generated client/build claims старых diagnostics-документов;
они не возвращаются в future ordering.

**Acceptance.** После траншей 2–5 active docs и README используют эту таблицу
либо более поздний явный amendment. Нельзя объявить summary полноценным
Overview без coverage/error journey или потерять Health commitment. Размер S;
изменение делается вместе с содержательным PR, а не отдельной чисткой дерева.

## P2. Health, Compare, Settings и Log

Каждый из этих journeys начинается только после отдельного bounded,
locale-neutral backend contract с provenance, availability, privacy/cursor
revision, resource budget и real-data fixture. Один implementation PR закрывает
один путь `surface → drill-down → evidence`; пустые tabs и mock score запрещены.

Acceptance для старта: unknown/unavailable отличаются от healthy/zero,
coverage входит в score/evidence, literals lazy и permission-aware, server
bounds исполняемы. Размер каждого journey — L. До выполнения prerequisites это
dependency-gated P2, а не параллельная frontend работа.

## Ненормативные кандидаты

Следующие идеи требуют отдельного решения и новой/исправленной спецификации.
Они не входят в acceptance и порядок выше:

- consolidated quality ledger поверх обязательных partial/data-health states;
- per-cell/server heatmap evidence сверх mechanical baseline operands/quality;
- evidence trail `range → incident → finding → entity → raw fact`;
- portable evidence manifest сверх обязательного sanitized share;
- bounded raw-fact inspector, открываемый из конкретного evidence;
- CSP/XSS/bidi hardening для будущих literal-rich surfaces;
- HTTP transfer/compression и 64×256 interaction performance gates сверх
  текущего compressed-tar budget;
- low-cardinality route metrics для новых UI paths и conditional Basic metadata
  в OpenAPI.

## Явно вне текущего scope

- multi-root API, `source` selector и `/v1/sources`;
- claimed global daily top на основании retained top-K;
- SQL text по умолчанию, text identity или SQL в deep-link;
- client joins по PID, label, name или SQL;
- client filtering страницы под видом полного search;
- отдельный anomalies/findings inbox до incident workflow;
- causal/root-cause graph без stored directional provenance;
- generic dashboards, chart builder или plugin framework;
- raw explorer как primary navigation;
- custom sessions, logout и RBAC поверх optional Basic Auth;
- admin/config write UI, remediation, annotations и любые write paths;
- production locales сверх EN/RU.

## Условие завершения контракта

Документ становится `IMPLEMENTED` только когда транши 1–9 имеют executable
evidence, P2 остаётся в отдельном backend-gated contract, а active status map
перечисляет только фактический remaining scope. Завершённые пункты после каждого
PR сворачиваются в одну строку baseline и не остаются в future ordering.
