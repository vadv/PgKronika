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
import i18n from "../i18n";
import {
  makeEntityHistoryResponse,
  makeEntityPointResponse,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { DataMaintenanceDetail } from "./DataMaintenanceDetail";

afterEach(() => vi.unstubAllGlobals());

function wrapper({ children }: { children: ReactNode }) {
  return createElement(
    QueryClientProvider,
    {
      client: new QueryClient({
        defaultOptions: { queries: { retry: false } },
      }),
    },
    children,
  );
}

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

const tableView = makeViewSpec({
  code: "tables",
  capabilities: { detail: true, history: true, related: true },
  columns: [
    {
      code: "relation",
      type: "text",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "size",
      type: "i64",
      unit: "bytes",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "seq_scan",
      type: "i64",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "idx_scan",
      type: "i64",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "dead_pct",
      type: "f64",
      unit: "percent",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "io_hit_pct",
      type: "f64",
      unit: "percent",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "xid_age",
      type: "i64",
      unit: "transactions",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "modified_since_analyze",
      type: "i64",
      unit: "count",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "inserted_since_vacuum",
      type: "i64",
      unit: "count",
      lazy: false,
      requires: [],
      availability: "available",
    },
    {
      code: "last_autovacuum",
      type: "timestamp",
      lazy: false,
      requires: [],
      availability: "available",
    },
  ],
});

function tablePoint() {
  return makeEntityPointResponse({
    view: "tables",
    entity: "table:orders",
    label: "public.orders",
    fields: [
      { code: "relation", value: "public.orders" },
      { code: "size", value: 1_503_238_144 },
      { code: "seq_scan", value: 42 },
      { code: "idx_scan", value: 14_200 },
      { code: "dead_pct", value: 12.4 },
      { code: "io_hit_pct", value: 97.3 },
      { code: "xid_age", value: 42_900_000 },
      { code: "modified_since_analyze", value: 422_000 },
      { code: "inserted_since_vacuum", value: 12_000 },
      { code: "last_autovacuum", value: 1_722_399_000_000_000 },
    ],
    related: [
      {
        relation: "table_active_vacuum",
        view: "vacuum",
        entity: "vacuum:8442",
        snapshot_ts_us: "1722400000000000",
        provenance: {
          kind: "temporal",
          method: "same_snapshot_database_relation_oid",
          fields: ["datid", "relid", "ts"],
        },
      },
    ],
  });
}

function tableHistory() {
  return makeEntityHistoryResponse({
    view: "tables",
    entity: "table:orders",
    label: "public.orders",
    columns: [
      "seq_scan",
      "idx_scan",
      "dead_pct",
      "io_hit_pct",
      "modified_since_analyze",
      "inserted_since_vacuum",
    ],
    snapshots: [
      {
        ts_us: "1722396400000000",
        present: true,
        values: [4, 8_000, 8.2, 99.1, 200_000, 4_000],
      },
      {
        ts_us: "1722398200000000",
        present: true,
        values: [12, 10_800, 10.1, 98.4, 310_000, 8_000],
      },
      {
        ts_us: "1722400000000000",
        present: true,
        values: [42, 14_200, 12.4, 97.3, 422_000, 12_000],
      },
    ],
  });
}

test("selected table uses a bounded reference-faithful forensic workspace", async () => {
  await i18n.changeLanguage("en");
  const requests: URL[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      requests.push(url);
      return Promise.resolve(
        response(url.searchParams.has("at") ? tablePoint() : tableHistory()),
      );
    }),
  );

  render(
    <DataMaintenanceDetail
      view={tableView}
      entity="table:orders"
      at="1722400000000000"
      span={86_400}
      onClose={() => {}}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );

  const detail = screen.getByTestId("data-maintenance-detail");
  expect(detail.getAttribute("data-view")).toBe("tables");
  expect(screen.getByTestId("maintenance-entity-strip")).toBeDefined();
  const temporal = await screen.findByTestId("maintenance-temporal-field");
  const lanes = within(temporal).getAllByTestId("maintenance-temporal-lane");
  expect(lanes).toHaveLength(3);
  const accessLane = lanes[0];
  if (accessLane === undefined) throw new Error("access lane is missing");
  expect(
    within(accessLane).getAllByTestId("maintenance-lane-trace"),
  ).toHaveLength(2);
  expect(screen.getByTestId("maintenance-primary-analysis")).toBeDefined();
  expect(screen.getByTestId("maintenance-state-analysis")).toBeDefined();
  expect(screen.getByTestId("maintenance-related-evidence")).toBeDefined();
  expect(
    screen.getByRole("heading", { name: "Access & buffer matrix" }),
  ).toBeDefined();
  expect(
    screen.getByRole("heading", { name: "Churn & freeze stats" }),
  ).toBeDefined();
  expect(
    screen.getByRole("heading", { name: "Related evidence" }),
  ).toBeDefined();
  const matrix = screen.getByTestId("maintenance-history-matrix");
  expect(within(matrix).getByText("Current")).toBeDefined();
  expect(within(matrix).getByText("Δ window")).toBeDefined();
  expect(within(matrix).getByText("Baseline")).toBeDefined();
  expect(screen.getAllByTestId("maintenance-key-stat")).toHaveLength(2);

  await waitFor(() => expect(requests.length).toBe(2));
  const history = requests.find((url) => url.searchParams.has("from"));
  expect(history?.searchParams.get("limit")).toBe("96");
  expect((history?.searchParams.get("columns") ?? "").split(",")).toHaveLength(
    6,
  );
  const from = BigInt(history?.searchParams.get("from") ?? "0");
  const to = BigInt(history?.searchParams.get("to") ?? "0");
  expect(to - from).toBe(21_600_000_000n);
  expect(detail.textContent).not.toMatch(
    /\/v1\/|table:orders|gaps|gated|proof|provenance/i,
  );
});

