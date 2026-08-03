import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { HeatmapResponse } from "../api/types";
import {
  makeEntityPointResponse,
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeHeatmapQuality,
  makeMetricSpec,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { ActivityWorkspace } from "./ActivityWorkspace";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function requestUrl(input: RequestInfo | URL): URL {
  return new URL(
    typeof input === "string"
      ? input
      : input instanceof Request
        ? input.url
        : input.href,
    "http://localhost",
  );
}

function Wrapper({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      {children}
    </QueryClientProvider>
  );
}

const activityView = makeViewSpec({
  code: "activity",
  canonical_metric: "active_fraction",
  joins: [
    {
      left: "activity",
      right: "process",
      kind: "best_effort",
      fields: ["pid"],
      cardinality: "zero_or_many",
      provenance: "pid",
    },
  ],
  metrics: [
    makeMetricSpec({ code: "active_fraction", unit: "ratio" }),
    makeMetricSpec({ code: "cpu", unit: "ratio" }),
  ],
  columns: [
    {
      availability: "available",
      code: "pid",
      lazy: false,
      requires: ["activity"],
      type: "i64",
    },
    {
      availability: "available",
      code: "database",
      lazy: false,
      requires: ["activity"],
      type: "text",
    },
    {
      availability: "available",
      code: "user",
      lazy: false,
      requires: ["activity"],
      type: "text",
    },
    {
      availability: "available",
      code: "application",
      lazy: false,
      requires: ["activity"],
      type: "text",
    },
    {
      availability: "available",
      code: "state",
      lazy: false,
      requires: ["activity"],
      type: "text",
    },
    {
      availability: "available",
      code: "process_link",
      lazy: false,
      requires: ["activity", "process"],
      type: "text",
    },
    {
      availability: "available",
      code: "cpu",
      lazy: false,
      requires: ["activity", "process"],
      type: "f64",
    },
  ],
  presets: [
    {
      code: "overview",
      columns: [
        "pid",
        "database",
        "user",
        "application",
        "state",
        "process_link",
        "cpu",
      ],
      sort: { column: "cpu", order: "desc" },
    },
    {
      code: "waits_locks",
      columns: ["pid", "database", "user", "state"],
      sort: { column: "pid", order: "desc" },
    },
  ],
});

const activityFrame = makeFrameResponse({
  view: "activity",
  columns: [
    makeFrameColumn({ code: "pid", type: "i64" }),
    makeFrameColumn({ code: "database", type: "text" }),
    makeFrameColumn({ code: "user", type: "text" }),
    makeFrameColumn({ code: "application", type: "text" }),
    makeFrameColumn({ code: "state", type: "text" }),
    makeFrameColumn({ code: "process_link", type: "text" }),
    makeFrameColumn({ code: "cpu", type: "f64" }),
  ],
  rows: [
    makeFrameRow({
      entity: "pid:18422",
      label: "api / erp_prod",
      cells: [18422, "erp_prod", "api", "web", "active", "pid", 0.82],
    }),
    makeFrameRow({
      entity: "pid:19041",
      label: "web / erp_prod",
      cells: [
        19041,
        "erp_prod",
        "web",
        "psql",
        "idle in transaction",
        null,
        null,
      ],
    }),
  ],
  page: { matched: 28, returned: 2 },
});

const heatmap: HeatmapResponse = {
  grid: { from_us: "100", to_us: "200", bucket_count: 96 },
  ranking: { exact: true, unseen_upper: 0 },
  quality: makeHeatmapQuality({ snapshots: 60 }),
  rows: [
    {
      entity: "pid:18422",
      label: "pid 18422",
      unit: "ratio",
      score: { lower: 0, upper: 1 },
      values: Array.from({ length: 96 }, (_, index) =>
        index % 5 === 0 ? null : index / 96,
      ),
    },
  ],
};

const locksRootFrame = makeFrameResponse({
  view: "locks",
  columns: [
    makeFrameColumn({ code: "pid", type: "i64" }),
    makeFrameColumn({ code: "blocked_by", type: "text" }),
    makeFrameColumn({ code: "wait_age_us", type: "f64" }),
    makeFrameColumn({ code: "target", type: "text" }),
  ],
  rows: [
    makeFrameRow({
      entity: "lock:root",
      label: "pid 19041",
      cells: [19041, "", null, "—"],
    }),
    makeFrameRow({
      entity: "lock:root-two",
      label: "pid 19042",
      cells: [19042, "", null, "—"],
    }),
  ],
  page: { matched: 6, returned: 2, next: "locks-page-2" },
});

const locksEdgeFrame = makeFrameResponse({
  view: "locks",
  columns: locksRootFrame.columns,
  rows: [
    makeFrameRow({
      entity: "lock:18422",
      label: "pid 18422",
      cells: [18422, "19041, 0", 12_400_000, "public.orders"],
    }),
    makeFrameRow({
      entity: "lock:18425",
      label: "pid 18425",
      cells: [18425, "19043, 19044", 8_000_000, "public.accounts"],
    }),
  ],
  page: { matched: 6, returned: 2 },
});

function stubRequests() {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = requestUrl(input);
      if (url.pathname === "/v1/timeline/heatmap")
        return Promise.resolve(json(heatmap));
      if (url.pathname === "/v1/frame/locks") {
        return Promise.resolve(
          json(
            url.searchParams.get("cursor") === "locks-page-2"
              ? locksEdgeFrame
              : locksRootFrame,
          ),
        );
      }
      if (url.pathname.startsWith("/v1/entity/activity/")) {
        return Promise.resolve(
          json(
            makeEntityPointResponse({
              view: "activity",
              entity: "pid:18422",
              related: [
                {
                  view: "processes",
                  entity: "proc:18422",
                  relation: "activity_process",
                  snapshot_ts_us: "1722400000000000",
                  provenance: {
                    kind: "best_effort",
                    method: "pid",
                    fields: ["pid"],
                  },
                },
              ],
            }),
          ),
        );
      }
      return Promise.resolve(json(activityFrame));
    }),
  );
}

