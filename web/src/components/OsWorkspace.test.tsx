import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { HeatmapResponse } from "../api/types";
import {
  makeContextResponse,
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeHeatmapQuality,
  makeMetricSpec,
  makeSpineResponse,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { OsWorkspace, osMetricForPreset } from "./OsWorkspace";

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

const processesView = makeViewSpec({
  code: "processes",
  scope: "host",
  canonical_metric: "cpu",
  metrics: [
    makeMetricSpec({ code: "cpu", unit: "ratio" }),
    makeMetricSpec({ code: "io", unit: "bytes_per_second" }),
  ],
  columns: [
    {
      availability: "available",
      code: "pid",
      lazy: false,
      requires: ["process"],
      type: "i64",
    },
    {
      availability: "available",
      code: "type",
      lazy: false,
      requires: ["process"],
      type: "text",
    },
    {
      availability: "available",
      code: "cpu",
      lazy: false,
      requires: ["process"],
      type: "f64",
      unit: "ratio",
    },
    {
      availability: "available",
      code: "rss",
      lazy: false,
      requires: ["process"],
      type: "u64",
      unit: "bytes",
    },
    {
      availability: "available",
      code: "command",
      lazy: true,
      requires: ["process"],
      type: "text",
    },
  ],
  presets: [
    {
      code: "pressure",
      columns: ["pid", "type", "cpu", "rss", "command"],
      sort: { column: "cpu", order: "desc" },
    },
    {
      code: "disk_io",
      columns: ["pid", "type", "cpu", "rss", "command"],
      sort: { column: "cpu", order: "desc" },
    },
  ],
});

const processFrame = makeFrameResponse({
  view: "processes",
  columns: [
    makeFrameColumn({ code: "pid", type: "i64" }),
    makeFrameColumn({ code: "type", type: "text" }),
    makeFrameColumn({ code: "cpu", type: "f64", unit: "ratio" }),
    makeFrameColumn({ code: "rss", type: "u64", unit: "bytes" }),
    makeFrameColumn({ code: "command", type: "text" }),
  ],
  rows: Array.from({ length: 24 }, (_, index) =>
    makeFrameRow({
      entity: `process:${18422 + index}:1722390000`,
      label: `postgres ${18422 + index}`,
      cells: [
        18422 + index,
        "postgres",
        0.9 - index / 100,
        8_000_000,
        "postgres",
      ],
    }),
  ),
  page: { matched: 218, returned: 24 },
  quality: {
    status: "partial",
    snapshots: 1,
    gaps: [],
    gated: [],
    unavailable_revision: [],
    resource_limited: ["processes:4096"],
    active_tail: true,
  },
});

const heatmap: HeatmapResponse = {
  grid: {
    from_us: "1722396400000000",
    to_us: "1722400000000000",
    bucket_count: 96,
  },
  ranking: { exact: true, unseen_upper: 0 },
  quality: makeHeatmapQuality({
    status: "partial",
    snapshots: 72,
    resource_limited: ["processes:4096"],
  }),
  rows: [
    {
      entity: "process:18422:1722390000",
      label: "postgres 18422",
      unit: "ratio",
      score: { lower: 1, upper: 2 },
      values: Array.from({ length: 96 }, (_, index) =>
        index % 7 === 0 ? null : index / 100,
      ),
    },
  ],
};

const spine = makeSpineResponse({
  series: [
    {
      code: "load_per_cpu",
      unit: "ratio",
      aggregation: "max",
      values: Array.from({ length: 24 }, (_, index) => index / 10),
    },
    {
      code: "psi_io_some",
      unit: "percent",
      aggregation: "max",
      values: Array.from({ length: 24 }, (_, index) => index),
    },
  ],
  quality: {
    status: "partial",
    snapshots: 24,
    gaps: [],
    gated: [],
    resource_limited: ["host_signals"],
    active_tail: true,
  },
});

function stubRequests(
  options: { spineStatus?: number; heatmapStatus?: number } = {},
) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = requestUrl(input);
      if (url.pathname === "/v1/timeline/spine") {
        return Promise.resolve(
          options.spineStatus === undefined
            ? json(spine)
            : json({ code: "spine_failed" }, options.spineStatus),
        );
      }
      if (url.pathname === "/v1/timeline/heatmap") {
        return Promise.resolve(
          options.heatmapStatus === undefined
            ? json(heatmap)
            : json({ code: "heatmap_failed" }, options.heatmapStatus),
        );
      }
      return Promise.resolve(json(processFrame));
    }),
  );
}

