# Web UI v6 — Scaffold & Catalog Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Создать фронтенд-проект `web/` (React 19 + Vite + TS), встроить его сборку в all-in-one бинарник `pg_kronika-web` через существующий rust-embed пайплайн, добавить frontend-гейт в CI и поднять каталог-driven каркас приложения (темы, i18n, API-клиент, TabBar из `/v1/ui/catalog`).

**Architecture:** Vite собирает SPA в `bins/pg_kronika-web/static/` (существующий `handlers/static_.rs` уже встраивает эту директорию через rust-embed и делает SPA-fallback на `index.html` — не трогаем). Фронт полностью catalog-driven: вкладки, колонки и пресеты приходят из `/v1/ui/catalog`, клиент не зашивает секции и формулы. Темы (dark/light) на CSS custom properties через `data-theme`; i18n (ru/en) на i18next с первого коммита.

**Tech Stack:** React 19, Vite 7, TypeScript strict, TanStack Query 5, i18next + react-i18next, vitest, eslint strict.

**Спека:** `docs/superpowers/specs/2026-07-30-web-ui-v6-design.md` (PR #147).

## Global Constraints

- Репозиторий: собственный клон агента; ветка `feat/web-ui-v6-scaffold` от `main`. Коммиты атомарные, conventional commits.
- Rust-гейт не ломаем: `cargo +1.96.0 fmt --all --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` должны оставаться зелёными после каждого таска.
- Никаких hex-цветов в компонентах — только токены из `web/src/design/tokens.css`.
- Никаких хардкод-строк в JSX — только `t()` из i18n. Словари `ru` и `en` обязаны иметь одинаковый набор ключей.
- `web/dist` в git не коммитим; `bins/pg_kronika-web/static/` — коммитим (результат сборки, как сейчас placeholder `index.html`).
- Node/npm появляется только в `web/` и CI; `flake.nix` обновляем, чтобы `cargo build` в nix-окружении видел свежий `static/` (сама сборка фронта в flake не интегрируется в этом плане — договорённость спеки: `vite build` отдельным шагом).

---

### Task 1: Scaffold `web/` + сборка в `static/` + Makefile

**Files:**
- Create: `web/package.json`
- Create: `web/tsconfig.json`
- Create: `web/vite.config.ts`
- Create: `web/index.html`
- Create: `web/src/main.tsx`
- Create: `web/src/App.tsx`
- Create: `web/src/App.test.tsx`
- Create: `web/vitest.config.ts`
- Modify: `Makefile` (новые цели)
- Modify: `.gitignore` (node_modules, dist)

**Interfaces:**
- Produces: `make web-frontend` — устанавливает зависимости и собирает SPA в `bins/pg_kronika-web/static/`; `make web-frontend-check` — tsc + lint + tests без сборки.

- [ ] **Step 1: Создать каркас пакета**

`web/package.json`:

```json
{
  "name": "pgkronika-web-ui",
  "private": true,
  "version": "0.0.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "test": "vitest run",
    "lint": "eslint . --max-warnings 0",
    "typecheck": "tsc --noEmit"
  },
  "dependencies": {
    "react": "^19.1.0",
    "react-dom": "^19.1.0"
  },
  "devDependencies": {
    "@testing-library/react": "^16.3.0",
    "@types/react": "^19.1.0",
    "@types/react-dom": "^19.1.0",
    "@vitejs/plugin-react": "^5.0.0",
    "jsdom": "^26.1.0",
    "typescript": "~5.9.2",
    "vite": "^7.1.0",
    "vitest": "^3.2.0"
  }
}
```

`web/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "skipLibCheck": true,
    "isolatedModules": true,
    "noEmit": true
  },
  "include": ["src"]
}
```

`web/vite.config.ts`:

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "../bins/pg_kronika-web/static",
    emptyOutDir: true,
    sourcemap: true,
  },
  server: {
    proxy: { "/v1": "http://127.0.0.1:8080" },
  },
});
```

`web/vitest.config.ts`:

```ts
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: { environment: "jsdom", globals: true },
});
```

`web/index.html`:

```html
<!doctype html>
<html lang="ru" data-theme="dark">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>PgKronika</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

`web/src/main.tsx`:

