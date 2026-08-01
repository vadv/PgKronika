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

test("row dock renders point fields from the entity endpoint", async () => {
  stubFetch(
    makeEntityPointResponse({
      view: "activity",
      entity: "db:1",
      fields: [
        { code: "tup", reason: null, status: "available", value: 42 },
        {
          code: "locks",
          reason: "producer_gap",
          status: "unavailable",
          value: null,
        },
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
  expect(missing.dataset.status).toBe("unavailable");
  expect(missing.title).toBe("unavailable · producer_gap");
});

test("row dock renders history snapshots when at is not set", async () => {
  stubFetch(
    makeEntityHistoryResponse({
      columns: ["tup", "locks"],
      snapshots: [
        {
          ts_us: "1722400000000000",
          values: [10, 0],
          statuses: ["available", "available"],
          reasons: [null, null],
        },
        {
          ts_us: "1722400060000000",
          values: [12, null],
          statuses: ["available", "not_collected"],
          reasons: [null, "not_collected"],
        },
      ],
    }),
  );
  renderDock({
    state: { ...baseState, at: null, dock: "row", entity: "db:1" },
  });
  await waitFor(() => expect(screen.getByText("tup")).toBeDefined());
  expect(screen.getByText("locks")).toBeDefined();
  expect(screen.getByText("10")).toBeDefined();
  expect(screen.getByText("12")).toBeDefined();
  const cells = screen.getAllByText("—");
  expect(cells.some((c) => c.dataset.status === "not_collected")).toBe(true);
});

test("row dock drills down via server related provenance and clears", async () => {
  stubFetch(
    makeEntityPointResponse({
      view: "statements",
      entity: "db:1",
      fields: [
        { code: "query", reason: null, status: "available", value: "select 1" },
      ],
      related: [
        {
          view: "plans",
          entity: "plan:9",
          relation: "statement_plan",
          provenance: {
            kind: "field_equality",
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
  // The drill target comes from the API related list — typed identity, no
  // client-side join by name/queryid.
  fireEvent.click(screen.getByRole("button", { name: "dock.row.drill" }));
  expect(onPatch).toHaveBeenCalledWith({
    view: "plans",
    entity: "plan:9",
    dock: "row",
  });
  fireEvent.click(screen.getByRole("button", { name: "dock.row.clear" }));
  expect(onPatch).toHaveBeenCalledWith({ entity: null, dock: null });
});
