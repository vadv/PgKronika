import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { HeatmapResponse } from "../api/types";
import {
  makeHeatmapQuality,
  makeMetricSpec,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { HeatmapStrip } from "./HeatmapStrip";

afterEach(() => vi.unstubAllGlobals());

const view = makeViewSpec({
  code: "statements",
  canonical_metric: "time",
  availability: "available",
  metrics: [
    makeMetricSpec({ code: "time", availability: "available" }),
    makeMetricSpec({ code: "calls", availability: "available" }),
    makeMetricSpec({ code: "io", availability: "gated" }),
  ],
});

const fixture: HeatmapResponse = {
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
  quality: makeHeatmapQuality({ status: "partial", snapshots: 3 }),
};

function renderStrip(
  overrides: Partial<{
    metric: string;
    onMetricChange: (m: string) => void;
    onSelectEntity: (e: string) => void;
    selectedRange: { fromUs: string; toUs: string };
    cursorUs: string | null;
    hoverUs: string | null;
    brushDraft: { fromUs: string; toUs: string } | null;
    baselineUs: string | null;
  }> = {},
) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(fixture), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
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
      selectedRange={overrides.selectedRange}
      cursorUs={overrides.cursorUs}
      hoverUs={overrides.hoverUs}
      brushDraft={overrides.brushDraft}
      baselineUs={overrides.baselineUs}
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
  // The structured tooltip shows the honest null marker on hover.
  fireEvent.mouseEnter(empty as Element);
  await waitFor(() =>
    expect(container.querySelector("[role='tooltip']")?.textContent).toContain(
      "spine.missing",
    ),
  );
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

test("partial chip tooltip lists localized quality reasons", async () => {
  const original = fixture.quality;
  fixture.quality = makeHeatmapQuality({
    status: "partial",
    gaps: [{ from_us: "1", to_us: "2" }],
    gated: ["statements"],
    resource_limited: [],
    active_tail: true,
  });
  const { container } = renderStrip();
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  const chip = screen.getByText("heatmap.partial");
  fireEvent.mouseEnter(chip);
  await waitFor(() =>
    expect(container.querySelector("[role='tooltip']")).not.toBeNull(),
  );
  const tip = container.querySelector("[role='tooltip']")?.textContent ?? "";
  expect(tip).toContain("heatmap.quality.gated");
  expect(tip).toContain("heatmap.quality.active_tail");
  fixture.quality = original;
});

test("shared time geometry aligns cursor, hover, brush, baseline and selected range on the bucket grid", async () => {
  renderStrip({
    selectedRange: { fromUs: "0", toUs: "4" },
    cursorUs: "4",
    hoverUs: "1",
    brushDraft: { fromUs: "1", toUs: "3" },
    baselineUs: "2",
  });
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  expect(
    screen.getByTestId("heatmap-time-overlay").style.insetInlineStart,
  ).toBe("220px");
  expect(screen.getByTestId("heatmap-selected-range").style.left).toBe("0%");
  expect(screen.getByTestId("heatmap-selected-range").style.width).toBe("100%");
  expect(screen.getByTestId("heatmap-cursor").style.left).toBe("100%");
  expect(screen.getByTestId("heatmap-hover-cursor").style.left).toBe("25%");
  expect(screen.getByTestId("heatmap-baseline").style.left).toBe("50%");
  expect(screen.getByTestId("heatmap-brush-draft").style.left).toBe("25%");
  expect(screen.getByTestId("heatmap-brush-draft").style.width).toBe("50%");
});

test("invalid or out-of-window shared timestamps are clamped or omitted without changing query geometry", async () => {
  renderStrip({ cursorUs: "9", hoverUs: "invalid", baselineUs: "-2" });
  await waitFor(() => expect(screen.getByText("alpha")).toBeDefined());
  expect(screen.getByTestId("heatmap-cursor").style.left).toBe("100%");
  expect(screen.getByTestId("heatmap-baseline").style.left).toBe("0%");
  expect(screen.queryByTestId("heatmap-hover-cursor")).toBeNull();
});