function Harness(props: { matched?: number; preset?: "pressure" | "disk_io" }) {
  const [metric, setMetric] = useState<"cpu" | "io">(
    osMetricForPreset(props.preset ?? "pressure"),
  );
  return (
    <OsWorkspace
      view={processesView}
      at="1722400000000000"
      span={3_600}
      from="1722396400000000"
      to="1722400000000000"
      metric={metric}
      preset={props.preset ?? "pressure"}
      q={null}
      sort={null}
      order={null}
      entity={null}
      matched={props.matched ?? 218}
      mobile={false}
      context={makeContextResponse({
        host: { logical_cpu_count: 32, kernel_version: "6.8.0" },
      })}
      onMetricChange={setMetric}
      onSort={() => {}}
      onSelectRow={() => {}}
      onMatched={() => {}}
    />
  );
}

test("maps Storage I/O to I/O and keeps other prepared OS lenses on CPU", () => {
  expect(osMetricForPreset("disk_io")).toBe("io");
  for (const preset of [
    "pressure",
    "cpu",
    "memory",
    "cgroup",
    "processes",
    "data_quality",
    null,
  ]) {
    expect(osMetricForPreset(preset)).toBe("cpu");
  }
});

test("builds one dense OS workspace from independently scoped evidence", async () => {
  stubRequests();
  render(<Harness />, { wrapper: Wrapper });

  expect(await screen.findByTestId("os-workspace")).toBeDefined();
  expect(screen.queryByTestId("infrastructure-analytical-center")).toBeNull();
  expect(screen.getByTestId("host-pressure-evidence")).toBeDefined();
  expect(screen.getByTestId("host-scope-guard").dataset.cpus).toBe("32");
  expect(screen.getByTestId("host-quality").dataset.limited).toBe("1");
  expect(screen.getByTestId("processes-time-matrix")).toBeDefined();
  expect(screen.getAllByTestId("process-interval-row")[0]).toHaveAttribute(
    "data-mode",
    "process_intervals",
  );
  expect(screen.getAllByTestId("time-matrix-bucket")).toHaveLength(24 * 96);
  expect(screen.getByTestId("os-frame-population").dataset.matched).toBe("218");
  expect(screen.getByTestId("os-heatmap-population").dataset.retained).toBe(
    "1",
  );

  const calls = vi.mocked(fetch).mock.calls.map(([input]) => requestUrl(input));
  const spineCall = calls.find((url) => url.pathname === "/v1/timeline/spine");
  const heatmapCall = calls.find(
    (url) => url.pathname === "/v1/timeline/heatmap",
  );
  expect(spineCall?.searchParams.get("buckets")).toBe("24");
  expect(heatmapCall?.searchParams.get("buckets")).toBe("96");
  expect(heatmapCall?.searchParams.get("top")).toBe("64");
  expect(heatmapCall?.searchParams.get("metric")).toBe("cpu");
});

test("switches the exact process matrix metric without changing host scope", async () => {
  stubRequests();
  render(<Harness />, { wrapper: Wrapper });

  await screen.findByTestId("os-workspace");
  fireEvent.click(screen.getByRole("button", { name: /I\/O/i }));
  await waitFor(() => {
    const heatmapCalls = vi
      .mocked(fetch)
      .mock.calls.map(([input]) => requestUrl(input))
      .filter((url) => url.pathname === "/v1/timeline/heatmap");
    expect(heatmapCalls.at(-1)?.searchParams.get("metric")).toBe("io");
  });
  expect(
    vi
      .mocked(fetch)
      .mock.calls.map(([input]) => requestUrl(input))
      .filter((url) => url.pathname === "/v1/timeline/spine"),
  ).toHaveLength(1);
});

test("announces host and process request failures without turning them into zero evidence", async () => {
  stubRequests({ spineStatus: 500, heatmapStatus: 500 });
  render(<Harness />, { wrapper: Wrapper });

  expect(await screen.findByTestId("os-workspace")).toBeDefined();
  const alerts = await screen.findAllByRole("alert");
  expect(alerts.length).toBeGreaterThanOrEqual(2);
  expect(screen.getAllByTestId("process-interval-row")[0].ariaLabel).toContain(
    "host.matrix.loadError",
  );
});