```tsx
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
```

- [ ] **Step 2: Написать падающий smoke-тест**

`web/src/App.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { App } from "./App";

test("renders app shell placeholder", () => {
  render(<App />);
  expect(screen.getByTestId("app-shell")).toBeDefined();
});
```

- [ ] **Step 3: Запустить тест, убедиться в падении**

Run: `cd web && npm install && npx vitest run`
Expected: FAIL — `App` не экспортирует ничего (нет `web/src/App.tsx`).

- [ ] **Step 4: Минимальная реализация**

`web/src/App.tsx`:

```tsx
export function App() {
  return <div data-testid="app-shell">PgKronika</div>;
}
```

- [ ] **Step 5: Тест зелёный**

Run: `cd web && npx vitest run`
Expected: PASS (1 test).

- [ ] **Step 6: Makefile и .gitignore**

В `Makefile` добавить (рядом с целью `web`):

```make
web-frontend: ## Install and build the SPA into bins/pg_kronika-web/static.
	cd web && npm ci && npm run build

web-frontend-check: ## Typecheck, lint and test the SPA without building.
	cd web && npm ci && npm run typecheck && npm run lint && npm run test
```

В `.gitignore` добавить:

```
web/node_modules/
web/dist/
```

- [ ] **Step 7: Сборка и проверка embed end-to-end**

Run:

```bash
make web-frontend
cargo +1.96.0 build -p pg_kronika-web
ls bins/pg_kronika-web/static/
```

Expected: в `static/` лежит `index.html` и `assets/*.js|css` с хэшами; бинарник собрался без изменений Rust-кода (rust-embed подхватывает директорию).

- [ ] **Step 8: Коммит**

```bash
git add web/ Makefile .gitignore bins/pg_kronika-web/static/
git commit -m "feat(web): scaffold React+Vite+TS SPA embedded via rust-embed"
```

---

### Task 2: CI frontend-гейт

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `make web-frontend-check`, `make web-frontend` (Task 1).
- Produces: job `frontend` в CI, блокирующий merge при красном гейте.

- [ ] **Step 1: Добавить job в ci.yml**

В `.github/workflows/ci.yml`, после job `fmt + clippy`:

```yaml
  frontend:
    name: frontend
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm
          cache-dependency-path: web/package-lock.json
      - run: make web-frontend-check
      - run: make web-frontend
```

- [ ] **Step 2: Проверить локально, что обе цели проходят**

Run: `make web-frontend-check && make web-frontend`
Expected: exit 0.

- [ ] **Step 3: Коммит**

```bash
git add .github/workflows/ci.yml web/package-lock.json
git commit -m "ci: frontend gate (tsc, eslint, vitest, vite build)"
```

---

### Task 3: Дизайн-токены, темы dark/light

**Files:**
- Create: `web/src/design/tokens.css`
- Create: `web/src/design/theme.ts`
- Create: `web/src/design/theme.test.ts`
- Modify: `web/src/main.tsx` (импорт tokens.css, установка темы)

**Interfaces:**
- Produces: `resolveTheme(): "dark" | "light"`, `applyTheme(theme)`, `toggleTheme()`; CSS-токены `--bg`, `--fg`, `--sev-ok`, `--sev-warn`, `--sev-crit`, `--accent`, `--delta-pos`, `--delta-neg`, `--mono-font` под `[data-theme="dark"]` и `[data-theme="light"]`.

- [ ] **Step 1: Падающий тест**

`web/src/design/theme.test.ts`:

```ts
import { expect, test, vi } from "vitest";
import { applyTheme, resolveTheme } from "./theme";

test("defaults to system preference", () => {
  window.matchMedia = vi.fn().mockReturnValue({ matches: true }) as never;
  localStorage.clear();
  expect(resolveTheme()).toBe("dark");
});

test("manual choice wins over system", () => {
  localStorage.setItem("pgk-theme", "light");
  expect(resolveTheme()).toBe("light");
});

test("applyTheme sets data-theme on documentElement", () => {
  applyTheme("light");
  expect(document.documentElement.dataset.theme).toBe("light");
});
```

- [ ] **Step 2: Запустить, убедиться в падении**

