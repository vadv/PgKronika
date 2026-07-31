# Web UI v6 — Summary & Heatmap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Оживить shell из PR #148 реальными панелями: счётчики/статусы вкладок из `/v1/views/summary` и полоса heatmap сущностей из `/v1/timeline/heatmap` с переключателем метрики.

**Architecture:** Бэкенд main уже обслуживает `/v1/ui/catalog` (без `source` — сервер single-source), `/v1/views/summary?at=`, `/v1/timeline/heatmap?view&metric&from&to&buckets&top`. Фронт чинится под реальный контракт (scaffold писал `?source=` по спеке — реальность отличается), затем TanStack Query хуки → TabBar badges → HeatmapStrip (свой рендер сетки, токены, честные null).

**Tech Stack:** React 19 + TS strict, TanStack Query, vitest, существующие слои `web/src/{api,state,components,design,i18n}`.

**Спека:** `docs/superpowers/specs/2026-07-30-web-ui-v6-design.md` (PR #147). Предыдущий план: `docs/superpowers/plans/2026-07-30-web-ui-v6-scaffold.md` (PR #148, ветка-база).

## Global Constraints

- Ветка `feat/web-ui-v6-summary-heatmap` от `feat/web-ui-v6-scaffold` (stacked PR, base = scaffold, пока тот не смержен).
- Skill `pgkronika-frontend` (`~/.kimi-code/skills/pgkronika-frontend/SKILL.md`) обязателен: токены, i18n, честные null, verdict-раскраска, никаких хардкод-строк/цветов.
- Коммиты conventional, после каждого таска `make web-frontend-check` зелёный; меняется UI → пересобрать `make web-frontend` и закоммитить `static/`.
- Пуш после каждого таска (просьба владельца).
- eslint strict: никаких suppress'ов — чиним код.

## Verified backend contract (снято с живого бинарника + ui/handlers.rs, 2026-07-31)

- `GET /v1/ui/catalog` — БЕЗ query-параметров (только header `If-None-Match`; 304 при совпадении). Любой query-параметр → `unknown_query_parameter`.
- `GET /v1/views/summary?at=<i64 us>` → `{ at_us: string, views: [{ view, snapshot_ts_us: string|null, population: number|null, status: string, notable: boolean }], quality: {...} }`. `population: null` у gated view.
- `GET /v1/timeline/heatmap?view=<code>&metric=<code>&from=<i64 us>&to=<i64 us>&buckets?=<1..256 def 56>&top?=<1..64 def 8>` → `{ grid: { from_us, to_us, bucket_count }, ranking: { exact: boolean, unseen_upper: number }, rows: [{ entity: string, label: string, unit: string, score: { lower: number, upper: number }, values: (number|null)[] }], quality: { status: "complete"|"partial"|"unavailable", snapshots, gaps: [{from_us,to_us}], gated: [], unavailable_revision: [], resource_limited: [], unbounded_segments: [], active_tail: boolean } }`.

---

### Task 1: API-клиент под реальный бэкенд

**Files:**
- Modify: `web/src/api/catalog.ts` (убрать `?source=`)
- Modify: `web/src/api/catalog.test.ts`
- Create: `web/src/api/summary.ts`, `web/src/api/summary.test.ts`
- Create: `web/src/api/heatmap.ts`, `web/src/api/heatmap.test.ts`
- Modify: `web/src/api/types.ts` (+DTO summary/heatmap)
- Modify: `web/src/App.tsx` (useCatalog() без аргумента)

**Interfaces:**
- Consumes: `apiFetch`, `ApiError` (scaffold Task 5).
- Produces:
  - `useCatalog()` — без аргументов, ключ `["catalog"]`.
  - `interface ViewSummaryItem { view: string; snapshot_ts_us: string | null; population: number | null; status: string; notable: boolean }`
  - `interface SummaryResponse { at_us: string; views: ViewSummaryItem[]; quality: QualityMeta }`
  - `interface QualityMeta { status: "complete" | "partial" | "unavailable"; snapshots: number; gaps: { from_us: string; to_us: string }[]; gated: string[]; unavailable_revision: string[]; resource_limited: string[]; active_tail?: boolean }`
  - `useSummary(at: string): UseQueryResult<SummaryResponse>` — ключ `["summary", at]`.
  - `interface HeatmapRow { entity: string; label: string; unit: string; score: { lower: number; upper: number }; values: (number | null)[] }`
  - `interface HeatmapResponse { grid: { from_us: string; to_us: string; bucket_count: number }; ranking: { exact: boolean; unseen_upper: number }; rows: HeatmapRow[]; quality: QualityMeta & { unbounded_segments?: string[] } }`
  - `useHeatmap(args: { view: string; metric: string; from: string; to: string; buckets?: number; top?: number })` — ключ `["heatmap", view, metric, from, to, buckets, top]`.

- [ ] **Step 1: Падающий тест на отсутствие source-параметра**

В `catalog.test.ts` изменить ожидание URL:

```ts
expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe("/v1/ui/catalog");
```

и вызов `useCatalog()` без аргумента. Run: `cd web && npx vitest run src/api/catalog.test.ts` — FAIL (текущий код шлёт `?source=`).

- [ ] **Step 2: Починить catalog.ts и App.tsx**

```ts
export function useCatalog() {
  return useQuery({
    queryKey: ["catalog"],
    queryFn: () => apiFetch<ProjectionCatalog>("/v1/ui/catalog"),
    staleTime: Infinity,
  });
}
```

В `App.tsx`: `const catalog = useCatalog();` (убрать `state.source` из вызова; `state.source` в URL остаётся — будущий multi-source).

- [ ] **Step 3: Типы + падающий тест summary**

В `types.ts` добавить интерфейсы из блока Interfaces выше (`QualityMeta`, `ViewSummaryItem`, `SummaryResponse`).

`summary.test.ts`:

```ts
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode, afterEach } from "react";
import { expect, test, vi } from "vitest";
import { useSummary } from "./summary";

afterEach(() => vi.unstubAllGlobals());

test("useSummary requests /v1/views/summary?at=", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"at_us":"1","views":[],"quality":{"status":"complete","snapshots":0,"gaps":[],"gated":[],"unavailable_revision":[],"resource_limited":[]}}', { status: 200 }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(() => useSummary("1722400000000000"), { wrapper });
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe(
    "/v1/views/summary?at=1722400000000000",
  );
});
```

Run: `cd web && npx vitest run src/api/summary.test.ts` — FAIL (модуля нет).

- [ ] **Step 4: Реализация summary.ts**

```ts
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { SummaryResponse } from "./types";

export function useSummary(at: string) {
  return useQuery({
    queryKey: ["summary", at],
    queryFn: () =>
      apiFetch<SummaryResponse>(`/v1/views/summary?at=${encodeURIComponent(at)}`),
  });
}
```

- [ ] **Step 5: Типы + падающий тест heatmap**

В `types.ts` добавить `HeatmapRow`, `HeatmapResponse` (из Interfaces).

`heatmap.test.ts`:

```ts
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode, afterEach } from "react";
import { expect, test, vi } from "vitest";
import { useHeatmap } from "./heatmap";

afterEach(() => vi.unstubAllGlobals());

test("useHeatmap builds query with all params", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"grid":{"from_us":"0","to_us":"1","bucket_count":56},"ranking":{"exact":false,"unseen_upper":0},"rows":[],"quality":{"status":"partial","snapshots":0,"gaps":[],"gated":[],"unavailable_revision":[],"resource_limited":[]}}', { status: 200 }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(
    () => useHeatmap({ view: "statements", metric: "time", from: "0", to: "86400000000", buckets: 56, top: 8 }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe(
    "/v1/timeline/heatmap?view=statements&metric=time&from=0&to=86400000000&buckets=56&top=8",
  );
});
```

Run: FAIL (модуля нет).

- [ ] **Step 6: Реализация heatmap.ts**

```ts
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { HeatmapResponse } from "./types";

export interface HeatmapArgs {
  view: string;
  metric: string;
  from: string;
  to: string;
  buckets?: number;
  top?: number;
}

export function useHeatmap(args: HeatmapArgs) {
  const params = new URLSearchParams({
    view: args.view,
    metric: args.metric,
    from: args.from,
    to: args.to,
  });
  if (args.buckets !== undefined) params.set("buckets", String(args.buckets));
  if (args.top !== undefined) params.set("top", String(args.top));
  const qs = params.toString();
  return useQuery({
    queryKey: ["heatmap", args.view, args.metric, args.from, args.to, args.buckets ?? null, args.top ?? null],
    queryFn: () => apiFetch<HeatmapResponse>(`/v1/timeline/heatmap?${qs}`),
  });
}
```

- [ ] **Step 7: Гейт + коммит + пуш**

Run: `make web-frontend-check && make web-frontend`
Expected: всё зелёное (static пересобран — App.tsx менялся).

```bash
git add web/src/api web/src/App.tsx bins/pg_kronika-web/static
git commit -m "fix(web): API client под реальный контракт main + summary/heatmap hooks"
git push
```

---

### Task 2: TabBar badges из summary

**Files:**
- Create: `web/src/components/TabBadge.tsx`, `web/src/components/TabBadge.test.tsx`
- Modify: `web/src/components/TabBar.tsx`, `web/src/components/TabBar.test.tsx`
- Modify: `web/src/App.tsx` (useSummary + передача в TabBar)
- Modify: `web/src/i18n/ru.json`, `web/src/i18n/en.json` (ключи статусов)

**Interfaces:**
- Consumes: `useSummary` (Task 1), `ViewSpec[]` (catalog).
- Produces: `<TabBar views={ViewSpec[]} active onSelect summaries={Map<string, ViewSummaryItem>} />`; `<TabBadge population={number|null} status={string} notable={boolean} />`.

- [ ] **Step 1: Падающий тест TabBadge**

```tsx
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBadge } from "./TabBadge";

test("renders population for available view", () => {
  render(<TabBadge population={500} status="complete" notable={false} />);
  expect(screen.getByText("500")).toBeDefined();
});

test("renders em-dash for null population (gated)", () => {
  render(<TabBadge population={null} status="unavailable" notable={false} />);
  expect(screen.getByText("—")).toBeDefined();
});

test("notable view gets accent marker", () => {
  const { container } = render(<TabBadge population={3} status="complete" notable={true} />);
  expect(container.querySelector("[data-notable='true']")).not.toBeNull();
});
```

Run: `cd web && npx vitest run src/components/TabBadge.test.tsx` — FAIL.

- [ ] **Step 2: Реализация TabBadge**

```tsx
export function TabBadge(props: { population: number | null; status: string; notable: boolean }) {
  return (
    <span
      data-notable={props.notable}
      style={{
        fontFamily: "var(--mono-font)",
        fontSize: "0.85em",
        color: props.notable
          ? "var(--sev-warn)"
          : props.status === "complete"
            ? "var(--fg-dim)"
            : "var(--sev-crit)",
        marginInlineStart: "4px",
      }}
    >
      {props.population ?? "—"}
    </span>
  );
}
```

- [ ] **Step 3: Интеграция в TabBar и App (TDD: сначала падение теста TabBar на новом prop)**

Обновить `TabBar.test.tsx`: передать `summaries={new Map([["activity", { view: "activity", snapshot_ts_us: "1", population: 142, status: "complete", notable: false }]])}` и ожидать `screen.getByText("142")`. Run — FAIL.

В `TabBar.tsx`: принять `summaries: Map<string, ViewSummaryItem>`, после текста вкладки рендерить `<TabBadge ... />` когда `!gated && summaries.get(v.code)`; для gated — ничего (сам таб уже dimmed).

В `App.tsx`: `const summary = useSummary(state.at ?? latestUs())`, где

```ts
function latestUs(): string {
  return String(Date.now() * 1000);
}
```

и передача `summaries={new Map((summary.data?.views ?? []).map((v) => [v.view, v]))}` в TabBar.

- [ ] **Step 4: i18n не нужен для чисел; гейт + статик + коммит + пуш**

Run: `make web-frontend-check && make web-frontend`

```bash
git add web/src bins/pg_kronika-web/static
git commit -m "feat(web): tab badges с population/status из /v1/views/summary"
git push
```

---

### Task 3: HeatmapStrip

**Files:**
- Create: `web/src/components/HeatmapStrip.tsx`, `web/src/components/HeatmapStrip.test.tsx`, `web/src/components/heatmapColor.ts`, `web/src/components/heatmapColor.test.ts`
- Modify: `web/src/App.tsx` (рендер полосы под TabBar)
- Modify: `web/src/i18n/ru.json`, `web/src/i18n/en.json` (`heatmap.metric`, `heatmap.partial`, `heatmap.empty`)

**Interfaces:**
- Consumes: `useHeatmap` (Task 1), `ViewSpec.metrics` (catalog).
- Produces: `<HeatmapStrip view={ViewSpec} metric={string} from={string} to={string} onMetricChange={(m)=>void} onSelectEntity={(entity:string)=>void} />`; `heatColor(t: number | null): string` — t∈[0,1] нормализованное значение, null → прозрачная «пустая» ячейка.

- [ ] **Step 1: Падающий тест heatmapColor**

```ts
import { expect, test } from "vitest";
import { heatColor } from "./heatmapColor";

test("null is empty cell, zero is the cold end", () => {
  expect(heatColor(null)).toBe("transparent");
  expect(heatColor(0)).not.toBe(heatColor(1));
});

test("monotonic ramp through token stops", () => {
  expect(heatColor(0.25)).toBe("var(--heat-1)");
  expect(heatColor(0.5)).toBe("var(--heat-2)");
  expect(heatColor(0.75)).toBe("var(--heat-3)");
  expect(heatColor(1)).toBe("var(--heat-4)");
});
```

Run — FAIL. Затем реализация: ступенчатая шкала (5 стопов: прозрачный + `--heat-0..4`, токены добавить в `tokens.css` для обеих тем: dark `#1f6feb,#3090ff,#d29922,#f85149,#ff7b72`; light — приглушённые аналоги). Пороги: t<0.2→0, <0.4→1, <0.6→2, <0.8→3, иначе 4.

- [ ] **Step 2: Падающий тест HeatmapStrip**

Фикстура ответа: 2 rows × 4 buckets, values `[[0, 1, null, 4],[2, 2, 2, 2]]`, `quality.status: "partial"`. Ожидания: рендерит label'ы строк, 8 ячеек (`[data-cell]`), ячейка null имеет `data-empty="true"`, badge «partial» присутствует, клик по metric-кнопке зовёт `onMetricChange`, клик по row label зовёт `onSelectEntity` с `entity`.

- [ ] **Step 3: Реализация HeatmapStrip**

- Нормализация: `t = value / max(non-null values)` (max>0; если все null — все ячейки empty).
- Сетка: `div` с `display:grid; grid-template-columns: 160px repeat(bucket_count, 1fr)`, ячейки 12×14px с `background: heatColor(...)`, title=`${label}: ${value ?? "—"}` (форматирование числа — простое `toFixed` если <10, целое иначе; полноценные форматтеры — позже).
- Metric switcher: кнопки из `view.metrics.filter(m => m.availability === "available")`, активная — accent underline, i18n `heatmap.metric`.
- quality.status !== "complete" → badge `t("heatmap.partial")` цвета `--sev-warn`.
- rows.length === 0 → текст `t("heatmap.empty")`.

- [ ] **Step 4: Интеграция в App + i18n-ключи + гейт + статик + коммит + пуш**

В `App.tsx`: под TabBar рендерить `<HeatmapStrip>` для активного view: `from = String((Date.now() - 86400_000) * 1000)`, `to = String(Date.now() * 1000)`, metric из локального state (default — `canonical_metric` view), onMetricChange → setState. i18n: `heatmap.metric: "Метрика"/"Metric"`, `heatmap.partial: "данные неполные"/"partial data"`, `heatmap.empty: "нет данных за диапазон"/"no data in range"` (паритет ключей — тест). Run: `make web-frontend-check && make web-frontend`

```bash
git add web/src bins/pg_kronika-web/static
git commit -m "feat(web): heatmap strip из /v1/timeline/heatmap с metric switcher"
git push
```

---

### Task 4: Демо-стаб + скриншоты (dark/light)

**Files:**
- Create: `web/scripts/demo-stub.mjs` (static + `/v1/ui/catalog`, `/v1/views/summary`, `/v1/timeline/heatmap` с богатой фикстурой: 9 views, populations, 8 rows × 56 buckets с «горячим» пиком и гэпом)
- Create: `web/scripts/demo-shot.mjs` (puppeteer-core, оба theme, viewport 1600×900)
- Modify: `web/package.json` (devDep `puppeteer-core`, scripts `demo:stub`, `demo:shot`)

**Interfaces:**
- Produces: `npm run demo:stub` (порт 18444), `npm run demo:shot` → `web/demo/shots/{dark,light}.png` (gitignore `web/demo/`).

- [ ] **Step 1: Стаб**

Каталог: переиспользовать реальный ответ `GET /v1/ui/catalog` живого бинарника (сохранить как фикстуру в stub-файле inline) — но со всеми views `"available"` и metrics у statements (`time, calls, io, temp`). Summary: populations 142/500/83/64/121/2/218/3/5, statuses complete. Heatmap: детерминированный PRNG (seed 42), 8 entities × 56 buckets, синус + пик около bucket 40, один гэп (buckets 20–22 null у всех) и `quality.gaps` соответствующий, `status: "partial"`.

- [ ] **Step 2: Скриншот-скрипт**

puppeteer-core, executablePath `process.env.CHROME ?? "/usr/bin/chromium-browser"`, localStorage `pgk-theme` dark/light, waitUntil networkidle0 + 1с, сохранить PNG. Run: `npm run demo:stub & sleep 1 && npm run demo:shot` — оба файла создаются, непустые (>10KB).

- [ ] **Step 3: Гейт + коммит + пуш**

```bash
git add web/scripts web/package.json web/package-lock.json .gitignore
git commit -m "chore(web): demo stub + screenshot harness"
git push
```

---

## Self-Review notes

- Контракт проверен против живого бинарника (curl `/v1/ui/catalog`, `/v1/views/summary`, `/v1/timeline/heatmap` 2026-07-31) и `ui/handlers.rs` сигнатур; поле `unbounded_segments` есть только у heatmap quality.
- Расхождение со спекой (спека требует `source` обязательным, реализация single-source) — зафиксировано здесь; при появлении multi-source вернём параметр и в URL-state, и в клиент.
- Spine (кривая суток + события + baseline) — НЕ в этом плане: это step 3 вместе с `/v1/timeline/events` и `/v1/timeline/health`.
- Известные хвосты из scaffold (useUiState, hashchange, --gap токен) — не трогаем здесь, отдельный polish-таск позже.