function renderWorkspace(
  preset: "overview" | "waits_locks",
  onOpenEntity: (view: string, entity: string) => void = () => {},
) {
  return render(
    <ActivityWorkspace
      view={activityView}
      at="150"
      span={3_600}
      from="100"
      to="200"
      metric="active_fraction"
      baselineUs={null}
      preset={preset}
      q={null}
      sort={null}
      order={null}
      entity={null}
      matched={28}
      mobile={false}
      onMetricChange={() => {}}
      onSort={() => {}}
      onSelectRow={() => {}}
      onOpenEntity={onOpenEntity}
      onMatched={() => {}}
    />,
    { wrapper: Wrapper },
  );
}

test("builds the joined Activity snapshot by default and keeps PID links calm", async () => {
  stubRequests();
  const onOpenEntity = vi.fn();
  renderWorkspace("overview", onOpenEntity);

  expect(await screen.findByTestId("activity-snapshot-table")).toBeDefined();
  expect(screen.getByTestId("activity-point-evidence")).toBeDefined();
  expect(screen.getByTestId("activity-process-link").textContent).toContain(
    "relation.activityProcess.pid",
  );
  const workspace = screen.getByTestId("activity-workspace");
  expect(workspace.textContent).not.toMatch(
    /best_effort|edge.only|point.snapshot|series \d+ \/|gaps|gated/i,
  );
  expect(screen.queryByTestId("activity-detached-heatmap")).toBeNull();
  expect(screen.queryByTestId("activity-sample-row")).toBeNull();
  expect(screen.queryByTestId("time-matrix-bucket")).toBeNull();
  const table = screen.getByRole("table", { name: "activity" });
  await waitFor(() => expect(table.getAttribute("aria-rowcount")).toBe("30"));
  expect(
    table
      .querySelector('[data-entity="pid:18422"]')
      ?.getAttribute("aria-rowindex"),
  ).toBe("3");
  expect(
    table.querySelector('[data-evidence-group="relation"]'),
  ).not.toBeNull();
  expect(table.querySelector('[data-evidence-group="os"]')).not.toBeNull();

  screen.getByTestId("activity-process-link-cell").click();
  await waitFor(() =>
    expect(onOpenEntity).toHaveBeenCalledWith("processes", "proc:18422"),
  );
  const relationCall = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => requestUrl(input))
    .find((url) => url.pathname.startsWith("/v1/entity/activity/"));
  expect(relationCall?.searchParams.get("include")).toBe("related");

  const heatmapCall = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => requestUrl(input))
    .find((url) => url.pathname === "/v1/timeline/heatmap");
  expect(heatmapCall).toBeUndefined();
});

