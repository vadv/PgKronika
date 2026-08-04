import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createInstance } from "i18next";
import { createElement, type ReactNode } from "react";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import en from "../i18n/en.json";
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

class TestPointerEvent extends MouseEvent {
  readonly pointerId: number;

  constructor(type: string, init: PointerEventInit = {}) {
    super(type, init);
    this.pointerId = init.pointerId ?? 0;
  }
}

beforeEach(() => vi.stubGlobal("PointerEvent", TestPointerEvent));
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
    range: { fromUs: String(FROM_US), toUs: String(AT_US) },
    onSelectAt: () => {},
    onSelectSpan: () => {},
    onSelectBaseline: () => {},
    onToggleLive: () => {},
    ...overrides,
  };
  return render(<Spine {...props} />, { wrapper });
}

type FetchImplementation = (input: RequestInfo | URL) => Promise<Response>;

async function renderLocalizedSpine(
  overrides: Partial<SpineProps> = {},
  fetchImplementation: FetchImplementation = stubFetch(),
) {
  vi.stubGlobal("fetch", vi.fn(fetchImplementation));
  const localized = createInstance();
  await localized.init({
    lng: "en",
    resources: { en: { translation: en } },
    interpolation: { escapeValue: false },
  });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const props: SpineProps = {
    at: String(AT_US),
    span: 3600,
    baseline: null,
    range: { fromUs: String(FROM_US), toUs: String(AT_US) },
    onSelectAt: () => {},
    onSelectBaseline: () => {},
    ...overrides,
  };
  return render(
    <I18nextProvider i18n={localized}>
      <QueryClientProvider client={client}>
        <Spine {...props} />
      </QueryClientProvider>
    </I18nextProvider>,
  );
}

function stubPartialFetch(failedSources: ReadonlySet<string>) {
  return (input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    const source = url.includes("/v1/timeline/events")
      ? "events"
      : url.includes("/v1/timeline/health")
        ? "health"
        : url.includes("/v1/incidents")
          ? "incidents"
          : "spine";
    if (failedSources.has(source)) {
      return Promise.resolve(
        new Response(JSON.stringify({ code: `${source}_transport_failed` }), {
          status: 500,
          headers: { "content-type": "application/json" },
        }),
      );
    }
    const body =
      source === "events"
        ? eventsFixture
        : source === "health"
          ? healthFixture
          : source === "incidents"
            ? incidentsFixture
            : spineFixture;
    return Promise.resolve(jsonResponse(body));
  };
}

test("renders verdict ribbon, score chip, event density, sparkline and summary", async () => {
  renderSpine();
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(0),
  );
  // Ribbon: calm buckets quiet, warn/crit full; no gap cells in this fixture.
  expect(screen.getAllByTestId("spine-ribbon-warn").length).toBeGreaterThan(0);
  expect(screen.getAllByTestId("spine-ribbon-crit").length).toBeGreaterThan(0);
  expect(screen.queryByTestId("spine-ribbon-gap")).toBeNull();
  expect(screen.getByRole("slider").getAttribute("data-health-render")).toBe(
    "signal-line",
  );
  expect(
    screen.getAllByTestId("spine-ribbon-crit")[0]?.querySelector("path"),
  ).not.toBeNull();
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
  // Event facts are aggregated into bounded density cells, never piled up.
  expect(screen.getAllByTestId("spine-event-density")).toHaveLength(3);
  expect(
    Number(
      screen.getAllByTestId("spine-event-density")[0]?.getAttribute("width"),
    ),
  ).toBeLessThanOrEqual(4);
  // Load sparkline skips the null bucket (one 2-point segment).
  const spark = screen.getByTestId("spine-load-line");
  expect(spark.getAttribute("points")?.split(" ")).toHaveLength(2);
  // Right summary: cursor time + current load + crit/warn counts.
  const summary = screen.getByTestId("spine-summary");
  expect(summary.textContent).toContain("host.load1");
  expect(summary.textContent).toContain("healthLine.events");
  expect(summary.textContent).toContain("▲24");
  expect(summary.textContent).toContain("●24");
  expect(screen.getByTestId("spine-cursor")).toBeDefined();
});

test("keeps current and previous incident requests inside the 24 hour API bound", async () => {
  const dayUs = 86_400_000_000;
  renderSpine({
    span: 86_400,
    range: {
      fromUs: String(AT_US - dayUs),
      toUs: String(AT_US),
    },
  });

  await waitFor(() => {
    const incidentRequests = vi
      .mocked(fetch)
      .mock.calls.map(
        ([input]) =>
          new URL(
            typeof input === "string"
              ? input
              : input instanceof Request
                ? input.url
                : input.href,
          ),
      )
      .filter((url) => url.pathname === "/v1/incidents");
    expect(incidentRequests).toHaveLength(2);
    expect(
      incidentRequests.every(
        (url) =>
          Number(url.searchParams.get("to")) -
            Number(url.searchParams.get("from")) <=
          dayUs,
      ),
    ).toBe(true);
  });
});

