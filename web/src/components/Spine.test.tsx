import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeEventFact,
  makeEventsResponse,
  makeHealthPoint,
  makeHealthResponse,
  makeIncident,
  makeIncidentsResponse,
  makeSpineResponse,
} from "../testkit/apiFixtures";
import { Spine, type SpineProps } from "./Spine";

afterEach(() => vi.unstubAllGlobals());

/** Fixed cursor; the spine window follows the zoom span (1 h here). */
const AT_US = 1_722_500_000_000_000;
const WINDOW_US = 3_600_000_000;
const FROM_US = AT_US - WINDOW_US;

const spineFixture = makeSpineResponse({
  grid: {
    from_us: String(FROM_US),
    to_us: String(AT_US),
    bucket_count: 3,
  },
  series: [
    {
      code: "host.load1",
      unit: "loadavg",
      aggregation: "avg",
      values: [1, 0.5, null],
    },
  ],
});

// Health is queried over the doubled window (previous + current) at 96
// buckets per window: the previous hour fully calm, the current hour half
// calm, then 24 degraded and 24 critical buckets.
const BUCKET_US = WINDOW_US / 96;
const healthFixture = makeHealthResponse({
  points: [
    ...Array.from({ length: 96 }, (_, i) =>
      makeHealthPoint({
        interval: {
          from_us: FROM_US - WINDOW_US + i * BUCKET_US,
          to_us: FROM_US - WINDOW_US + (i + 1) * BUCKET_US,
        },
        overall_state: "normal",
      }),
    ),
    ...Array.from({ length: 96 }, (_, i) =>
      makeHealthPoint({
        interval: {
          from_us: FROM_US + i * BUCKET_US,
          to_us: FROM_US + (i + 1) * BUCKET_US,
        },
        overall_state: i < 48 ? "normal" : i < 72 ? "degraded" : "critical",
        ...(i >= 72
          ? {
              floor_evidence: [{ class: "oom_kill", supporting_fact_id: "f1" }],
            }
          : i >= 48
            ? {
                domains: [
                  {
                    domain: "cpu_pressure",
                    penalty: 0.4,
                    driving_factor_ids: [1],
                  },
                ],
              }
            : {}),
      }),
    ),
  ],
});

const incidentsFixture = makeIncidentsResponse({
  incidents: [
    makeIncident({
      interval: { from: FROM_US + WINDOW_US / 2, to: AT_US },
    }),
  ],
});

const eventsFixture = makeEventsResponse({
  events: [
    makeEventFact({
      event_kind: "pg.checkpoint.completed",
      notable_class: "info",
      occurred_at_us: FROM_US + WINDOW_US / 2,
      sort_ts_us: FROM_US + WINDOW_US / 2,
    }),
    makeEventFact({
      event_instance_id: "instance-2",
      event_kind: "pg.log.error_group_observed",
      notable_class: "panic",
      occurred_at_us: FROM_US + WINDOW_US / 4,
      sort_ts_us: FROM_US + WINDOW_US / 4,
    }),
    makeEventFact({
      event_instance_id: "instance-3",
      event_kind: "pg.maintenance.autovacuum_reported",
      notable_class: "info",
      occurred_at_us: FROM_US + (WINDOW_US * 3) / 4,
      sort_ts_us: FROM_US + (WINDOW_US * 3) / 4,
    }),
  ],
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function stubFetch() {
  return vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    const body = url.includes("/v1/timeline/events")
      ? eventsFixture
      : url.includes("/v1/timeline/health")
        ? healthFixture
        : url.includes("/v1/incidents")
          ? incidentsFixture
          : spineFixture;
    return Promise.resolve(jsonResponse(body));
  });
}

function stubRect(svg: Element) {
  svg.getBoundingClientRect = () =>
    ({
      left: 0,
      top: 0,
      right: 1000,
      bottom: 60,
      width: 1000,
      height: 60,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }) as DOMRect;
}

function renderSpine(overrides: Partial<SpineProps> = {}) {
  vi.stubGlobal("fetch", stubFetch());
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const props: SpineProps = {
    at: String(AT_US),
    span: 3600,
    baseline: null,
    onSelectAt: () => {},
    onSelectSpan: () => {},
    onSelectBaseline: () => {},
    ...overrides,
  };
  return render(<Spine {...props} />, { wrapper });
}

