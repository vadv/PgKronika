import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { ViewSpec } from "../api/types";
import { HeatmapStrip } from "./HeatmapStrip";

afterEach(() => vi.unstubAllGlobals());

const view = {
  code: "statements",
  canonical_metric: "time",
  availability: "available",
  metrics: [
    { code: "time", availability: "available" },
    { code: "calls", availability: "available" },
    { code: "io", availability: "gated" },
  ],
} as unknown as ViewSpec;

const fixture = {
  grid: { from_us: "0", to_us: "4", bucket_count: 4 },
  ranking: { exact: true, unseen_upper: 0 },
  rows: [
    {
      entity: "e1",
      label: "alpha",
      unit: "ms",
      score: { lower: 0, upper: 4 },
      values: [0, 1, null, 4],
    },
    {
      entity: "e2",
      label: "beta",
      unit: "ms",
      score: { lower: 2, upper: 2 },
      values: [2, 2, 2, 2],
    },
  ],
  quality: {
    status: "partial",
    snapshots: 3,
    gaps: [],
    gated: [],
    unavailable_revision: [],
    resource_limited: [],
  },
};

function renderStrip(
  overrides: Partial<{
    metric: string;
    onMetricChange: (m: string) => void;
    onSelectEntity: (e: string) => void;
  }> = {},
) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(fixture), { status: 200 }),
    ),
  );
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return render(
    <HeatmapStrip
      view={view}
      metric={overrides.metric ?? "time"}
      from="0"
      to="4"
      onMetricChange={overrides.onMetricChange ?? (() => {})}
      onSelectEntity={overrides.onSelectEntity ?? (() => {})}
    />,
    { wrapper },
  );
}

test("renders row labels and one cell per bucket, null cell marked empty", async () => {
  const { container } = renderStrip();
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  expect(screen.getByText("beta")).toBeDefined();
  expect(container.querySelectorAll("[data-cell]")).toHaveLength(8);
  const empty = container.querySelector("[data-empty='true']");
  expect(empty).not.toBeNull();
  expect(empty?.getAttribute("title")).toContain("—");
});

test("partial quality renders a warning badge", async () => {
  renderStrip();
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  expect(screen.getByText(/partial/)).toBeDefined();
});

test("metric switcher lists only available metrics and reports changes", async () => {
  const onMetricChange = vi.fn();
  renderStrip({ onMetricChange });
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  expect(screen.queryByRole("button", { name: "io" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "calls" }));
  expect(onMetricChange).toHaveBeenCalledWith("calls");
});

test("row label click reports the entity", async () => {
  const onSelectEntity = vi.fn();
  renderStrip({ onSelectEntity });
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  fireEvent.click(screen.getByText("alpha"));
  expect(onSelectEntity).toHaveBeenCalledWith("e1");
});
