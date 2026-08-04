import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { UiState } from "../state/url";
import {
  makeEntityHistoryResponse,
  makeEntityPointResponse,
  makeIncident,
  makeIncidentFinding,
  makeIncidentsResponse,
  makeViewSpec,
} from "../testkit/apiFixtures";
import {
  DockOverlay,
  historyColumnSeries,
  type DockOverlayProps,
} from "./DockOverlay";

afterEach(() => vi.unstubAllGlobals());

function wrapper({ children }: { children: ReactNode }) {
  return createElement(
    QueryClientProvider,
    { client: new QueryClient() },
    children,
  );
}

function stubFetch(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(() =>
      Promise.resolve(
        new Response(JSON.stringify(body), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    ),
  );
}

function stubEntityModes(point: unknown, history: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      const body = url.searchParams.has("at") ? point : history;
      return Promise.resolve(
        new Response(JSON.stringify(body), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    }),
  );
}

const baseState: UiState = {
  view: "activity",
  at: "1722400000000000",
  span: 3600,
  baseline: null,
  preset: null,
  q: null,
  sort: null,
  order: null,
  focus: null,
  dock: null,
  entity: null,
};

function renderDock(overrides: Partial<DockOverlayProps> = {}) {
  const props: DockOverlayProps = {
    state: baseState,
    view: makeViewSpec({ code: "activity" }),
    at: baseState.at ?? "1722400000000000",
    mobile: false,
    onClose: () => {},
    onSelectIncident: () => {},
    onPatch: () => {},
    ...overrides,
  };
  return render(<DockOverlay {...props} />, { wrapper });
}

test("renders nothing when the dock is closed", () => {
  const { container } = renderDock();
  expect(container.firstChild).toBeNull();
});

function stubIncidents() {
  stubFetch(
    makeIncidentsResponse({
      incidents: [
        makeIncident({
          incident_key: "incident-1",
          findings: [
            makeIncidentFinding({
              lens_id: "lens-1",
              confidence: "high",
              scope: {
                logical_section: "locks",
                identity: [],
                column: "xact",
              },
            }),
          ],
        }),
      ],
    }),
  );
}

test("incident list opens the detail and back returns", async () => {
  stubIncidents();
  const onSelectIncident = vi.fn();
  renderDock({
    state: { ...baseState, dock: "incidents" },
    onSelectIncident,
  });
  await waitFor(() =>
    expect(screen.getByRole("button", { name: /summary/ })).toBeDefined(),
  );
  fireEvent.click(screen.getByRole("button", { name: /summary/ }));
  expect(onSelectIncident).toHaveBeenCalledWith("incident-1");
  expect(screen.getByText("lens-1")).toBeDefined();

  fireEvent.click(screen.getByRole("button", { name: "dock.incidents.back" }));
  expect(onSelectIncident).toHaveBeenCalledWith(null);
  await waitFor(() =>
    expect(screen.getByRole("button", { name: /summary/ })).toBeDefined(),
  );
});

test("finding jump patches the view and focuses the incident", async () => {
  stubIncidents();
  const onPatch = vi.fn();
  renderDock({ state: { ...baseState, dock: "incidents" }, onPatch });
  await waitFor(() =>
    expect(screen.getByRole("button", { name: /summary/ })).toBeDefined(),
  );
  fireEvent.click(screen.getByRole("button", { name: /summary/ }));
  fireEvent.click(screen.getByRole("button", { name: "dock.incidents.jump" }));
  expect(onPatch).toHaveBeenCalledWith({
    view: "locks",
    focus: "incident-1",
  });
});

