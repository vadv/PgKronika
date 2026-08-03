import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { HeatmapResponse } from "../api/types";
import {
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeHeatmapQuality,
  makeMetricSpec,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { PlansWorkspace } from "./PlansWorkspace";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
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

const plansView = makeViewSpec({
  code: "plans",
  canonical_metric: "time",
  joins: [
    {
      left: "plans",
      right: "statements",
      kind: "best_effort",
      fields: ["queryid", "dbid", "userid"],
      cardinality: "many_to_one",
      provenance: "ossc_queryid_dbid_userid_attribution",
    },
    {
      left: "plans",
      right: "statements",
      kind: "best_effort",
      fields: ["queryid_stat_statements", "dbid", "userid"],
      cardinality: "many_to_one",
      provenance: "vadv_queryid_stat_statements_dbid_userid_attribution",
    },
  ],
  metrics: [
    makeMetricSpec({ code: "time", unit: "us" }),
    makeMetricSpec({ code: "calls", unit: "count" }),
  ],
  columns: [
    {
      availability: "available",
      code: "planid",
      lazy: false,
      requires: ["plans"],
      type: "i64",
    },
    {
      availability: "available",
      code: "queryid",
      lazy: false,
      requires: ["plans"],
      type: "i64",
    },
    {
      availability: "available",
      code: "calls",
      lazy: false,
      requires: ["plans"],
      type: "f64",
    },
    {
      availability: "available",
      code: "mean",
      lazy: false,
      requires: ["plans"],
      type: "f64",
      unit: "ms",
    },
    {
      availability: "available",
      code: "first_call",
      lazy: false,
      requires: ["plans"],
      type: "timestamp",
    },
    {
      availability: "available",
      code: "last_call",
      lazy: false,
      requires: ["plans"],
      type: "timestamp",
    },
  ],
  presets: [
    {
      code: "regression",
      columns: ["planid", "queryid", "mean"],
      sort: { column: "mean", order: "desc" },
    },
    {
      code: "change_timeline",
      columns: [
        "planid",
        "queryid",
        "first_call",
        "last_call",
        "calls",
        "mean",
      ],
      sort: { column: "last_call", order: "desc" },
    },
  ],
});

const planRows = Array.from({ length: 4 }, (_, index) =>
  makeFrameRow({
    entity: `plan:${77 + index}`,
    label: `plan ${77 + index}`,
    cells: [
      String(77 + index),
      String(42 + index),
      18 - index,
      42 - index * 7,
      String(1722390000000000 + index * 1_000_000),
      String(1722400000000000 - index * 1_000_000),
    ],
  }),
);

const planFrame = makeFrameResponse({
  view: "plans",
  columns: [
    makeFrameColumn({ code: "planid", type: "i64" }),
    makeFrameColumn({ code: "queryid", type: "i64" }),
    makeFrameColumn({ code: "calls", type: "f64" }),
    makeFrameColumn({ code: "mean", type: "f64", unit: "ms" }),
    makeFrameColumn({ code: "first_call", type: "timestamp" }),
    makeFrameColumn({ code: "last_call", type: "timestamp" }),
  ],
  rows: planRows,
  page: { matched: 1_000, returned: 4 },
});

const heatmap: HeatmapResponse = {
  grid: {
    from_us: "1722390000000000",
    to_us: "1722400000000000",
    bucket_count: 96,
  },
  ranking: { exact: true, unseen_upper: 0 },
  quality: makeHeatmapQuality({ snapshots: 88 }),
  rows: [
    {
      entity: "plan:77",
      label: "plan 77",
      unit: "us",
      score: { lower: 1, upper: 2 },
      values: Array.from({ length: 96 }, (_, index) =>
        index % 9 === 0 ? null : index * 100,
      ),
    },
  ],
};

function stubRequests(heatmapResponse: Response = json(heatmap)) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = requestUrl(input);
      return Promise.resolve(
        url.pathname === "/v1/timeline/heatmap"
          ? heatmapResponse.clone()
          : json(planFrame),
      );
    }),
  );
}

function Harness(props: { preset?: "regression" | "change_timeline" }) {
  const [metric, setMetric] = useState("time");
  return (
    <PlansWorkspace
      view={plansView}
      at="1722400000000000"
      span={3_600}
      from="1722390000000000"
      to="1722400000000000"
      metric={metric}
      baselineUs={null}
      preset={props.preset ?? "regression"}
      q={null}
      sort={null}
      order={null}
      entity={null}
      matched={1_000}
      mobile={false}
      onMetricChange={setMetric}
      onSort={() => {}}
      onSelectRow={() => {}}
      onOpenEntity={() => {}}
      onMatched={() => {}}
    />
  );
}

test("builds one dense Plans matrix with bounded regression evidence", async () => {
  stubRequests();
  render(<Harness />, { wrapper: Wrapper });

  expect(await screen.findByTestId("plans-time-matrix")).toBeDefined();
  expect(screen.getByTestId("plans-regression-boundary")).toBeDefined();
  expect(screen.queryByTestId("plans-detached-heatmap")).toBeNull();
  const provenance = screen.getByTestId("plans-attribution-provenance");
  expect(provenance.textContent).toContain(
    "ossc_queryid_dbid_userid_attribution",
  );
  expect(provenance.textContent).toContain(
    "vadv_queryid_stat_statements_dbid_userid_attribution",
  );
  expect(
    await screen.findAllByTestId("plan-observation-envelope"),
  ).toHaveLength(3);
  expect(screen.getByTestId("plan-change-evidence").dataset.provenance).toBe(
    "first_last_observed_only",
  );

  const calls = vi.mocked(fetch).mock.calls.map(([input]) => requestUrl(input));
  const heatmapCall = calls.find(
    (url) => url.pathname === "/v1/timeline/heatmap",
  );
  expect(heatmapCall?.searchParams.get("view")).toBe("plans");
  expect(heatmapCall?.searchParams.get("buckets")).toBe("96");
  expect(heatmapCall?.searchParams.get("top")).toBe("64");
  expect(
    calls.some(
      (url) =>
        url.pathname === "/v1/frame/plans" &&
        url.searchParams.get("preset") === "change_timeline" &&
        url.searchParams.get("limit") === "3",
    ),
  ).toBe(true);
});

test("switches exact temporal metric without changing the Plans lens", async () => {
  stubRequests();
  render(<Harness />, { wrapper: Wrapper });
  await screen.findByTestId("plans-time-matrix");

  fireEvent.click(screen.getByRole("button", { name: "calls" }));
  await waitFor(() =>
    expect(
      vi
        .mocked(fetch)
        .mock.calls.map(([input]) => requestUrl(input))
        .some(
          (url) =>
            url.pathname === "/v1/timeline/heatmap" &&
            url.searchParams.get("metric") === "calls",
        ),
    ).toBe(true),
  );
  expect(
    vi
      .mocked(fetch)
      .mock.calls.map(([input]) => requestUrl(input))
      .filter((url) => url.pathname === "/v1/frame/plans")
      .some((url) => url.searchParams.get("preset") === "regression"),
  ).toBe(true);
});

test("keeps ranked plan rows usable when temporal evidence fails", async () => {
  stubRequests(json({ code: "heatmap_failed" }, 500));
  render(<Harness preset="change_timeline" />, { wrapper: Wrapper });

  await waitFor(() =>
    expect(screen.getByTestId("ranked-matrix-body").dataset.loadedRows).toBe(
      "4",
    ),
  );
  expect(screen.getByTestId("plans-time-matrix")).toBeDefined();
  expect(screen.getByRole("button", { name: /table\.retry/i })).toBeDefined();
});