test("renders verdict ribbon, score chip, glyphs, sparkline and summary", async () => {
  renderSpine();
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(0),
  );
  // Ribbon: calm buckets quiet, warn/crit full; no gap cells in this fixture.
  expect(screen.getAllByTestId("spine-ribbon-warn").length).toBeGreaterThan(0);
  expect(screen.getAllByTestId("spine-ribbon-crit").length).toBeGreaterThan(0);
  expect(screen.queryByTestId("spine-ribbon-gap")).toBeNull();
  // Bucket tooltip: time range, localized verdict and the API reason.
  const critCell = screen.getAllByTestId("spine-ribbon-crit")[0];
  expect(critCell?.querySelector("title")?.textContent).toContain(
    "spine.verdict.crit",
  );
  expect(critCell?.querySelector("title")?.textContent).toContain(
    "health.floor.oom_kill",
  );
  // Score chip: 24 crit buckets (15 min), 24 warn (15 min), 1 incident.
  // 100 − 15×3 − 15×0.5 − 1×5 = 42.5 → 43; prev window is fully calm.
  expect(screen.getByTestId("spine-score").textContent).toContain("43");
  expect(screen.getByTestId("spine-score-delta").textContent).toContain("▼57");
  // Event glyphs per the approved mapping.
  expect(screen.getByText("●")).toBeDefined();
  expect(screen.getByText("◆")).toBeDefined();
  expect(screen.getByText("○")).toBeDefined();
  // Load sparkline skips the null bucket (one 2-point segment).
  const spark = screen.getByTestId("spine-load-line");
  expect(spark.getAttribute("points")?.split(" ")).toHaveLength(2);
  // Right summary: cursor time + current load + crit/warn counts.
  const summary = screen.getByTestId("spine-summary");
  expect(summary.textContent).toContain("host.load1");
  expect(summary.textContent).toContain("▲24");
  expect(summary.textContent).toContain("●24");
  expect(screen.getByTestId("spine-cursor")).toBeDefined();
});

test("a health-less window renders honest gap markers, not silence", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : input.href;
      const body = url.includes("/v1/timeline/health")
        ? makeHealthResponse({ points: [] })
        : url.includes("/v1/timeline/events")
          ? makeEventsResponse({ events: [] })
          : url.includes("/v1/incidents")
            ? makeIncidentsResponse()
            : spineFixture;
      return Promise.resolve(jsonResponse(body));
    }),
  );
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  render(
    <Spine
      at={String(AT_US)}
      span={3600}
      baseline={null}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
    />,
    { wrapper },
  );
  // Health has no points but the load series has values: the strip renders,
  // every ribbon bucket an explicit gap marker with a "no data" tooltip.
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-gap")).toHaveLength(96),
  );
  expect(
    screen.getAllByTestId("spine-ribbon-gap")[0]?.querySelector("title")
      ?.textContent,
  ).toContain("spine.missing");
});

test("an empty window shows the no-data line instead of a blank chart", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : input.href;
      const body = url.includes("/v1/timeline/health")
        ? makeHealthResponse({ points: [] })
        : url.includes("/v1/timeline/events")
          ? makeEventsResponse({ events: [] })
          : url.includes("/v1/incidents")
            ? makeIncidentsResponse()
            : makeSpineResponse({ series: [] });
      return Promise.resolve(jsonResponse(body));
    }),
  );
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  render(
    <Spine
      at={String(AT_US)}
      span={3600}
      baseline={null}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
    />,
    { wrapper },
  );
  await waitFor(() =>
    expect(screen.getByTestId("spine-state").textContent).toContain(
      "spine.missing",
    ),
  );
  expect(screen.queryByRole("slider")).toBeNull();
});

test("click on the strip reports the cursor µs at that position", async () => {
  const onSelectAt = vi.fn();
  renderSpine({ onSelectAt });
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(0),
  );
  const svg = screen.getByRole("slider");
  stubRect(svg);
  fireEvent.click(svg, { clientX: 500 });
  expect(onSelectAt).toHaveBeenCalledWith(String(FROM_US + WINDOW_US / 2));
});

test("shift+click sets the baseline, a repeat nearby clears it", async () => {
  const onSelectBaseline = vi.fn();
  renderSpine({
    onSelectBaseline,
    baseline: String(FROM_US + WINDOW_US / 2),
  });
  await waitFor(() =>
    expect(screen.getByTestId("spine-baseline")).toBeDefined(),
  );
  const svg = screen.getByRole("slider");
  stubRect(svg);
  fireEvent.click(svg, { clientX: 100, shiftKey: true });
  expect(onSelectBaseline).toHaveBeenCalledWith(
    String(FROM_US + WINDOW_US / 10),
  );
  // 500px == the current baseline position; repeat shift-click clears it.
  fireEvent.click(svg, { clientX: 500, shiftKey: true });
  expect(onSelectBaseline).toHaveBeenLastCalledWith(null);
});

test("mode button toggles LIVE → REPLAY and back", async () => {
  const onSelectAt = vi.fn();
  const { unmount } = renderSpine({ at: null, onSelectAt });
  const liveButton = await screen.findByRole("button", { name: /live/i });
  fireEvent.click(liveButton);
  expect(onSelectAt).toHaveBeenCalledWith(expect.stringMatching(/^\d+$/));
  unmount();

  renderSpine({ at: String(AT_US), onSelectAt });
  const replayButton = await screen.findByRole("button", { name: /replay/i });
  fireEvent.click(replayButton);
  expect(onSelectAt).toHaveBeenLastCalledWith(null);
});

test("zoom group reports the selected span", async () => {
  const onSelectSpan = vi.fn();
  renderSpine({ onSelectSpan });
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(0),
  );
  fireEvent.click(screen.getByRole("button", { name: /86400/ }));
  expect(onSelectSpan).toHaveBeenCalledWith(86400);
});