test("tab switches the dock kind and close calls onClose", () => {
  const onPatch = vi.fn();
  const onClose = vi.fn();
  renderDock({ state: { ...baseState, dock: "incidents" }, onPatch, onClose });
  fireEvent.click(screen.getByRole("tab", { name: "dock.tabs.row" }));
  expect(onPatch).toHaveBeenCalledWith({ dock: "row" });
  fireEvent.click(screen.getByRole("button", { name: "dock.close" }));
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("desktop row detail is a full forensic workspace and keeps the token in Raw", async () => {
  stubFetch(
    makeEntityPointResponse({ view: "activity", entity: "AQAEBQAAx9" }),
  );
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  renderDock({ state: { ...baseState, dock: "row", entity: "AQAEBQAAx9" } });
  await waitFor(() => expect(screen.getByRole("tabpanel")).toBeDefined());
  const aside = screen.getByLabelText("dock.title");
  expect(aside.classList.contains("dock-overlay--row-workspace")).toBe(true);
  expect(aside.style.insetBlockStart).toBe("136px");
  expect(aside.style.insetBlockEnd).toBe("24px");
  expect(aside.style.insetInline).toBe("0");
  // The raw token never renders in the normal summary or native tooltip.
  expect(screen.queryByText("AQAEBQAA…")).toBeNull();
  expect(
    screen.getByTestId("dock-entity-heading").getAttribute("title"),
  ).toBeNull();
  expect(screen.queryByTestId("dock-copy-token")).toBeNull();
  fireEvent.click(screen.getByRole("tab", { name: "dock.detail.raw" }));
  const copy = screen.getByRole("button", {
    name: "dock.row.copyTechnicalId",
  });
  fireEvent.click(copy);
  expect(writeText).toHaveBeenCalledWith("AQAEBQAAx9");
});

test("row dock renders point fields from the entity endpoint", async () => {
  stubFetch(
    makeEntityPointResponse({
      view: "activity",
      entity: "db:1",
      fields: [
        { code: "tup", value: 42 },
        { code: "locks", value: null },
        { code: "rss", value: null },
      ],
      quality: { status: "partial", gaps: [], gated: [] },
    }),
  );
  renderDock({
    state: { ...baseState, dock: "row", entity: "db:1" },
    view: makeViewSpec({
      code: "activity",
      columns: [
        {
          code: "tup",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
        {
          code: "locks",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
        {
          code: "rss",
          type: "i64",
          lazy: false,
          requires: ["os_process"],
          availability: "gated",
        },
      ],
    }),
  });
  await waitFor(() => expect(screen.getByText("42")).toBeDefined());
  expect(screen.getByText("tup")).toBeDefined();
  expect(screen.queryByText("locks")).toBeNull();
  expect(screen.getByText("rss")).toBeDefined();
  expect(screen.getByText("not collected")).toBeDefined();
  expect(document.querySelector("[data-forensic-summary]")).not.toBeNull();
  const summary = screen.getByRole("tabpanel");
  expect(summary.textContent).not.toMatch(
    /partial|complete|gaps|gated|point projection|\/v1\/entity/i,
  );
  expect(document.querySelector("[data-detail-provenance]")).toBeNull();
  expect(summary.textContent).not.toContain("—");
});

test("process detail orders real Linux evidence into a dense forensic workspace", async () => {
  const fields = [
    { code: "command", value: "postgres: api-worker erp_prod" },
    { code: "read_bytes_per_second", value: 2_000_000 },
    { code: "cache_served_read_bytes_per_second", value: 8_000_000 },
    { code: "logical_read_bytes_per_second", value: 10_000_000 },
    { code: "logical_write_bytes_per_second", value: 1_400_000 },
    { code: "write_bytes_per_second", value: null },
    { code: "cpu_system", value: 0.2 },
    { code: "cpu_user", value: 0.3 },
    { code: "cpu", value: 0.5 },
    { code: "run_delay", value: 0.4 },
    { code: "rss", value: 412_884 },
    { code: "virtual_memory", value: 1_048_576 },
    { code: "voluntary_context_switches_per_second", value: 10 },
    { code: "minor_faults_per_second", value: 100 },
    { code: "parent_pid", value: 1 },
    { code: "uid", value: 999 },
    { code: "effective_uid", value: 998 },
    { code: "started_at", value: "1722400000000000" },
    { code: "pid", value: 12496 },
    { code: "type", value: "postgres: backend" },
    { code: "state", value: "R" },
    { code: "cgroup", value: "/system.slice/postgresql.service" },
  ];
  const unitByCode: Record<string, string | null> = {
    cpu: "ratio",
    cpu_user: "ratio",
    cpu_system: "ratio",
    run_delay: "ratio",
    rss: "kib",
    virtual_memory: "kib",
    voluntary_context_switches_per_second: "per_second",
    minor_faults_per_second: "per_second",
    logical_read_bytes_per_second: "bytes_per_second",
    logical_write_bytes_per_second: "bytes_per_second",
    cache_served_read_bytes_per_second: "bytes_per_second",
    read_bytes_per_second: "bytes_per_second",
    write_bytes_per_second: "bytes_per_second",
  };
  const relatedAt = "1722399990000000";
  const processPoint = makeEntityPointResponse({
    view: "processes",
    entity: "process:12496",
    label: "postgres: backend api-worker",
    fields,
    related: [
      {
        view: "activity",
        entity: "pid:12496",
        relation: "activity_process",
        snapshot_ts_us: relatedAt,
        provenance: {
          kind: "best_effort",
          method: "pid",
          fields: ["pid"],
        },
      },
    ],
  });
  const activityPoint = makeEntityPointResponse({
    view: "activity",
    entity: "pid:12496",
    label: "app/api-worker (active)",
    fields: [
      { code: "database", value: "erp_prod" },
      { code: "user", value: "app" },
      { code: "application", value: "api-worker" },
      { code: "state", value: "active" },
      { code: "wait_event", value: "IO:DataFileRead" },
      { code: "query_duration_us", value: 1_240_000 },
      { code: "query", value: "select * from orders where id = $1" },
    ],
  });
  const requestUrls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input instanceof Request ? input.url : input);
      requestUrls.push(url);
      const body = url.includes("/entity/activity/")
        ? activityPoint
        : processPoint;
      return Promise.resolve(
        new Response(JSON.stringify(body), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    }),
  );
  const onPatch = vi.fn();
  renderDock({
    state: {
      ...baseState,
      view: "processes",
      dock: "row",
      entity: "process:12496",
    },
    view: makeViewSpec({
      code: "processes",
      columns: fields.map((field) => ({
        code: field.code,
        type:
          field.code === "started_at"
            ? "timestamp"
            : typeof field.value === "number"
              ? "f64"
              : "text",
        unit: unitByCode[field.code] ?? null,
        lazy: false,
        requires: ["process"],
        availability: "available",
      })),
    }),
    onPatch,
  });

  await waitFor(() => expect(screen.getByText("12496")).toBeDefined());
  const groupCodes = Array.from(
    document.querySelectorAll<HTMLElement>("[data-forensic-group]"),
  ).map((group) => group.dataset.forensicGroup);
  expect(groupCodes).toEqual(["compute", "ioCache", "context"]);
  expect(screen.getByText("dock.detail.source.process.compute")).toBeDefined();
  expect(screen.getByText("dock.detail.source.process.ioCache")).toBeDefined();
  expect(screen.getByText("dock.detail.source.process.context")).toBeDefined();
  expect(screen.getByText("dock.detail.subgroup.memory")).toBeDefined();
  expect(screen.getByText("dock.detail.subgroup.readPath")).toBeDefined();
  expect(screen.getByText("dock.detail.subgroup.ioRates")).toBeDefined();
  expect(screen.getByText("dock.detail.subgroup.execution")).toBeDefined();
  expect(
    await screen.findByText("dock.detail.relatedActivity.title"),
  ).toBeDefined();
  expect(screen.getByText("select * from orders where id = $1")).toBeDefined();
  expect(screen.getByText("1.24 s")).toBeDefined();
  expect(
    document.querySelector(
      '[data-field="write_bytes_per_second"] .entity-detail__value',
    )?.textContent,
  ).toBe("not collected");
  expect(
    requestUrls.some((url) => {
      const request = new URL(url);
      return (
        request.pathname.includes("/entity/activity/") &&
        request.searchParams.get("at") === relatedAt
      );
    }),
  ).toBe(true);
  fireEvent.click(
    screen.getByRole("button", {
      name: /dock\.detail\.relatedActivity\.open/,
    }),
  );
  expect(onPatch).toHaveBeenCalledWith({
    view: "activity",
    entity: "pid:12496",
    dock: "row",
    preset: null,
    q: null,
    sort: null,
    order: null,
    at: relatedAt,
  });
  const fieldCodes = (group: string) =>
    Array.from(
      document.querySelectorAll<HTMLElement>(
        `[data-forensic-group="${group}"] [data-field]`,
      ),
    ).map((field) => field.dataset.field);
  expect(fieldCodes("compute").slice(0, 4)).toEqual([
    "cpu",
    "cpu_user",
    "cpu_system",
    "run_delay",
  ]);
  expect(fieldCodes("ioCache").slice(0, 3)).toEqual([
    "logical_read_bytes_per_second",
    "cache_served_read_bytes_per_second",
    "read_bytes_per_second",
  ]);
  expect(fieldCodes("context").slice(0, 4)).toEqual([
    "parent_pid",
    "uid",
    "effective_uid",
    "started_at",
  ]);
  expect(
    document.querySelector(
      '[data-field="cache_served_read_bytes_per_second"][data-semantic="estimate"]',
    ),
  ).not.toBeNull();
  expect(screen.getByText("EST").getAttribute("title")).toBe(
    "semantic.kind.EST.label: semantic.kind.EST.explanation",
  );
  expect(screen.getAllByText("R").length).toBeGreaterThan(0);
  expect(screen.getAllByText("G").length).toBeGreaterThan(0);
  expect(screen.getAllByText("S").length).toBeGreaterThan(0);
  expect(screen.getByText("10/s")).toBeDefined();
  expect(screen.getByRole("tabpanel").textContent).not.toMatch(
    /page-cache hits|proof|confidence|exact match|gaps|gated/i,
  );
});

test("row dock fetches bounded history only after selecting the History tab", async () => {
  const point = makeEntityPointResponse({
    fields: [{ code: "tup", value: 12 }],
  });
  const history = makeEntityHistoryResponse({
    columns: ["tup", "locks"],
    snapshots: [
      {
        ts_us: "1722400000000000",
        present: true,
        values: [10, 0],
      },
      {
        ts_us: "1722400060000000",
        present: true,
        values: [12, null],
      },
    ],
  });
  stubEntityModes(point, history);
  renderDock({
    state: { ...baseState, dock: "row", entity: "db:1" },
    view: makeViewSpec({
      code: "activity",
      capabilities: { detail: true, history: true, related: false },
      columns: [
        {
          code: "tup",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
        {
          code: "locks",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
      ],
    }),
  });
  await waitFor(() => expect(screen.getByText("12")).toBeDefined());
  fireEvent.click(screen.getByRole("tab", { name: "dock.detail.history" }));
  await waitFor(() => expect(screen.getByText("10")).toBeDefined());
  const historyTable = within(screen.getByRole("table"));
  expect(historyTable.getByText("tup")).toBeDefined();
  expect(historyTable.getByText("locks")).toBeDefined();
  expect(screen.getAllByText("—").length).toBeGreaterThan(0);
});

test("historyColumnSeries coerces numbers and numeric strings, rejects everything else honestly", () => {
  const data = makeEntityHistoryResponse({
    columns: ["metric"],
    snapshots: [
      { ts_us: "1", present: true, values: [10] },
      { ts_us: "2", present: true, values: ["14"] },
      { ts_us: "3", present: true, values: [null] },
      { ts_us: "4", present: true, values: ["abc"] },
      { ts_us: "5", present: true, values: [""] },
      { ts_us: "6", present: true, values: ["  "] },
      { ts_us: "7", present: true, values: [true] },
    ],
  });
  expect(historyColumnSeries(data, 0)).toEqual([
    10,
    14,
    null,
    null,
    null,
    null,
    null,
  ]);
});

test("history trend charts only numeric columns and tracks the latest observed value", async () => {
  const point = makeEntityPointResponse({
    fields: [{ code: "tup", value: 12 }],
  });
  const history = makeEntityHistoryResponse({
    columns: ["tup", "note"],
    snapshots: [
      { ts_us: "1722400000000000", present: true, values: [10, "ok"] },
      { ts_us: "1722400060000000", present: true, values: ["14", "ok"] },
      { ts_us: "1722400120000000", present: true, values: [null, "ok"] },
    ],
  });
  stubEntityModes(point, history);
  renderDock({
    state: { ...baseState, dock: "row", entity: "db:1" },
    view: makeViewSpec({
      code: "activity",
      capabilities: { detail: true, history: true, related: false },
      columns: [
        {
          code: "tup",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
        {
          code: "note",
          type: "text",
          lazy: false,
          requires: [],
          availability: "available",
        },
      ],
    }),
  });
  await waitFor(() => expect(screen.getByText("12")).toBeDefined());
  fireEvent.click(screen.getByRole("tab", { name: "dock.detail.history" }));
  await waitFor(() =>
    expect(
      document.querySelector(".entity-detail__history-trend"),
    ).not.toBeNull(),
  );
  const lanes = document.querySelectorAll(".entity-detail__history-lane");
  expect(lanes).toHaveLength(1);
  expect(
    document.querySelector(".entity-detail__history-lane-label")?.textContent,
  ).toBe("tup");
  expect(
    document.querySelector(".entity-detail__history-lane-value")?.textContent,
  ).toBe("14");
  expect(
    document.querySelectorAll(".entity-detail__history-lane-chart path").length,
  ).toBeGreaterThan(0);
});

test("history trend stays absent with fewer than two snapshots", async () => {
  const point = makeEntityPointResponse({
    fields: [{ code: "tup", value: 12 }],
  });
  const history = makeEntityHistoryResponse({
    columns: ["tup"],
    snapshots: [{ ts_us: "1722400000000000", present: true, values: [10] }],
  });
  stubEntityModes(point, history);
  renderDock({
    state: { ...baseState, dock: "row", entity: "db:1" },
    view: makeViewSpec({
      code: "activity",
      capabilities: { detail: true, history: true, related: false },
      columns: [
        {
          code: "tup",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
      ],
    }),
  });
  await waitFor(() => expect(screen.getByText("12")).toBeDefined());
  fireEvent.click(screen.getByRole("tab", { name: "dock.detail.history" }));
  await waitFor(() => expect(screen.getByRole("table")).toBeDefined());
  expect(document.querySelector(".entity-detail__history-trend")).toBeNull();
});

test("history follows every continuation without a window quality banner", async () => {
  const point = makeEntityPointResponse({
    fields: [{ code: "tup", value: 12 }],
  });
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      let body: unknown = point;
      if (!url.searchParams.has("at")) {
        const cursor = url.searchParams.get("cursor");
        const page = cursor === null ? 1 : cursor === "page-2" ? 2 : 3;
        body = makeEntityHistoryResponse({
          columns: ["tup"],
          snapshots: [
            {
              ts_us: String(1722400000000000 + page * 1_000_000),
              present: true,
              values: [page],
            },
          ],
          page: { next: page < 3 ? `page-${page + 1}` : null },
          quality:
            page === 1
              ? { status: "complete", gaps: [], gated: [] }
              : page === 2
                ? {
                    status: "partial",
                    gaps: [{ from_us: "1", to_us: "2" }],
                    gated: [],
                  }
                : {
                    status: "partial",
                    gaps: [],
                    gated: ["os_process"],
                  },
        });
      }
      return Promise.resolve(
        new Response(JSON.stringify(body), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    }),
  );
  renderDock({
    state: { ...baseState, dock: "row", entity: "db:1" },
    view: makeViewSpec({
      code: "activity",
      capabilities: { detail: true, history: true, related: false },
      columns: [
        {
          code: "tup",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
      ],
    }),
  });

  await waitFor(() => expect(screen.getByText("12")).toBeDefined());
  fireEvent.click(screen.getByRole("tab", { name: "dock.detail.history" }));
  const firstLoadMore = await screen.findByRole("button", {
    name: "dock.row.loadMore",
  });
  expect(screen.queryByText("dock.detail.historyQuality")).toBeNull();
  fireEvent.click(firstLoadMore);
  await waitFor(() =>
    expect(within(screen.getByRole("table")).getByText("2")).toBeDefined(),
  );
  expect(document.querySelector("[data-history-quality]")).toBeNull();
  expect(screen.getAllByTestId("history-snapshot")).toHaveLength(2);
  fireEvent.click(screen.getByRole("button", { name: "dock.row.loadMore" }));
  await waitFor(() =>
    expect(within(screen.getByRole("table")).getByText("3")).toBeDefined(),
  );
  expect(document.querySelector("[data-history-quality]")).toBeNull();
  expect(screen.getAllByTestId("history-snapshot")).toHaveLength(3);
  expect(
    screen.queryByRole("button", { name: "dock.row.loadMore" }),
  ).toBeNull();

  const historyUrls = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => new URL((input as Request).url))
    .filter((url) => url.searchParams.has("from"));
  expect(historyUrls.map((url) => url.searchParams.get("cursor"))).toEqual([
    null,
    "page-2",
    "page-3",
  ]);
});

test("mobile dock docks to the bottom as a capped sheet", () => {
  renderDock({
    state: { ...baseState, dock: "incidents" },
    mobile: true,
  });
  const aside = screen.getByLabelText("dock.title");
  expect(aside.style.maxHeight).toBe("60vh");
  expect(aside.style.insetBlockEnd).toBe("0");
  expect(aside.style.width).toBe("");
});

test("row dock in LIVE mode sends the resolved at (point shape), not a bare token", async () => {
  const fetchMock = vi.fn().mockImplementation(() =>
    Promise.resolve(
      new Response(
        JSON.stringify(
          makeEntityPointResponse({
            view: "activity",
            entity: "db:1",
            label: "",
            fields: [{ code: "tup", value: 42 }],
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );
  vi.stubGlobal("fetch", fetchMock);
  renderDock({
    state: { ...baseState, at: null, dock: "row", entity: "db:1" },
    at: "1722400000000000",
  });
  await waitFor(() => expect(screen.getByText("42")).toBeDefined());
  const input = fetchMock.mock.calls[0]?.[0];
  const url = input instanceof Request ? input.url : String(input);
  expect(url).toContain("/v1/entity/activity/db%3A1");
  expect(url).toContain("at=1722400000000000");
  // No label from the API: the view name heads the dock, the token stays a
  // routing material is not exposed as the title.
  const heading = screen.getByText("tabs.activity");
  expect(heading.parentElement?.textContent).not.toContain("db:1");
  expect(
    screen.getByTestId("dock-entity-heading").getAttribute("title"),
  ).toBeNull();
});

test("statements row dock: query heading fallback, uncut id, bounded query detail", async () => {
  const full = "-1999008735841373854";
  stubFetch(
    makeEntityPointResponse({
      view: "statements",
      entity: "stmt:1",
      label: full,
      fields: [
        { code: "queryid", value: full },
        { code: "query", value: "select * from orders where id = $1" },
      ],
    }),
  );
  renderDock({
    state: { ...baseState, view: "statements", dock: "row", entity: "stmt:1" },
    view: makeViewSpec({
      code: "statements",
      columns: [
        {
          code: "queryid",
          type: "i64",
          lazy: false,
          requires: [],
          availability: "available",
        },
        {
          code: "query",
          type: "text",
          lazy: true,
          requires: [],
          availability: "available",
        },
      ],
    }),
  });
  // The identifier renders complete — never a "-1999008…" cut.
  await waitFor(() => expect(screen.getByText(full)).toBeDefined());
  // The bare numeric label heads as a localized "Query · <short id>" fallback.
  expect(screen.getByText(/dock\.row\.heading\.statements/)).toBeDefined();
  // Query text is detail-only and server-bounded, but available when the
  // connected PostgreSQL role may see it.
  expect(screen.getByText("select * from orders where id = $1")).toBeDefined();
  fireEvent.click(screen.getByRole("tab", { name: "dock.detail.raw" }));
  expect(screen.getByText("dock.detail.rawProjectedOnly")).toBeDefined();
  const raw = document.querySelector("[data-raw-evidence]")?.textContent ?? "";
  expect(raw).toContain("select * from orders where id = $1");
  expect(raw).toContain("stmt:1");
  expect(raw).toContain("/v1/entity/statements/");
  expect(raw).toContain('"quality"');
});

test("row dock drills down via server related provenance and clears", async () => {
  stubFetch(
    makeEntityPointResponse({
      view: "statements",
      entity: "db:1",
      fields: [{ code: "query", value: "select 1" }],
      related: [
        {
          view: "plans",
          entity: "plan:9",
          relation: "statement_plan",
          snapshot_ts_us: "1722400000000000",
          provenance: {
            kind: "best_effort",
            method: "ossc_queryid_dbid_userid_attribution",
            fields: ["queryid", "dbid", "userid"],
          },
        },
      ],
    }),
  );
  const onPatch = vi.fn();
  renderDock({
    state: {
      ...baseState,
      view: "statements",
      dock: "row",
      entity: "db:1",
      q: "queryid=424242",
    },
    view: makeViewSpec({ code: "statements" }),
    onPatch,
  });
  await waitFor(() => expect(screen.getByText("select 1")).toBeDefined());
  fireEvent.click(
    screen.getByRole("tab", { name: "dock.detail.relationships" }),
  );
  // The drill target comes from the API related list — typed identity, no
  // client-side join by name/queryid.
  const drill = screen.getByRole("button", { name: /dock\.relation\.query/ });
  expect(drill.textContent).toContain("dock.relation.query");
  expect(drill.textContent).toContain("plans");
  expect(drill.textContent).not.toMatch(
    /statement_plan|best_effort|exact|ossc_queryid|queryid|dbid|userid/,
  );
  fireEvent.click(drill);
  expect(onPatch).toHaveBeenCalledWith({
    view: "plans",
    entity: "plan:9",
    dock: "row",
    preset: null,
    q: null,
    sort: null,
    order: null,
    at: "1722400000000000",
  });
  fireEvent.click(screen.getByRole("button", { name: "dock.row.clear" }));
  expect(onPatch).toHaveBeenCalledWith({ entity: null, dock: null });
});

test("activity-process relationship stays positive across partial collection", async () => {
  stubFetch(
    makeEntityPointResponse({
      view: "activity",
      entity: "activity:44",
      quality: {
        status: "partial",
        gaps: [{ from_us: "1722399999000000", to_us: "1722400000000000" }],
        gated: [],
      },
      related: [
        {
          view: "processes",
          entity: "process:44",
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
  );
  renderDock({
    state: {
      ...baseState,
      view: "activity",
      dock: "row",
      entity: "activity:44",
    },
    view: makeViewSpec({ code: "activity" }),
  });

  fireEvent.click(
    await screen.findByRole("tab", { name: "dock.detail.relationships" }),
  );
  const link = screen.getByRole("button", { name: /dock\.relation\.pid/ });
  expect(link.textContent).toContain("dock.relation.pid");
  expect(link.textContent).not.toMatch(
    /activity_process|best_effort|exact|partial|gaps|proof/,
  );
});