Run: `cd web && npx vitest run src/design/theme.test.ts`
Expected: FAIL — модуль не существует.

- [ ] **Step 3: Реализация theme.ts**

`web/src/design/theme.ts`:

```ts
export type Theme = "dark" | "light";
const KEY = "pgk-theme";

export function resolveTheme(): Theme {
  const saved = localStorage.getItem(KEY);
  if (saved === "dark" || saved === "light") return saved;
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem(KEY, theme);
}
```

- [ ] **Step 4: Токены**

`web/src/design/tokens.css`:

```css
:root {
  --mono-font: "JetBrains Mono", ui-monospace, monospace;
  --ui-font: "Inter", system-ui, sans-serif;
}
[data-theme="dark"] {
  --bg: #0d1117;
  --bg-raised: #161b22;
  --border: #30363d;
  --fg: #c9d1d9;
  --fg-dim: #8b949e;
  --sev-ok: #3fb950;
  --sev-warn: #d29922;
  --sev-crit: #f85149;
  --accent: #58a6ff;
  --delta-pos: #3fb950;
  --delta-neg: #f85149;
}
[data-theme="light"] {
  --bg: #ffffff;
  --bg-raised: #f6f8fa;
  --border: #d0d7de;
  --fg: #1f2328;
  --fg-dim: #59636e;
  --sev-ok: #1a7f37;
  --sev-warn: #9a6700;
  --sev-crit: #cf222e;
  --accent: #0969da;
  --delta-pos: #1a7f37;
  --delta-neg: #cf222e;
}
```

- [ ] **Step 5: Подключить в main.tsx и прогнать тесты**

В `web/src/main.tsx` перед рендером:

```tsx
import "./design/tokens.css";
import { applyTheme, resolveTheme } from "./design/theme";

applyTheme(resolveTheme());
```

Run: `cd web && npx vitest run`
Expected: PASS (все тесты).

- [ ] **Step 6: Коммит**

```bash
git add web/src/design/ web/src/main.tsx
git commit -m "feat(web): design tokens with dark/light themes"
```

---

### Task 4: i18n (ru/en) с проверкой паритета ключей

**Files:**
- Create: `web/src/i18n/index.ts`
- Create: `web/src/i18n/ru.json`
- Create: `web/src/i18n/en.json`
- Create: `web/src/i18n/parity.test.ts`
- Modify: `web/src/main.tsx` (инициализация), `web/package.json` (зависимости)

**Interfaces:**
- Produces: инициализированный i18next, `useTranslation()` доступен в любом компоненте; тест паритета гарантирует одинаковые ключи словарей.

- [ ] **Step 1: Зависимости и падающий тест паритета**

Run: `cd web && npm install i18next react-i18next i18next-browser-languagedetector`

`web/src/i18n/parity.test.ts`:

```ts
import { expect, test } from "vitest";
import en from "./en.json";
import ru from "./ru.json";

test("ru and en dictionaries have identical keys", () => {
  expect(Object.keys(ru).sort()).toEqual(Object.keys(en).sort());
});
```

Run: `cd web && npx vitest run src/i18n/parity.test.ts`
Expected: FAIL — файлов словарей нет.

- [ ] **Step 2: Словари и инициализация**

`web/src/i18n/ru.json`:

```json
{
  "app.title": "PgKronika",
  "tabs.activity": "Активность",
  "tabs.statements": "Запросы",
  "tabs.plans": "Планы",
  "tabs.tables": "Таблицы",
  "tabs.indexes": "Индексы",
  "tabs.vacuum": "Vacuum",
  "tabs.processes": "Процессы",
  "tabs.locks": "Блокировки",
  "tabs.events": "События"
}
```

`web/src/i18n/en.json`:

```json
{
  "app.title": "PgKronika",
  "tabs.activity": "Activity",
  "tabs.statements": "Statements",
  "tabs.plans": "Plans",
  "tabs.tables": "Tables",
  "tabs.indexes": "Indexes",
  "tabs.vacuum": "Vacuum",
  "tabs.processes": "Processes",
  "tabs.locks": "Locks",
  "tabs.events": "Events"
}
```

`web/src/i18n/index.ts`:

```ts
import i18n from "i18next";
import LanguageDetector from "i18next-browser-languagedetector";
import { initReactI18next } from "react-i18next";
import en from "./en.json";
import ru from "./ru.json";

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: { ru: { translation: ru }, en: { translation: en } },
    fallbackLng: "en",
    supportedLngs: ["ru", "en"],
    interpolation: { escapeValue: false },
  });

export default i18n;
```

- [ ] **Step 3: Подключить и прогнать**

В `web/src/main.tsx` добавить `import "./i18n";`.

Run: `cd web && npx vitest run`
Expected: PASS.

- [ ] **Step 4: Коммит**

```bash
git add web/src/i18n/ web/package.json web/package-lock.json web/src/main.tsx
git commit -m "feat(web): i18n ru/en with dictionary parity test"
```

---

### Task 5: Типизированный API-клиент + catalog hook

**Files:**
- Create: `web/src/api/types.ts`
- Create: `web/src/api/client.ts`
- Create: `web/src/api/client.test.ts`
- Create: `web/src/api/catalog.ts`
- Create: `web/src/api/catalog.test.ts`
- Modify: `web/package.json` (+ @tanstack/react-query)

**Interfaces:**
- Produces:
  - `class ApiError extends Error { code: string; status: number }`
  - `apiFetch<T>(path: string): Promise<T>` — GET, маппит `application/problem+json` в `ApiError`.
  - Типы `ProjectionCatalog`, `ViewSpec`, `MetricSpec`, `ColumnSpec`, `PresetSpec`, `Availability` (ровно по `bins/pg_kronika-web/src/ui/catalog.rs`).
  - `useCatalog(source: string)` — TanStack Query hook, ключ `["catalog", source]`, `staleTime: Infinity`.

- [ ] **Step 1: Зависимость**

Run: `cd web && npm install @tanstack/react-query`

- [ ] **Step 2: Падающий тест клиента**

`web/src/api/client.test.ts`:

```ts
import { afterEach, expect, test, vi } from "vitest";
import { ApiError, apiFetch } from "./client";

afterEach(() => vi.unstubAllGlobals());

test("returns parsed json on 200", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"revision":1}', { status: 200 }),
  ));
  await expect(apiFetch<{ revision: number }>("/v1/ui/catalog?source=x"))
    .resolves.toEqual({ revision: 1 });
});

test("maps problem+json to ApiError", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"code":"unknown_source","title":"no such source"}', {
      status: 404,
      headers: { "content-type": "application/problem+json" },
    }),
  ));
  const err = await apiFetch("/v1/x").catch((e: unknown) => e);
  expect(err).toBeInstanceOf(ApiError);
  expect((err as ApiError).code).toBe("unknown_source");
});
```

Run: `cd web && npx vitest run src/api/client.test.ts`
Expected: FAIL — модуля нет.

- [ ] **Step 3: Реализация клиента**

`web/src/api/client.ts`:

```ts
export class ApiError extends Error {
  constructor(
    public readonly code: string,
    public readonly status: number,
    detail?: string,
  ) {
    super(detail ?? code);
    this.name = "ApiError";
  }
}

interface ProblemJson {
  code?: string;
  title?: string;
  detail?: string;
}

export async function apiFetch<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: "application/json" } });
  if (!res.ok) {
    let problem: ProblemJson = {};
    if (res.headers.get("content-type")?.includes("problem+json")) {
      problem = (await res.json()) as ProblemJson;
    }
    throw new ApiError(
      problem.code ?? "http_error",
      res.status,
      problem.detail ?? problem.title,
    );
  }
  return (await res.json()) as T;
}
```

- [ ] **Step 4: Типы каталога (зеркало `ui/catalog.rs`)**

`web/src/api/types.ts`:

```ts
export type Availability =
  | "available"
  | "gated"
  | "not_collected"
  | "unsupported_type";
export type Scope = "database" | "host" | "instance";
export type ValueType =
  | "i64" | "u64" | "f64" | "bool" | "text" | "timestamp";

export interface MetricSpec {
  code: string;
  revision: number;
  unit: string;
  aggregation: string;
  formula: string;
  requires: string[];
  availability: Availability;
}

export interface ColumnSpec {
  code: string;
  type: ValueType;
  source?: string;
  formula?: string;
  unit?: string;
  threshold_metric?: string;
  lazy: boolean;
  requires: string[];
  availability: Availability;
}

export interface PresetSpec {
  code: string;
  columns: string[];
  sort: { column: string; order: "asc" | "desc" };
}

export interface ViewSpec {
  view_code: number;
  code: string;
  view_revision: number;
  scope: Scope;
  identity_revision: number;
  availability: Availability;
  inputs: unknown[];
  joins: unknown[];
  metrics: MetricSpec[];
  columns: ColumnSpec[];
  presets: PresetSpec[];
  canonical_metric: string;
}

export interface ProjectionCatalog {
  revision: number;
  views: ViewSpec[];
}
```

- [ ] **Step 5: Падающий тест хука**

`web/src/api/catalog.test.ts`:

```ts
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { useCatalog } from "./catalog";

afterEach(() => vi.unstubAllGlobals());

test("useCatalog fetches catalog for source", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"revision":1,"views":[]}', { status: 200 }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  const { result } = renderHook(() => useCatalog("local"), { wrapper });
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(result.current.data?.revision).toBe(1);
  expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe(
    "/v1/ui/catalog?source=local",
  );
});
```

Run: `cd web && npx vitest run src/api/catalog.test.ts`
Expected: FAIL — модуля нет.

- [ ] **Step 6: Реализация хука**

`web/src/api/catalog.ts`:

```ts
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { ProjectionCatalog } from "./types";

export function useCatalog(source: string) {
  return useQuery({
    queryKey: ["catalog", source],
    queryFn: () =>
      apiFetch<ProjectionCatalog>(
        `/v1/ui/catalog?source=${encodeURIComponent(source)}`,
      ),
    staleTime: Infinity,
  });
}
```

- [ ] **Step 7: Тесты зелёные + коммит**

Run: `cd web && npx vitest run`
Expected: PASS.

```bash
git add web/src/api/ web/package.json web/package-lock.json
git commit -m "feat(web): typed API client and catalog hook"
```

---

### Task 6: AppShell + URL-state + каталог-driven TabBar

**Files:**
- Create: `web/src/state/url.ts`
- Create: `web/src/state/url.test.ts`
- Create: `web/src/components/TabBar.tsx`
- Create: `web/src/components/TabBar.test.tsx`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `useCatalog` (Task 5), i18n ключи `tabs.*` (Task 4).
- Produces:
  - `interface UiState { source: string; view: string; at: string | null }`
  - `parseHash(hash: string): UiState`, `toHash(state: UiState): string`, `useUiState(): [UiState, (patch: Partial<UiState>) => void]`
  - `<TabBar views={ViewSpec[]} active={string} onSelect={code}/>` — рендерит вкладки из каталога, недоступные (`availability !== "available"`) помечает dimmed.

- [ ] **Step 1: Падающий тест URL-state**

`web/src/state/url.test.ts`:

```ts
import { expect, test } from "vitest";
import { parseHash, toHash } from "./url";

test("roundtrips state", () => {
  const state = { source: "local", view: "statements", at: "1722400000000000" };
  expect(parseHash(toHash(state))).toEqual(state);
});

test("defaults when hash empty", () => {
  expect(parseHash("")).toEqual({ source: "local", view: "activity", at: null });
});
```

Run: `cd web && npx vitest run src/state/url.test.ts`
Expected: FAIL.

- [ ] **Step 2: Реализация url.ts**

`web/src/state/url.ts`:

```ts
export interface UiState {
  source: string;
  view: string;
  at: string | null;
}

export function parseHash(hash: string): UiState {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  return {
    source: params.get("source") ?? "local",
    view: params.get("view") ?? "activity",
    at: params.get("at"),
  };
}

export function toHash(state: UiState): string {
  const params = new URLSearchParams();
  params.set("source", state.source);
  params.set("view", state.view);
  if (state.at !== null) params.set("at", state.at);
  return `#${params.toString()}`;
}
```

- [ ] **Step 3: Падающий тест TabBar**

`web/src/components/TabBar.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBar } from "./TabBar";
import type { ViewSpec } from "../api/types";