test("discloses a lower-bound event total when the bounded cursor budget is exhausted", async () => {
  let eventPage = 0;
  await renderLocalizedSpine({}, (input) => {
    const url = new URL(
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href,
    );
    if (url.pathname === "/v1/timeline/events") {
      eventPage += 1;
      return Promise.resolve(
        jsonResponse(
          makeEventsResponse({
            events: [
              makeEventFact({
                event_instance_id: `page-${eventPage}`,
                occurred_at_us: FROM_US + eventPage,
                sort_ts_us: FROM_US + eventPage,
              }),
            ],
            next_cursor: `cursor-${eventPage + 1}`,
          }),
        ),
      );
    }
    const body =
      url.pathname === "/v1/timeline/health"
        ? healthFixture
        : url.pathname === "/v1/incidents"
          ? incidentsFixture
          : spineFixture;
    return Promise.resolve(jsonResponse(body));
  });

  await waitFor(() => expect(eventPage).toBe(4));
  expect(screen.getByTestId("spine-summary").textContent).toContain(
    "events ≥4",
  );
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
      range={{ fromUs: String(FROM_US), toUs: String(AT_US) }}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
      onToggleLive={() => {}}
    />,
    { wrapper },
  );
  // Health has no points but the load series has values: the strip renders,
  // every ribbon bucket an explicit local no-snapshot marker.
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-gap")).toHaveLength(96),
  );
  expect(
    screen.getAllByTestId("spine-ribbon-gap")[0]?.querySelector("title")
      ?.textContent,
  ).toContain("data.noSnapshotInterval");
  expect(screen.getByTestId("spine-score").textContent).toContain("—");
  expect(screen.getByTestId("health-score-state").textContent).toContain(
    "data.noSnapshotCurrent",
  );
  expect(screen.queryByTestId("health-line-quality")).toBeNull();
});

test("a missing health interval does not suppress the score from observed points", async () => {
  await renderLocalizedSpine({}, (input) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    if (url.includes("/v1/timeline/health")) {
      return Promise.resolve(
        jsonResponse(
          makeHealthResponse({ points: healthFixture.points.slice(0, -1) }),
        ),
      );
    }
    const body = url.includes("/v1/timeline/events")
      ? eventsFixture
      : url.includes("/v1/incidents")
        ? incidentsFixture
        : spineFixture;
    return Promise.resolve(jsonResponse(body));
  });
  await waitFor(() =>
    expect(screen.getByTestId("spine-score").textContent).toContain("44"),
  );
  expect(screen.queryByTestId("health-score-state")).toBeNull();
});

test("selection overlays share one SVG grid while gap hatch remains on top", async () => {
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
      baseline={String(FROM_US + WINDOW_US / 2)}
      range={{ fromUs: String(FROM_US), toUs: String(AT_US) }}
      hoverUs={String(FROM_US + WINDOW_US / 4)}
      brushDraft={{
        fromUs: String(FROM_US + WINDOW_US / 5),
        toUs: String(FROM_US + (WINDOW_US * 7) / 10),
      }}
      onSelectAt={() => {}}
      onSelectBaseline={() => {}}
    />,
    { wrapper },
  );

  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-gap")).toHaveLength(96),
  );
  const selected = screen.getByTestId("health-selected-range");
  const draft = screen.getByTestId("health-brush-draft");
  const baseline = screen.getByTestId("spine-baseline");
  const hover = screen.getByTestId("health-hover-cursor");
  const firstHatch = screen.getAllByTestId("spine-gap-hatch")[0];

  expect(selected.getAttribute("x")).toBe("0");
  expect(selected.getAttribute("width")).toBe("1000");
  expect(draft.getAttribute("x")).toBe("200");
  expect(draft.getAttribute("width")).toBe("500");
  expect(baseline.getAttribute("x1")).toBe("500");
  expect(hover.getAttribute("x1")).toBe("250");
  expect(
    selected.compareDocumentPosition(firstHatch as Node) &
      Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  expect(
    draft.compareDocumentPosition(firstHatch as Node) &
      Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
});