test("adds compact waiter to blocker relations only to Waits & Locks", async () => {
  stubRequests();
  const onOpenEntity = vi.fn();
  renderWorkspace("waits_locks", onOpenEntity);

  const strip = await screen.findByTestId("activity-lock-evidence");
  expect(screen.getByTestId("activity-time-matrix")).toBeDefined();
  await waitFor(() =>
    expect(screen.getAllByTestId("time-matrix-bucket")).toHaveLength(2 * 96),
  );
  expect(strip.getAttribute("data-provenance")).toBeNull();
  expect(strip.querySelector(".activity-lock-evidence__badge")).toBeNull();
  expect(
    await screen.findByRole("button", { name: /18422.*19041/ }),
  ).toBeDefined();
  expect(screen.getByRole("button", { name: /^18422 → 0 ·/ })).toBeDefined();
  expect(screen.getByRole("button", { name: /18425.*19043/ })).toBeDefined();
  expect(strip.querySelectorAll("button")).toHaveLength(3);
  expect(strip.textContent).not.toContain("19041 → —");
  expect(strip.textContent).toContain("18422 → 0");
  expect(strip.textContent).not.toContain("19044");
  screen.getByRole("button", { name: /^18422 → 0 ·/ }).click();
  expect(onOpenEntity).toHaveBeenCalledWith("locks", "lock:18422");
  await waitFor(() => {
    const lockCalls = vi
      .mocked(fetch)
      .mock.calls.map(([input]) => requestUrl(input))
      .filter((url) => url.pathname === "/v1/frame/locks");
    expect(lockCalls[0]?.searchParams.get("limit")).toBe("16");
    expect(lockCalls[0]?.searchParams.get("preset")).toBe("tree");
    expect(
      lockCalls.some(
        (url) => url.searchParams.get("cursor") === "locks-page-2",
      ),
    ).toBe(true);
  });
});

test("stops lock continuation pagination on error and exposes an explicit retry", async () => {
  let continuationAttempts = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = requestUrl(input);
      if (url.pathname === "/v1/timeline/heatmap")
        return Promise.resolve(json(heatmap));
      if (url.pathname === "/v1/frame/locks") {
        if (url.searchParams.get("cursor") === "locks-page-2") {
          continuationAttempts += 1;
          return Promise.resolve(
            new Response(JSON.stringify({ code: "continuation_failed" }), {
              status: 500,
              headers: { "content-type": "application/json" },
            }),
          );
        }
        return Promise.resolve(json(locksRootFrame));
      }
      return Promise.resolve(json(activityFrame));
    }),
  );
  renderWorkspace("waits_locks");

  const retry = await screen.findByRole("button", {
    name: /table\.error.*table\.retry/i,
  });
  expect(continuationAttempts).toBe(1);

  await new Promise((resolve) => setTimeout(resolve, 25));
  expect(continuationAttempts).toBe(1);

  retry.click();
  await waitFor(() => expect(continuationAttempts).toBe(2));
});

test("retries the initial lock page instead of requesting a continuation", async () => {
  let initialAttempts = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = requestUrl(input);
      if (url.pathname === "/v1/timeline/heatmap")
        return Promise.resolve(json(heatmap));
      if (url.pathname === "/v1/frame/locks") {
        initialAttempts += 1;
        if (initialAttempts === 1) {
          return Promise.resolve(
            new Response(JSON.stringify({ code: "initial_failed" }), {
              status: 500,
              headers: { "content-type": "application/json" },
            }),
          );
        }
        return Promise.resolve(json(locksEdgeFrame));
      }
      return Promise.resolve(json(activityFrame));
    }),
  );
  renderWorkspace("waits_locks");

  const retry = await screen.findByRole("button", {
    name: /table\.error.*table\.retry/i,
  });
  expect(initialAttempts).toBe(1);
  retry.click();

  expect(
    await screen.findByRole("button", { name: /18422.*19041/ }),
  ).toBeDefined();
  expect(initialAttempts).toBe(2);
  const lockCalls = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => requestUrl(input))
    .filter((url) => url.pathname === "/v1/frame/locks");
  expect(lockCalls.every((url) => !url.searchParams.has("cursor"))).toBe(true);
});