test("related evidence is navigable and missing history stays local", async () => {
  await i18n.changeLanguage("en");
  const onOpenEntity = vi.fn();
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      return Promise.resolve(
        response(
          url.searchParams.has("at")
            ? tablePoint()
            : makeEntityHistoryResponse({
                ...tableHistory(),
                snapshots: [],
                quality: { status: "partial", gaps: [], gated: [] },
              }),
        ),
      );
    }),
  );

  render(
    <DataMaintenanceDetail
      view={tableView}
      entity="table:orders"
      at="1722400000000000"
      span={3_600}
      onClose={() => {}}
      onOpenEntity={onOpenEntity}
    />,
    { wrapper },
  );

  const related = await screen.findByRole("button", {
    name: "Open related Vacuum evidence",
  });
  fireEvent.click(related);
  expect(onOpenEntity).toHaveBeenCalledWith(
    "vacuum",
    "vacuum:8442",
    "1722400000000000",
  );
  expect(screen.getByTestId("maintenance-history-empty")).toBeDefined();
  expect(
    screen
      .getByTestId("maintenance-collection-state")
      .getAttribute("data-status"),
  ).toBe("partial");
  expect(screen.getByTestId("data-maintenance-detail").textContent).not.toMatch(
    /gap|gated|same_snapshot_database_relation_oid/i,
  );
});

test("history-disabled entity types stay honest without an endless loading state", async () => {
  await i18n.changeLanguage("en");
  const fetchMock = vi.fn().mockResolvedValue(response(tablePoint()));
  vi.stubGlobal("fetch", fetchMock);

  render(
    <DataMaintenanceDetail
      view={{
        ...tableView,
        capabilities: { ...tableView.capabilities, history: false },
      }}
      entity="table:orders"
      at="1722400000000000"
      span={3_600}
      onClose={() => {}}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );

  expect(await screen.findByText("public.orders")).toBeDefined();
  expect(screen.getByTestId("maintenance-history-not-collected")).toBeDefined();
  expect(screen.queryByText("loading history")).toBeNull();
  expect(fetchMock).toHaveBeenCalledTimes(1);
});