const views = [
  { code: "activity", availability: "available" },
  { code: "statements", availability: "gated" },
] as unknown as ViewSpec[];

test("renders one tab per catalog view, gated dimmed", () => {
  render(<TabBar views={views} active="activity" onSelect={() => {}} />);
  expect(screen.getByRole("tab", { name: /activity/i })).toBeDefined();
  const gated = screen.getByRole("tab", { name: /statements/i });
  expect(gated.getAttribute("aria-disabled")).toBe("true");
});
```

Run: `cd web && npx vitest run src/components/TabBar.test.tsx`
Expected: FAIL.

- [ ] **Step 4: Реализация TabBar**

`web/src/components/TabBar.tsx`:

```tsx
import { useTranslation } from "react-i18next";
import type { ViewSpec } from "../api/types";

export function TabBar(props: {
  views: ViewSpec[];
  active: string;
  onSelect: (code: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <div role="tablist" style={{ display: "flex", gap: "var(--gap, 4px)" }}>
      {props.views.map((v) => {
        const gated = v.availability !== "available";
        return (
          <button
            key={v.code}
            role="tab"
            aria-selected={props.active === v.code}
            aria-disabled={gated}
            style={{
              fontFamily: "var(--mono-font)",
              color: gated
                ? "var(--fg-dim)"
                : props.active === v.code
                  ? "var(--accent)"
                  : "var(--fg)",
              background: "none",
              border: "none",
              borderBottom:
                props.active === v.code
                  ? "2px solid var(--accent)"
                  : "2px solid transparent",
              cursor: gated ? "default" : "pointer",
            }}
            onClick={() => !gated && props.onSelect(v.code)}
          >
            {t(`tabs.${v.code}`)}
          </button>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 5: Собрать App**

`web/src/App.tsx`:

```tsx
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCatalog } from "./api/catalog";
import { TabBar } from "./components/TabBar";
import { parseHash, toHash } from "./state/url";

const queryClient = new QueryClient();

function Shell() {
  const { t } = useTranslation();
  const [state, setState] = useState(() => parseHash(location.hash));
  const catalog = useCatalog(state.source);

  const patch = (p: Partial<typeof state>) => {
    const next = { ...state, ...p };
    setState(next);
    location.hash = toHash(next);
  };

  return (
    <div data-testid="app-shell" style={{ background: "var(--bg)", color: "var(--fg)", minHeight: "100dvh" }}>
      <header style={{ fontFamily: "var(--mono-font)", padding: "4px 8px" }}>
        {t("app.title")} · {state.source}
      </header>
      {catalog.isSuccess && (
        <TabBar views={catalog.data.views} active={state.view} onSelect={(view) => patch({ view })} />
      )}
    </div>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <Shell />
    </QueryClientProvider>
  );
}
```

Обновить `web/src/App.test.tsx` (fetch замокан, каталог пуст):

```tsx
import { render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "./App";

afterEach(() => vi.unstubAllGlobals());

test("renders app shell placeholder", () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"revision":1,"views":[]}', { status: 200 }),
  ));
  render(<App />);
  expect(screen.getByTestId("app-shell")).toBeDefined();
});
```

- [ ] **Step 6: Все тесты + гейт + сборка**

Run: `make web-frontend-check && make web-frontend && cargo +1.96.0 build -p pg_kronika-web`
Expected: всё зелёное; бинарник содержит SPA.

- [ ] **Step 7: Коммит**

```bash
git add web/src/
git commit -m "feat(web): app shell with catalog-driven tab bar and URL state"
```

---

## Self-Review notes

- Покрытие спеки (шаг 1 порядка реализации): каркас+embed+CI (Tasks 1–2), темы (Task 3), i18n (Task 4), каталог → динамические вкладки (Tasks 5–6). Spine/heatmap/frame — следующие планы, как и договорено в спеке.
- Типы `MetricSpec/ColumnSpec/ViewSpec` сверены с `bins/pg_kronika-web/src/ui/catalog.rs` построчно (имена полей, `type` вместо `value_type`, optional-поля).
- `inputs`/`joins` в `ViewSpec` сознательно `unknown[]` — клиент их не интерпретирует на этом шаге; типизируются, когда появится потребитель.
