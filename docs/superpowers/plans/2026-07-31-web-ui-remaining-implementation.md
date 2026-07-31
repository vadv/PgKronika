# Оставшийся Web UI — план реализации

Дата: 2026-07-31.

**Статус: FUTURE.**

План подготовлен относительно `main` на
[`0fbc3dff79a12496d9c1ca16f7208a96da880fa3`](https://github.com/vadv/PgKronika/commit/0fbc3dff79a12496d9c1ca16f7208a96da880fa3).
Нормативное поведение, границы и
критерии продукта заданы в
[`2026-07-31-web-ui-remaining-contract.md`](../specs/2026-07-31-web-ui-remaining-contract.md).
Этот документ отвечает только на вопрос: в какой последовательности и какими
проверяемыми изменениями реализовать контракт.

## Goal

Довести Web UI от реализованного v6 baseline до честного investigation flow:
сначала доказать истинность сохранённых `EntitySeries`, затем связать все
видимые панели одним точным browser state и только после этого добавить
server-driven frame, detail и incident journeys.

## Architecture

Работа разбита по цельным вертикальным траншам. Каждый транш начинается от
актуального `main` после принятия предшественника и поставляет вместе все
затронутые writer/reader/API/generated-client/UI/test/qualification части.
Стековые half-contract PR и отдельные PR по одному component запрещены.

## Tech Stack

Rust workspace, OVF/PGM reader и writer, Axum/OpenAPI, сгенерированный
TypeScript client, React 19/Vite/TanStack Query, Vitest и Playwright против
упакованного `pg_kronika-web`.

## Normative inputs

- Remaining-scope contract:
  [`2026-07-31-web-ui-remaining-contract.md`](../specs/2026-07-31-web-ui-remaining-contract.md).
- `EntitySeries` format and truth:
  [`2026-07-28-entity-series-block-design.md`](../specs/2026-07-28-entity-series-block-design.md).
- UI API and resource bounds:
  [`2026-07-28-web-ui-api-design.md`](../specs/2026-07-28-web-ui-api-design.md).
- UI composition:
  [`2026-07-30-web-ui-v6-design.md`](../specs/2026-07-30-web-ui-v6-design.md).
- Timeline and incidents:
  [`2026-07-22-overview-index-timeline-api.md`](../specs/2026-07-22-overview-index-timeline-api.md),
  [`2026-07-16-kronika-incident-lenses-design.md`](../specs/2026-07-16-kronika-incident-lenses-design.md)
  and
  [`2026-07-17-kronika-incident-implementation.md`](../specs/2026-07-17-kronika-incident-implementation.md).

## Implemented baseline — не backlog

[PR #150](https://github.com/vadv/PgKronika/pull/150) уже поставил SPA,
catalog, минимальные summary/heatmap, OpenAPI-generated client, deterministic
static tarball и полный набор
frontend/OpenAPI/static gates. Планы scaffold и summary/heatmap и дизайн
codegen не исполняются повторно. Следующие задачи расширяют этот baseline.

## Global constraints

- Runtime остаётся single-root: нет `source` selector, key или API.
- Timestamp/identity/sort values не проходят через JavaScript `Number`, если
  server contract допускает потерю точности; codec использует decimal string
  или `BigInt` до presentation boundary.
- Missing не равен zero; gap, reset, absent, gated, unavailable revision,
  `resource_limited` и active tail сохраняются раздельно.
- Нет PID-only и client-side joins. Activity identity включает
  `(pid, backend_start=starttime)` и temporal provenance.
- Frame и default statement path не содержат query/plan text. Statement SQL
  штатно `NULL`, пока отдельный privacy-aware detail contract не докажет иное.
- Все allocations, responses, caches и builders имеют byte-accounted bound до
  materialization. Cap failure даёт closed degraded state.
- Изменение API проходит цепочку Rust DTO → OpenAPI → generated TypeScript →
  consumer test в одном транше.
- Каждый новый UI state поставляется одновременно для EN/RU, `Intl`, keyboard,
  focus, loading/error/empty/partial и responsive layout. Полный mobile triage
  health/incidents/findings появляется вместе с incident surface в Tasks 5–6.
- Нормативный browser smoke запускается против packaged binary, не только
  Vite dev server.

## Delivery map

| PR | Приоритет | Размер | Результат | Почему сейчас | Backend gate |
| ---: | --- | --- | --- | --- | --- |
| 1 | P0 | L | `EntitySeries` truthfulness + qualification | все следующие экраны зависят от истинности stored series | текущий projection/OVF baseline |
| 2 | P0 | M | honest browser state + truthful summary/heatmap | панели уже видимы пользователю, но теряют time precision и quality | PR 1 принят |
| 3 | P0 | L | Context API + catalog-driven Frame/TableView | frame уже обслуживается server, но UI/context journey отсутствует | PR 2 принят |
| 4 | P1 | L | entity/history/storage privacy-first | stable frame впервые даёт честную точку drill-down | PR 3 и detail composition contract |
| 5 | P1 | L | timeline/restart/cursor/incidents | единая time model позволяет не фабриковать restart/causality | time model из PR 2 |
| 6 | P1 | M | интегрированная product qualification | закрывает cross-feature evidence, не косметический долг | evidence из PR 2–5 |
| 7 | P1 | S | OpenAPI/runtime bounds и strict startup | schema drift и permissive log filter мешают безопасному client/startup | state/API contracts стабильны |
| 8 | P1 | M | live HTTP BDD T7 | закрывает точный остаток активного T7 | существующий live harness |
| closure | P1 | S | mapping/narrowing Overview/Health/Index | убирает двусмысленную IA после появления реальных journeys | включается в PR 5 или 6 |
| — | P2 | L/journey | Health/Compare/Settings/Log | сейчас строить нельзя: нет bounded backend facts | отдельные bounded backend contracts |

PR 1–3 идут строго последовательно. PR 4–8 могут переставляться только если их
явные зависимости соблюдены; closure mapping входит в PR 5 или 6. Изменение
порядка фиксируется в active status map до начала реализации.

Ближайшие два substantive PR: сначала PR 1 целиком, затем PR 2 целиком. PR 3
не стартует параллельно: иначе TableView закрепит недоказанную series/time
semantics и создаст повторную миграцию UI state.

## Task 0: сверить head и зафиксировать evidence envelope

Перед каждым траншем:

- [ ] Fetch `origin/main`, записать exact base SHA в PR и qualification
      artifact.
- [ ] Проверить active specs, code symbols и tests, перечисленные в
      remaining contract; не выводить статус из имени файла или старого PR.
- [ ] Согласовать format/API revision и rollback unit до первого production
      изменения.
- [ ] Зафиксировать измеряемые memory/read/response/browser bounds и stop
      conditions в PR body.
- [ ] Открывать один non-draft PR только после прохождения локальных
      target-gates; дождаться exact-head CI attempt 1.

## Task 1 / PR 1: `EntitySeries` truthfulness + qualification

**Рабочие области:**
`crates/kronika-reader/src/overview/web_index/build.rs::{build_view_series,evaluate_deltas}`,
`crates/kronika-analytics/src/web_projection.rs`, EntitySeries codec/model,
`bins/pg_kronika-web/src/ui/catalog.rs` и соответствующие tests/qualification
artifacts.

### 1.1 Boundary semantics and revision

- [ ] Написать RED fixtures для normal predecessor на границе двух segments,
      reset, explicit gap, absent predecessor и disappearing/reappearing row.
- [ ] Ввести bounded predecessor state между segments; не переносить значение
      через доказанный gap и не вычислять delta после неизвестного reset.
- [ ] Применить selective revision policy: `metric_revision` для formula/reset/
      aggregation/unit, `view_revision` для identity/join/required inputs,
      `block_revision` только для wire-layout incompatibility.
- [ ] Старые wrongly-complete metrics сделать `unavailable_revision` точным
      `metric_revision` bump; background rebuild/migration не входят в PR.
      Поддерживаемые независимые revisions читаются.
- [ ] Добавить reader↔writer round-trip, active-tail/sealed parity и negative
      unsupported-revision tests.

### 1.2 Activity temporal identity

- [ ] Написать positive/negative fixtures для `activity + process`: совпадает
      весь `(pid, backend_start=starttime)` в том же snapshot/co-temporal
      sample; PID-only, reused PID, duplicate, missing и nearest-time candidate
      не join-ятся.
- [ ] Реализовать typed join с provenance и убрать общий
      `requires.len() != 1` gate только для полностью определённых multi-input
      projections.
- [ ] Оставить CPU/I/O `gated`, если provenance входа не доказана.

### 1.3 Bounded degradation and ranking truth

- [ ] Написать cap fixtures отдельно для каждой metric/view и whole-build.
- [ ] Резервировать память до decode/materialization; публиковать
      `resource_limited` только для затронутой metric/view, сохраняя соседние.
- [ ] Не публиковать произвольный partial top. `ranking.exact=true` допустим
      только при доказанном `unseen_upper`/candidate bound.
- [ ] Проверить `lower <= truth <= upper`, churn/end-leader и invisible
      candidate property/golden tests.

### 1.4 N=96/1440 qualification

- [ ] На точном head собрать correctness/size/RSS/read/decompression/ranking
      evidence для N=96 и N=1440.
- [ ] Использовать четыре обязательных size cases: 96 segments × 15m,
      1440 × 1m, size-seal production fixture и максимальный statements union
      top-K.
- [ ] Доказать 64 МиБ decoded rows, 32 МиБ builder одного view, не более
      100 МиБ дополнительного peak, top-K 64, identity 256 bytes, label
      160 bytes, stored/decoded view ≤256 КиБ и 0 PGM reads на heatmap serving
      path.
- [ ] Записать targets: median selected stored view ≤24 КиБ, все web-блоки
      сегмента ≤10% PGM на production fixture, daily web blocks ≤10 МиБ при
      штатном seal. Не применять эти targets как hard gate к adversarial
      1440×1m. При miss сокращать labels/materialized metrics либо остановиться;
      не ослаблять hard caps, K или `exact_score`.
- [ ] Сохранить команды, exact SHA, corpus/fixture и raw measurements в
      repository-owned qualification artifact.

### 1.5 PR 1 acceptance and stop

- [ ] Все boundary deltas считаются ровно один раз; gap/reset/absent остаются
      missing, а не zero.
- [ ] Activity CPU/I/O видимы только при полном temporal identity match.
- [ ] Cap одной metric/view не запрещает остальные и не создаёт ложный top.
- [ ] Writer, reader, revision, tests и artifact входят в один PR.
- [ ] Остановить публикацию при недоказанном provenance, reservation или
      revision transition; rollback — предыдущий binary, читающий только
      поддерживаемые независимые block/view/metric revisions.

Целевые проверки:

```bash
cargo test -p kronika-analytics
cargo test -p kronika-reader
cargo test -p pg_kronika-web
cargo +1.96.0 fmt --all --check
RUSTFLAGS='-D warnings' cargo clippy --workspace --all-targets
cargo run -p xtask -- check-deps
python3 -B scripts/validate-single-root-terminology.py
git diff --check
```

## Task 2 / PR 2: honest browser state + truthful summary/heatmap

**Рабочие области:** `web/src/App.tsx`, `web/src/state/url.ts`, catalog,
summary/heatmap hooks and components, i18n/formatters, packaged assets и browser
fixtures. Backend wire меняется только при выявленном schema/runtime mismatch.

### 2.1 Canonical state and navigation

- [ ] Написать property tests для precision-safe
      `at/span/baseline/live/view/metric`: round-trip, invalid input, limits,
      DST, reload и Back/Forward.
- [ ] В этом же PR заменить опасную OpenAPI `int64 → TypeScript number`
      границу для time query на decimal-string schema/serializer,
      регенерировать client и использовать `BigInt` внутри arithmetic;
      milliseconds разрешены только для display.
- [ ] Удалить fake `source` из URL, header, cache keys и copy/share; legacy
      `source` canonicalize away без выбора другого root.
- [ ] Подписать state reducer на navigation и выводить summary/heatmap из
      одного absolute `at/span`, без независимого mount-time `Date.now()`.
- [ ] Разделить transient continuation и sanitized share state.

### 2.2 Truthful baseline and quality presentation

- [ ] Загружать heatmap baseline вторым independently cached request и merge
      только по opaque entity token.
- [ ] Показывать baseline delta и mechanical why: оба операнда, delta, unit и
      quality/provenance без client verdict; не добавлять table delta, пока API
      не доказывает один и тот же paginated entity set.
- [ ] Merge разрешён только для одинаковых entity/view/metric revisions,
      `bucket_count`, bucket width/span и relative index alignment; absolute
      ranges различаются на baseline offset. Missing с любой стороны означает
      «нет delta», не zero.
- [ ] Поскольку текущие `HeatmapResponse`/`HeatmapRow` не несут revisions,
      добавить typed `view_revision`/`metric_revision` через Rust DTO → OpenAPI
      → generated client и включить их в оба cache keys и merge proof.
- [ ] Отобразить отдельно collection `N/M`, `read_state`, `visibility`,
      retained exact/approx/unseen и доступные gap/null/gated/unavailable/
      `resource_limited`/active-tail/observed-zero facts на их server-defined
      гранулярности. Response-level reason не переносится на отдельную cell;
      недостающая cell reason требует typed DTO/OpenAPI field.
- [ ] Использовать server unit и score bounds. Copy не называет retained
      top-K глобальным daily top.

### 2.3 Catalog refresh, EN/RU, a11y and mobile

- [ ] Реализовать conditional catalog flow: 200+ETag, последующий
      `If-None-Match`, 304 reuse, явный refresh и поздний 200 с новой revision.
- [ ] Добавить локализованные loading/error/empty/degraded states и safe
      fallback для unknown `{code, params}`.
- [ ] Общие `Intl` formatters обеспечивают EN/RU parity, UTC wire и одну
      выбранную IANA timezone; `<html lang>` синхронизирован.
- [ ] Реализовать keyboard/focus path, non-color legend и screen-reader/table
      equivalent для heatmap.
- [ ] На viewport `<760px` summary/heatmap остаются responsive, без horizontal
      overflow и скрытых обязательных states. Incident mobile triage — Task 5.

### 2.4 Packaged qualification and PR 2 acceptance

- [ ] Browser fixtures на packaged binary покрывают deep-link, reload,
      Back/Forward, baseline, partial/gap/down, catalog 200/304/refresh, EN/RU,
      keyboard и mobile.
- [ ] Copy/share из live режима фиксирует absolute `at/range`; continuation
      cursor остаётся только в in-memory query state.
- [ ] Проверить hard maximum 64×256 cells без unbounded DOM/state growth.
- [ ] Собрать deterministic tarball и commit-ить frontend source+tarball
      атомарно.
- [ ] Остановить PR, если панели показывают разные ranges, 304 становится
      error, state теряет precision/quality или smoke работает лишь с Vite.

Целевые проверки:

```bash
make web-frontend-check
make web-frontend
make web-bundle-budget
make openapi
make openapi-lint
make web-codegen
git diff --exit-code -- bins/pg_kronika-web/openapi
git diff --exit-code -- web/src/api/schema.d.ts
git diff --exit-code -- bins/pg_kronika-web/static.tar.gz
cargo test -p pg_kronika-web
cargo +1.96.0 fmt --all --check
RUSTFLAGS='-D warnings' cargo clippy --workspace --all-targets
cargo run -p xtask -- check-deps
python3 -B scripts/validate-single-root-terminology.py
git diff --check
```

## Task 3 / PR 3: Context API + catalog-driven Frame/TableView

**Рабочие области:** context DTO/reader/handler/OpenAPI,
`bins/pg_kronika-web/src/ui/frame`, `api_docs::configured`, generated client,
URL query state и новый общий TableView/Toolbar/Header.

### 3.1 Bounded Context API

- [ ] RED tests: полный logical database list не зависит от active frame; role,
      replication и quality не выводятся из строк текущей страницы.
- [ ] Реализовать `GET /v1/ui/context?at=<decimal i64>` без `source`, closed
      error/degraded states, pre-materialization reservation и hard maximum
      512 КиБ encoded response.
- [ ] В пределах cap список полный; превышение возвращает typed
      `response_too_large` без rows, а UI показывает context unavailable.
      Partial list требует отдельного deterministic pagination/omitted-count
      amendment.
- [ ] Экспортировать OpenAPI и generated types; freshness test запрещает
      ручные параллельные DTO во frontend.

### 3.2 Server-driven frame journey

- [ ] Добавить frame/context hooks, durable whitelisted typed facets/sort/focus
      и transient in-memory `q`/page continuation; free `q` и cursor не входят
      в URL/history/share.
- [ ] Построить один catalog-driven TableView для всех девяти views: columns,
      presets, units и sort/filter capabilities только из catalog.
- [ ] Использовать server `q/sort/page/neighbors` и точный retained
      `matched`; рядом показывать point collection `N/M` из summary.
- [ ] Не искать по lazy/unknown fields и не фильтровать загруженную страницу
      под видом полного поиска.
- [ ] Entity select меняет focus/URL и выделяет frame row. Detail/evidence
      surface появляется только после server contract в Task 4; query/plan
      text не попадает во frame.

### 3.3 Cursor, bounds and acceptance

- [ ] Проверить frame maximum 1 МиБ, query/limit/cursor bounds, один точный PGM
      и максимум второй predecessor PGM.
- [ ] Context tests доказывают full response в пределах 512 КиБ, fail-closed cap
      outcome выше предела и qualification fields row count/encoded bytes/cap
      outcome без database names.
- [ ] Malformed/query-mismatched cursor и unavailable snapshot получают
      разные 400/410 presentation. Recovery сохраняет range/q/sort, сбрасывает
      continuation и не сшивает разные snapshots.
- [ ] Matrix всех девяти views доказывает server order/filter/matched,
      two-page continuation без дублей и отсутствие lazy cells.
- [ ] Packaged-browser journey проходит heatmap → frame → focus →
      Back/Forward.
- [ ] Остановить PR, если context зависит от rows страницы, client вычисляет
      `matched` или cursor recovery меняет snapshot молча.

Целевые проверки:

```bash
cargo test -p kronika-reader
cargo test -p pg_kronika-web
make openapi
make openapi-lint
make web-codegen
git diff --exit-code -- bins/pg_kronika-web/openapi
git diff --exit-code -- web/src/api/schema.d.ts
make web-frontend-check
make web-frontend
make web-bundle-budget
git diff --exit-code -- bins/pg_kronika-web/static.tar.gz
cargo +1.96.0 fmt --all --check
RUSTFLAGS='-D warnings' cargo clippy --workspace --all-targets
cargo run -p xtask -- check-deps
python3 -B scripts/validate-single-root-terminology.py
git diff --check
```

## Обязательные поля P1-траншей

Нормативная карточка каждого P1-транша находится в remaining contract. Перед
кодом PR body дополняет её exact head SHA и заполняет все владельцы ниже;
пропущенное поле является stop condition, а не задачей «на потом».

| Транш | Exact evidence и dependency | Data/API/format | Resource/privacy | Failure, degradation, observability | Tests, acceptance, rollback |
| --- | --- | --- | --- | --- | --- |
| 4 entity/history/storage | `api_docs::configured`; `crates/kronika-source-pg/src/statements.rs::statements_query`; после PR 3 | entity/storage DTO, две availability axes, query default absent | 32 PGM/6h/2000 snapshots, byte-accounted caches; heavy literals lazy | typed disclosure/cap states; cache reservations/hit/join/cancel evidence без literals | point/history/storage and privacy fixtures; additive route+dock rollback |
| 5 timeline/incidents | existing timeline/incident routes; `web_lifecycle`; incident `ClockRelation`; time model PR 2 | endpoint-specific 31d/24h bounds, in-memory cursor, absolute durable intent | bounded existing responses; sanitized share без cursor/literals | coverage/skipped/clock/restart видимы; endpoint-specific 400/410 notice | PGM restart/gap/incident packaged journey; hide additive surfaces on rollback |
| 6 product qualification | UI/i18n/formatter/focus/browser suites после PR 2–5 | wire не меняется без отдельного DTO/OpenAPI step | 64×256 UI bound; fixture не содержит secrets | normal/partial/gap/down matrix и exact binary SHA | EN/RU/a11y/keyboard/theme/viewport E2E; no locale/a11y rollback |
| 7 API/runtime/startup | `api_docs::configured`, OpenAPI tree, `main.rs::init_tracing`; после time fix PR 2 | runtime bounds/enums/schema parity; strict log filter | bounded existing responses; log не раскрывает secret | unknown enum fallback; invalid filter exits before bind | schema positive/negative + process tests; synchronized DTO/client rollback |
| 8 live HTTP T7 | active plan T7 и existing in-process router/live PG harness | production wire неизменен; доказываются range/order/cursor/totals/gaps | matrix runtime измерен; reusable fixtures bounded | assertion показывает endpoint/major/fixture без sensitive values | supported-major matrix; test-only rollback, semantic coverage не удаляется |
| closure mapping | active docs, shipped routes and UI после PR 2–5 | wire/storage не меняются | resource/privacy claims только с уже принятым evidence | stale/duplicate names становятся exact mapping или gated | status-map review в PR 5/6; additive wording rollback |

## Task 4 / PR 4: entity/history/storage privacy-first

- [ ] До кода согласовать composition двух осей: collection availability и
      literal disclosure. Они не схлопываются в один `available`.
- [ ] RED tests: default statement query `not_collected`, redacted/truncated/
      privilege-denied literals, gap/reset history, oversized response,
      concurrent cache, cancellation и singleflight.
- [ ] Реализовать bounded `/v1/entity/{view}/{entity}` point+history и
      `/v1/storage`; related entities только из stored provenance.
- [ ] Соблюсти history ≤32 PGM, ≤6h, ≤2000 snapshots и byte reservations для
      metadata/EntitySeries/PGM projection caches до decode.
- [ ] Не помещать sensitive/heavy literals — SQL, plan/log text, definitions,
      paths, secrets — в listing, URL, cursor, error params или metrics.
      Bounded identity/display labels остаются verbatim и не становятся
      identity.
- [ ] Рендерить stored text только как plain React text, без HTML/Markdown и
      `dangerouslySetInnerHTML`; full CSP/bidi policy остаётся отдельным
      ненормативным решением.
- [ ] Добавить privacy-first dock/popover, EN/RU/a11y/mobile и packaged journey.
- [ ] Остановиться до публикации route, если disclosure composition или
      byte-accounted cache bound не доказаны. Rollback отключает additive
      routes/surface без изменения frame.

Размер L; старт только после PR 3. Все API/codegen/frontend/static/generic gates
из Task 3 обязательны.

## Task 5 / PR 5: timeline/restart/cursor/incidents

- [ ] RED fixture: factual latest time, gap, stale/down, event, restart,
      incident focus и возврат к absolute range.
- [ ] Связать timeline и incidents с time model PR 2. Timeline допускает 31d;
      heatmap/frame/incidents — 24h. Не расширять endpoint bounds на клиенте.
- [ ] Continuation cursor хранить только transient. Timeline post-restart 410
      и frame 400/410 получают endpoint-specific recovery и тексты: cursor
      удаляется, первая страница того же absolute intent запрашивается заново,
      пользователь видит notice.
- [ ] Сначала показывать `analysis_status`, capabilities, completeness,
      skipped и coverage; finding остаётся гипотезой.
- [ ] Рисовать directional relation только из stored `blocked_by`; при
      `ClockRelation::Simultaneous` без provenance causal UX запрещён.
- [ ] Packaged browser и live lifecycle tests покрывают restart и gap без
      ложного continuation или causality.
- [ ] Rollback скрывает additive UI surfaces; stop — отсутствие runtime clock
      provenance или strict joins для заявленного causal behavior.

Размер L; зависит от Task 2. API/codegen gates добавляются только при wire
изменении, generic Rust/frontend/static gates обязательны.

## Task 6 / PR 6: закрыть интегрированную product qualification

- [ ] Свести evidence из Tasks 2–5 в одну матрицу journeys × EN/RU ×
      keyboard/screen reader × light/dark × desktop/mobile.
- [ ] Проверить deep-link, partial/gap/down, baseline, frame, entity/incident
      focus и sanitized share против packaged binary.
- [ ] Добавить только недостающие fixtures/formatters/focus corrections; не
      откладывать сюда обязательную доступность предыдущих features.
- [ ] Зафиксировать browser version, binary SHA, fixture and viewport bounds.
- [ ] Stop — flaky timing/sleep oracle или dev-server-only evidence. Rollback
      не может отключать accessibility либо одну locale отдельно.

Размер M. Pseudo-locale не является acceptance этого PR и требует отдельного
ненормативного решения.

## Task 7 / PR 7: OpenAPI/runtime bounds and strict startup

- [ ] Сопоставить для каждого закрытого UI endpoint runtime min/max, enums,
      cursor representation и OpenAPI schema; написать positive/negative
      parity tests.
- [ ] Проверить, что time representation, исправленная в Task 2, не
      регрессировала в новых endpoints.
- [ ] Закрывать enum только если runtime contract закрыт; extensible values
      сохраняют unknown-safe consumer path.
- [ ] Написать process tests: неверный `KRONIKA_WEB_LOG` завершает процесс
      non-zero до bind, допустимый filter запускается; secret не попадает в log.
- [ ] Выполнить `make openapi`, `make openapi-lint`, `make web-codegen`,
      freshness diff и все Rust/frontend gates.
- [ ] Stop — schema шире/уже runtime либо generated client расходится с DTO.

Размер S. Route metrics и conditional Basic metadata сюда не входят без
отдельного решения.

## Task 8 / PR 8: live HTTP BDD T7

Источник шагов — раздел T7 в
[`2026-07-10-web-api-bdd.md`](2026-07-10-web-api-bdd.md).

- [ ] Переиспользовать in-process HTTP router поверх collector→reader и live
      PostgreSQL oracle; не подменять browser mocks доказательством reader
      semantics.
- [ ] Проверить range/order, exact `/v1/segments` row totals, две cursor pages
      без дублей/пропусков, batch и explicit gaps.
- [ ] Добавить multi-row activity, оба `pg_store_plans` layouts и одну OS
      multi-scope section.
- [ ] Запустить поддерживаемые PostgreSQL majors и опубликовать runtime matrix.
- [ ] Stop — sleep-based/flaky oracle или неограниченный CI budget. Сначала
      измерить и сузить fixture, не убирать semantic coverage.

Размер M. Этот PR не добавляет новый UI и не заменяет packaged browser E2E.

## Closure step в PR 5 или 6: mapping/narrowing Overview, Health and Index

- [ ] После Tasks 2–5 составить однозначную mapping table старых названий и
      v6 journeys: summary/timeline, catalog `indexes`, incidents и
      backend-gated Health.
- [ ] Для каждого активного commitment выбрать только одно: уже покрытый
      surface с evidence, точный remaining owner либо explicit narrowing.
- [ ] Обновить active status map additive wording; не удалять исторические
      документы и не создавать дублирующие Overview/Index tabs.
- [ ] Не называть минимальный summary полноценным Overview без coverage/error
      journey и не объявлять Health реализованным без score/services contract.

Размер S. Это closure часть содержательного PR 5 или 6, не самостоятельный
docs-only micro-PR и не чистка docs tree.

## P2 entry criteria: Health, Compare, Settings and Log

Executable checklist для этих journeys создаётся только после появления
отдельного backend contract. Для старта каждого обязательны:

- bounded locale-neutral endpoint и real-data fixture;
- provenance, coverage и честные unknown/unavailable/healthy/zero states;
- privacy/literal policy, cursor/revision semantics и response/cache budgets;
- один цельный путь `surface → drill-down → evidence`;
- EN/RU/a11y/mobile/browser acceptance.

До выполнения entry criteria запрещены empty/mock tabs, client-computed score,
admin/write UI и generic dashboard. Оценка — L на каждый journey.

## Repository-wide gates

Каждый production PR запускает focused RED/GREEN tests, затем применимые
layer-gates и полный repository contract:

```bash
cargo +1.96.0 fmt --all --check
RUSTFLAGS='-D warnings' cargo clippy --workspace --all-targets
cargo test --workspace
cargo run -p xtask -- check-deps
python3 -B scripts/validate-single-root-terminology.py
git diff --check
```

При изменении API дополнительно:

```bash
make openapi
make openapi-lint
make web-codegen
git diff --exit-code -- bins/pg_kronika-web/openapi
git diff --exit-code -- web/src/api/schema.d.ts
```

При изменении frontend/static assets дополнительно:

```bash
make web-frontend-check
make web-frontend
make web-bundle-budget
git diff --exit-code -- bins/pg_kronika-web/static.tar.gz
```

Exact-head GitHub Actions должны завершиться успешно. Неизменённый упавший run
не перезапускается: причина исправляется новым commit. Qualification artifact
всегда связывает SHA, fixture/corpus, команды и measured bounds.

## Общие stop и rollback rules

- Не публиковать значение или статус, если provenance/quality field отсутствует;
  unknown остаётся unknown.
- Не ослаблять cap и не принимать unbounded allocation ради прохождения fixture.
- Не смешивать несовместимые stored revisions; writer+reader+revision — один
  rollback unit.
- Frontend source и embedded tarball откатываются атомарно.
- Additive endpoint можно скрыть/отключить, но нельзя оставлять UI, который
  молча реконструирует отсутствующий backend fact.
- При изменении active requirement status map обновляется в том же PR;
  завершённый транш сворачивается в compact baseline, а не остаётся checklist.

## Definition of roadmap complete

Tasks 1–8 и closure step имеют exact-head test/qualification evidence, active
documents не
обещают уже завершённые работы, а P2 journeys либо получили отдельные backend
contracts, либо явно остаются dependency-gated. Ненормативные candidates не
считаются обязательствами без отдельного решения.