test("localized evidence summary exposes verdicts outside the slider leaf", async () => {
  await renderLocalizedSpine();
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-crit").length).toBeGreaterThan(
      0,
    ),
  );
  const summary = screen.getByRole("status");
  expect(summary.textContent).toContain("48 calm");
  expect(summary.textContent).toContain("24 warning");
  expect(summary.textContent).toContain("24 critical");
  expect(summary.textContent).toContain("Current interval: critical");
  expect(summary.textContent).not.toContain(/gaps|sources|coverage/i);
  expect(screen.getByText("Health · PostgreSQL + OS")).toBeDefined();
  expect(screen.getByTestId("spine-score").textContent).toContain(
    "now critical",
  );
  expect(screen.getByRole("slider").getAttribute("aria-describedby")).toContain(
    summary.id,
  );
});

test("health transport failure keeps OS evidence without a source warning", async () => {
  await renderLocalizedSpine({}, stubPartialFetch(new Set(["health"])));
  await waitFor(() =>
    expect(screen.getByTestId("spine-load-line")).toBeDefined(),
  );
  expect(screen.queryByTestId("health-line-source-state")).toBeNull();
  expect(screen.getByTestId("spine-load-line")).toBeDefined();
  expect(screen.getByRole("slider")).toBeDefined();
  expect(screen.getByRole("status").textContent).not.toContain(
    /partial|source/i,
  );
});

test("spine and event failures do not weaken retained health observations", async () => {
  await renderLocalizedSpine(
    {},
    stubPartialFetch(new Set(["spine", "events"])),
  );
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-crit").length).toBeGreaterThan(
      0,
    ),
  );
  expect(screen.queryByTestId("health-line-source-state")).toBeNull();
  expect(screen.getAllByTestId("spine-ribbon-crit").length).toBeGreaterThan(0);
  expect(screen.getByRole("status").textContent).not.toContain(
    /partial|source/i,
  );
});

test("event evidence remains visible when health and spine responses are empty", async () => {
  await renderLocalizedSpine({}, (input) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    const body = url.includes("/v1/timeline/events")
      ? eventsFixture
      : url.includes("/v1/timeline/health")
        ? makeHealthResponse({ points: [] })
        : url.includes("/v1/incidents")
          ? incidentsFixture
          : makeSpineResponse({ series: [] });
    return Promise.resolve(jsonResponse(body));
  });

  expect(await screen.findByRole("slider")).toBeDefined();
  expect(screen.getAllByTestId("spine-event-density")).toHaveLength(3);
  expect(screen.queryByTestId("spine-state")).toBeNull();
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
      range={{ fromUs: String(FROM_US), toUs: String(AT_US) }}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
      onToggleLive={() => {}}
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

test("pointer click on the strip reports the cursor µs at that position", async () => {
  const onSelectAt = vi.fn();
  renderSpine({ onSelectAt });
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(0),
  );
  const svg = screen.getByRole("slider");
  stubRect(svg);
  Object.assign(svg, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });
  fireEvent.pointerDown(svg, { clientX: 500, pointerId: 1, button: 0 });
  fireEvent.pointerUp(svg, { clientX: 500, pointerId: 1, button: 0 });
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
  Object.assign(svg, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });
  fireEvent.pointerDown(svg, {
    clientX: 100,
    pointerId: 1,
    button: 0,
    shiftKey: true,
  });
  fireEvent.pointerUp(svg, {
    clientX: 100,
    pointerId: 1,
    button: 0,
    shiftKey: true,
  });
  expect(onSelectBaseline).toHaveBeenCalledWith(
    String(FROM_US + WINDOW_US / 10),
  );
  // 500px == the current baseline position; repeat shift-click clears it.
  fireEvent.pointerDown(svg, {
    clientX: 500,
    pointerId: 2,
    button: 0,
    shiftKey: true,
  });
  fireEvent.pointerUp(svg, {
    clientX: 500,
    pointerId: 2,
    button: 0,
    shiftKey: true,
  });
  expect(onSelectBaseline).toHaveBeenLastCalledWith(null);
});

test("timeline contains no embedded mode or zoom controls", async () => {
  renderSpine();
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(0),
  );
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.queryByRole("group", { name: /spine\.zoom/i })).toBeNull();
});

