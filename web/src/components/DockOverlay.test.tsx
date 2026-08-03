import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
import { DockOverlay, type DockOverlayProps } from "./DockOverlay";

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

test("row dock: no visible token line — the token rides tooltip and copy", async () => {
  stubFetch(
    makeEntityPointResponse({ view: "activity", entity: "AQAEBQAAx9" }),
  );
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  renderDock({ state: { ...baseState, dock: "row", entity: "AQAEBQAAx9" } });
  await waitFor(() =>
    expect(screen.getByTestId("dock-copy-token")).toBeDefined(),
  );
  // The raw token never renders as its own line; the heading carries it.
  expect(screen.queryByText("AQAEBQAA…")).toBeNull();
  expect(screen.getByTitle("AQAEBQAAx9")).toBeDefined();
  fireEvent.click(screen.getByTestId("dock-copy-token"));
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
      ],
      quality: { status: "partial", gaps: [], gated: [] },
    }),
  );
  renderDock({
    state: { ...baseState, dock: "row", entity: "db:1" },
  });
  await waitFor(() => expect(screen.getByText("42")).toBeDefined());
  expect(screen.getByText("tup")).toBeDefined();
  expect(screen.getByText("dock.row.partial")).toBeDefined();
  const missing = screen.getByText("—");
  expect(missing.dataset.status).toBeUndefined();
  expect(missing.title).toBe("");
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
        values: [10, 0],
      },
      {
        ts_us: "1722400060000000",
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
  expect(screen.getByText("tup")).toBeDefined();
  expect(screen.getByText("locks")).toBeDefined();
  expect(screen.getAllByText("—").length).toBeGreaterThan(0);
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
  // short secondary line — never the raw token as the title.
  const heading = screen.getByText("tabs.activity");
  expect(heading.parentElement?.textContent).not.toContain("db:1");
  expect(screen.getByTitle("db:1")).toBeDefined();
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
  expect(document.querySelector("[data-raw-evidence]")?.textContent).toContain(
    "select * from orders where id = $1",
  );
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
  const drill = screen.getByRole("button", { name: /statement_plan/ });
  // Relation kind, method and fields are visible evidence, not tooltip-only.
  expect(drill.textContent).toContain("best_effort");
  expect(drill.textContent).toContain("ossc_queryid_dbid_userid_attribution");
  expect(drill.textContent).toContain("queryid, dbid, userid");
  fireEvent.click(drill);
  expect(onPatch).toHaveBeenCalledWith({
    view: "plans",
    entity: "plan:9",
    dock: "row",
  });
  fireEvent.click(screen.getByRole("button", { name: "dock.row.clear" }));
  expect(onPatch).toHaveBeenCalledWith({ entity: null, dock: null });
});
