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
import { StatementsWorkspace } from "./StatementsWorkspace";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const view = makeViewSpec({
  code: "statements",
  canonical_metric: "time",
  metrics: [
    makeMetricSpec({ code: "time", unit: "ms" }),
    makeMetricSpec({ code: "calls", unit: "count" }),
  ],
  columns: [
    {
      availability: "available",
      code: "queryid",
      lazy: false,
      requires: [],
      type: "i64",
    },
    {
      availability: "available",
      code: "database",
      lazy: false,
      requires: [],
      type: "text",
    },
    {
      availability: "available",
      code: "user",
      lazy: false,
      requires: [],
      type: "text",
    },
    {
      availability: "available",
      code: "total",
      lazy: false,
      requires: [],
      type: "f64",
      unit: "duration_ms",
    },
  ],
});

const heatmap: HeatmapResponse = {
  grid: { from_us: "100", to_us: "200", bucket_count: 96 },
  ranking: { exact: true, unseen_upper: 0 },
  quality: makeHeatmapQuality({ snapshots: 96 }),
  rows: [
    {
      entity: "stmt:1",
      label: "statement 1",
      unit: "ms",
      score: { lower: 1, upper: 2 },
      values: Array.from({ length: 96 }, (_, index) => index),
    },
  ],
};

const frame = makeFrameResponse({
  view: "statements",
  columns: [
    makeFrameColumn({ code: "queryid", type: "i64" }),
    makeFrameColumn({ code: "database", type: "text" }),
    makeFrameColumn({ code: "user", type: "text" }),
    makeFrameColumn({ code: "total", type: "f64", unit: "duration_ms" }),
  ],
  rows: [
    makeFrameRow({
      entity: "stmt:1",
      label: "statement 1",
      cells: ["10001", "orders", "app_rw", 42],
    }),
    makeFrameRow({
      entity: "stmt:2",
      label: "statement 2",
      cells: ["10002", "orders", "reporter", 21],
    }),
  ],
  page: { matched: 1_000, returned: 2 },
});

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

function stubWorkspaceRequests(heatmapResponse: Response = json(heatmap)) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = requestUrl(input);
      return Promise.resolve(
        url.pathname === "/v1/timeline/heatmap"
          ? heatmapResponse.clone()
          : json(frame),
      );
    }),
  );
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
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

function Harness() {
  const [metric, setMetric] = useState("time");
  return (
    <StatementsWorkspace
      view={view}
      at="150"
      span={3_600}
      from="100"
      to="200"
      metric={metric}
      baselineUs={null}
      preset="time"
      q={null}
      sort={null}
      order={null}
      entity={null}
      matched={1_000}
      mobile={false}
      onMetricChange={setMetric}
      onSort={() => {}}
      onSelectRow={() => {}}
      onMatched={() => {}}
    />
  );
}

test("builds one bounded Statements matrix with the 96-bucket retained set", async () => {
  stubWorkspaceRequests();
  render(<Harness />, { wrapper: Wrapper });

  expect(await screen.findByTestId("statements-time-matrix")).toBeDefined();
  expect(screen.queryByTestId("statements-detached-heatmap")).toBeNull();
  await waitFor(() =>
    expect(
      document
        .querySelector(".statements-workspace__count")
        ?.getAttribute("data-retained"),
    ).toBe("1"),
  );
  expect(
    document
      .querySelector(".statements-workspace__count")
      ?.getAttribute("data-matched"),
  ).toBe("1000");

  const heatmapCall = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => requestUrl(input))
    .find((url) => url.pathname === "/v1/timeline/heatmap");
  expect(heatmapCall?.searchParams.get("buckets")).toBe("96");
  expect(heatmapCall?.searchParams.get("top")).toBe("64");
});

test("changes temporal evidence metric without changing the frame lens", async () => {
  stubWorkspaceRequests();
  render(<Harness />, { wrapper: Wrapper });
  await screen.findByTestId("statements-time-matrix");

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

  const frameCalls = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => requestUrl(input))
    .filter((url) => url.pathname === "/v1/frame/statements");
  expect(
    frameCalls.every((url) => url.searchParams.get("preset") === "time"),
  ).toBe(true);
});

test("keeps ranked frame evidence usable when the heatmap fails", async () => {
  stubWorkspaceRequests(json({ code: "heatmap_failed" }, 500));
  render(<Harness />, { wrapper: Wrapper });

  await waitFor(() =>
    expect(screen.getByTestId("ranked-matrix-body").dataset.loadedRows).toBe(
      "2",
    ),
  );
  expect(screen.getByTestId("statements-time-matrix")).toBeDefined();
  expect(screen.getByRole("button", { name: /table\.retry/i })).toBeDefined();
});