test("pointer brush previews immediately and commits exactly once on pointer up", async () => {
  const onBrushDraft = vi.fn();
  const onCommitRange = vi.fn();
  const setPointerCapture = vi.fn();
  const releasePointerCapture = vi.fn();
  renderSpine({ onBrushDraft, onCommitRange });
  const svg = await screen.findByRole("slider");
  stubRect(svg);
  Object.assign(svg, { setPointerCapture, releasePointerCapture });

  fireEvent.pointerDown(svg, { clientX: 200, pointerId: 7, button: 0 });
  fireEvent.pointerMove(svg, { clientX: 650, pointerId: 7, buttons: 1 });
  expect(setPointerCapture).toHaveBeenCalledWith(7);
  expect(onBrushDraft).toHaveBeenLastCalledWith({
    fromUs: String(FROM_US + WINDOW_US * 0.2),
    toUs: String(FROM_US + WINDOW_US * 0.65),
  });
  expect(onCommitRange).not.toHaveBeenCalled();

  fireEvent.pointerUp(svg, { clientX: 650, pointerId: 7, button: 0 });
  expect(onCommitRange).toHaveBeenCalledTimes(1);
  expect(onCommitRange).toHaveBeenCalledWith({
    fromUs: String(FROM_US + WINDOW_US * 0.2),
    toUs: String(FROM_US + WINDOW_US * 0.65),
  });
  expect(releasePointerCapture).toHaveBeenCalledWith(7);
});

test("captured far pointer-up commits a brush without an intermediate move", async () => {
  const onSelectAt = vi.fn();
  const onBrushDraft = vi.fn();
  const onHover = vi.fn();
  const onCommitRange = vi.fn();
  renderSpine({ onSelectAt, onBrushDraft, onHover, onCommitRange });
  const svg = await screen.findByRole("slider");
  stubRect(svg);
  Object.assign(svg, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });

  fireEvent.pointerDown(svg, { clientX: 200, pointerId: 12, button: 0 });
  fireEvent.pointerUp(svg, { clientX: 1200, pointerId: 12, button: 0 });

  expect(onSelectAt).not.toHaveBeenCalled();
  expect(onCommitRange).toHaveBeenCalledTimes(1);
  expect(onCommitRange).toHaveBeenCalledWith({
    fromUs: String(FROM_US + WINDOW_US * 0.2),
    toUs: String(AT_US),
  });
  expect(onBrushDraft).toHaveBeenLastCalledWith(null);
  expect(onHover).toHaveBeenLastCalledWith(null);
});

test("pointer cancel and lost capture clear ephemeral state without committing", async () => {
  const onBrushDraft = vi.fn();
  const onHover = vi.fn();
  const onCommitRange = vi.fn();
  renderSpine({ onBrushDraft, onHover, onCommitRange });
  const svg = await screen.findByRole("slider");
  stubRect(svg);
  Object.assign(svg, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });

  fireEvent.pointerDown(svg, { clientX: 200, pointerId: 13, button: 0 });
  fireEvent.pointerMove(svg, { clientX: 650, pointerId: 13, buttons: 1 });
  fireEvent.pointerCancel(svg, { clientX: 650, pointerId: 13 });
  expect(onBrushDraft).toHaveBeenLastCalledWith(null);
  expect(onHover).toHaveBeenLastCalledWith(null);
  expect(onCommitRange).not.toHaveBeenCalled();

  fireEvent.pointerDown(svg, { clientX: 300, pointerId: 14, button: 0 });
  fireEvent.pointerMove(svg, { clientX: 700, pointerId: 14, buttons: 1 });
  fireEvent.lostPointerCapture(svg, { clientX: 700, pointerId: 14 });
  expect(onBrushDraft).toHaveBeenLastCalledWith(null);
  expect(onHover).toHaveBeenLastCalledWith(null);
  expect(onCommitRange).not.toHaveBeenCalled();
});

test("lost capture after pointer-up does not commit the finished brush twice", async () => {
  const onCommitRange = vi.fn();
  renderSpine({ onCommitRange });
  const svg = await screen.findByRole("slider");
  stubRect(svg);
  Object.assign(svg, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });

  fireEvent.pointerDown(svg, { clientX: 200, pointerId: 15, button: 0 });
  fireEvent.pointerMove(svg, { clientX: 650, pointerId: 15, buttons: 1 });
  fireEvent.pointerUp(svg, { clientX: 650, pointerId: 15, button: 0 });
  fireEvent.lostPointerCapture(svg, { clientX: 650, pointerId: 15 });

  expect(onCommitRange).toHaveBeenCalledTimes(1);
});

test("sub-threshold pointer movement remains a cursor click", async () => {
  const onSelectAt = vi.fn();
  const onCommitRange = vi.fn();
  renderSpine({ onSelectAt, onCommitRange });
  const svg = await screen.findByRole("slider");
  stubRect(svg);
  Object.assign(svg, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });

  fireEvent.pointerDown(svg, { clientX: 500, pointerId: 9, button: 0 });
  fireEvent.pointerMove(svg, { clientX: 503, pointerId: 9, buttons: 1 });
  fireEvent.pointerUp(svg, { clientX: 503, pointerId: 9, button: 0 });

  expect(onCommitRange).not.toHaveBeenCalled();
  expect(onSelectAt).toHaveBeenCalledWith(
    String(FROM_US + Math.round(WINDOW_US * 0.503)),
  );
});

test("live mode anchors the grid and hatches the forming tail bucket", async () => {
  vi.useFakeTimers({ now: AT_US / 1000, toFake: ["Date"] });
  try {
    renderSpine({ at: null });
    await waitFor(() =>
      expect(screen.getAllByTestId("spine-ribbon-ok").length).toBeGreaterThan(
        0,
      ),
    );
    // The tail bucket is hatched and its tooltip says the period is forming.
    const forming = screen.getByTestId("spine-ribbon-forming");
    expect(
      forming.parentElement?.querySelector("title")?.textContent,
    ).toContain("spine.forming");
    // Score over the 95 completed buckets: the forming tail removes one
    // critical bucket, so 23 crit + 24 warn and 1 incident round to 44.
    expect(screen.getByTestId("spine-score").textContent).toContain("44");
    expect(screen.getByTestId("spine-score-delta").textContent).toContain(
      "▼56",
    );
  } finally {
    vi.useRealTimers();
  }
});

test("a 503 during revalidation keeps the ribbon — warming is cold-start only", async () => {
  const client = new QueryClient();
  client.setQueryData(
    ["timeline-health", String(FROM_US - WINDOW_US), String(AT_US), 37500000],
    healthFixture,
  );
  client.setQueryData(
    ["timeline-spine", String(FROM_US), String(AT_US), 96],
    spineFixture,
  );
  client.setQueryData(
    ["incidents", String(FROM_US), String(AT_US)],
    incidentsFixture,
  );
  client.setQueryData(
    ["incidents", String(FROM_US - WINDOW_US), String(FROM_US)],
    incidentsFixture,
  );
  client.setQueryData(
    ["timeline-events", String(FROM_US), String(AT_US), 50, 4],
    eventsFixture,
  );
  // Every refetch fails as a warm-up 503; cached answers must stay on screen.
  vi.stubGlobal(
    "fetch",
    vi.fn(() =>
      Promise.resolve(
        new Response(
          JSON.stringify({
            code: "analytic_capacity_unavailable",
            params: { retry_after_seconds: 1 },
          }),
          { status: 503, headers: { "content-type": "application/json" } },
        ),
      ),
    ),
  );
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  render(
    <Spine
      at={String(AT_US)}
      span={3600}
      baseline={null}
      range={{ fromUs: String(FROM_US), toUs: String(AT_US) }}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
      onToggleLive={() => {}}
    />,
    { wrapper },
  );
  await waitFor(() =>
    expect(screen.getAllByTestId("spine-ribbon-crit").length).toBeGreaterThan(
      0,
    ),
  );
  expect(screen.queryByTestId("spine-state")).toBeNull();
});

test("a cold start under a 503 says warming, not error", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(() =>
      Promise.resolve(
        new Response(
          JSON.stringify({
            code: "analytic_capacity_unavailable",
            params: { retry_after_seconds: 1 },
          }),
          { status: 503, headers: { "content-type": "application/json" } },
        ),
      ),
    ),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  render(
    <Spine
      at={String(AT_US)}
      span={3600}
      baseline={null}
      range={{ fromUs: String(FROM_US), toUs: String(AT_US) }}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
      onToggleLive={() => {}}
    />,
    { wrapper },
  );
  await waitFor(() =>
    expect(screen.getByTestId("spine-state").textContent).toContain(
      "loading.warming",
    ),
  );
  expect(screen.queryByRole("slider")).toBeNull();
});

test("a window without a previous one says so instead of a bare dash", async () => {
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
        ? makeHealthResponse({
            points: Array.from({ length: 96 }, (_, i) =>
              makeHealthPoint({
                interval: {
                  from_us: FROM_US + i * BUCKET_US,
                  to_us: FROM_US + (i + 1) * BUCKET_US,
                },
                overall_state: "normal",
              }),
            ),
          })
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
      range={{ fromUs: String(FROM_US), toUs: String(AT_US) }}
      onSelectAt={() => {}}
      onSelectSpan={() => {}}
      onSelectBaseline={() => {}}
      onToggleLive={() => {}}
    />,
    { wrapper },
  );
  await waitFor(() =>
    expect(screen.getByTestId("spine-score-delta").textContent).toContain(
      "spine.score.noPrev",
    ),
  );
});
