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
import { ActivityDetail } from "./ActivityDetail";

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

const columns = [
  ["pid", "i64"],
  ["user", "text"],
  ["database", "text"],
  ["application", "text"],
  ["backend_type", "text"],
  ["state", "text"],
  ["wait_event", "text"],
  ["query", "text"],
  ["queryid", "i64"],
  ["query_duration_us", "f64", "us"],
  ["transaction_duration_us", "f64", "us"],
  ["process_link", "text"],
  ["cpu", "f64", "ratio"],
  ["rss", "i64", "kib"],
  ["threads", "u64", "count"],
  ["read_bytes_per_second", "f64", "bytes_per_second"],
  ["write_bytes_per_second", "f64", "bytes_per_second"],
  ["command", "text"],
] as const;

const activityView = makeViewSpec({
  code: "activity",
  capabilities: { detail: true, history: true, related: true },
  columns: columns.map(([code, type, unit]) => ({
    code,
    type,
    ...(unit === undefined ? {} : { unit }),
    lazy: code === "query",
    requires: [],
    availability: "available" as const,
  })),
});

function point() {
  return makeEntityPointResponse({
    view: "activity",
    entity: "activity:AQACKwAAAAgZ5adgJlgGAA",
    label: "pid 18422",
    fields: [
      { code: "pid", value: 18422 },
      { code: "user", value: "app_rw" },
      { code: "database", value: "orders" },
      { code: "application", value: "checkout-api" },
      { code: "backend_type", value: "client backend" },
      { code: "state", value: "active" },
      { code: "wait_event", value: "Lock:transactionid" },
      {
        code: "query",
        value: "UPDATE orders SET status=$1 WHERE id=$2 RETURNING id",
      },
      { code: "queryid", value: "9180220441127101" },
      { code: "query_duration_us", value: 128_000_000 },
      { code: "transaction_duration_us", value: 194_000_000 },
      { code: "process_link", value: "Linked process" },
      { code: "cpu", value: 0.42 },
      { code: "rss", value: 354_611 },
      { code: "threads", value: 26 },
      { code: "read_bytes_per_second", value: 18_400_000 },
      { code: "write_bytes_per_second", value: 4_800_000 },
      { code: "command", value: "/usr/lib/postgresql/17/bin/postgres" },
    ],
    related: [
      {
        relation: "activity_process",
        view: "processes",
        entity: "process:18422:a",
        snapshot_ts_us: "1722400000000000",
        provenance: {
          kind: "best_effort",
          method: "pid",
          fields: ["pid"],
        },
      },
      {
        relation: "activity_process",
        view: "processes",
        entity: "process:18422:b",
        snapshot_ts_us: "1722399990000000",
        provenance: {
          kind: "best_effort",
          method: "pid_neighbor",
          fields: ["pid"],
        },
      },
    ],
  });
}

function history() {
  return makeEntityHistoryResponse({
    view: "activity",
    entity: "activity:AQACKwAAAAgZ5adgJlgGAA",
    label: "pid 18422",
    columns: [
      "state",
      "wait_event",
      "query_duration_us",
      "transaction_duration_us",
      "cpu",
      "rss",
      "read_bytes_per_second",
      "write_bytes_per_second",
    ],
    snapshots: [
      {
        ts_us: "1722396400000000",
        present: true,
        values: [
          "active",
          null,
          12_000_000,
          48_000_000,
          0.11,
          332_000,
          2_400_000,
          900_000,
        ],
      },
      {
        ts_us: "1722398200000000",
        present: true,
        values: [
          "active",
          "Client:ClientRead",
          64_000_000,
          118_000_000,
          0.28,
          346_000,
          12_600_000,
          2_100_000,
        ],
      },
      {
        ts_us: "1722400000000000",
        present: true,
        values: [
          "active",
          "Lock:transactionid",
          128_000_000,
          194_000_000,
          0.42,
          354_611,
          18_400_000,
          4_800_000,
        ],
      },
    ],
  });
}

test("selected Activity observation becomes one bounded PostgreSQL and OS canvas", async () => {
  await i18n.changeLanguage("en");
  const requests: URL[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      requests.push(url);
      return Promise.resolve(
        response(url.searchParams.has("at") ? point() : history()),
      );
    }),
  );

  render(
    <ActivityDetail
      view={activityView}
      entity="activity:AQACKwAAAAgZ5adgJlgGAA"
      at="1722400000000000"
      span={86_400}
      onClose={() => {}}
      onOpenEntity={() => {}}
      onFindStatements={() => {}}
      onOpenWaits={() => {}}
    />,
    { wrapper },
  );

  const detail = screen.getByTestId("activity-detail");
  expect(screen.getByTestId("activity-entity-strip")).toBeDefined();
  const temporal = await screen.findByTestId("activity-temporal-field");
  expect(
    within(temporal).getAllByTestId("activity-temporal-lane"),
  ).toHaveLength(4);
  expect(
    within(temporal).getAllByTestId("activity-observation-cell"),
  ).toHaveLength(3);
  expect(screen.getByTestId("activity-postgres-observation")).toBeDefined();
  expect(screen.getByTestId("activity-snapshot-matrix")).toBeDefined();
  expect(screen.getByTestId("activity-related-evidence")).toBeDefined();
  expect(
    screen.getByText("UPDATE orders SET status=$1 WHERE id=$2 RETURNING id"),
  ).toBeDefined();
  expect(screen.getAllByText("Lock:transactionid").length).toBeGreaterThan(0);

  await waitFor(() => expect(requests).toHaveLength(2));
  const historyRequest = requests.find((url) => url.searchParams.has("from"));
  expect(historyRequest?.searchParams.get("limit")).toBe("96");
  expect(
    (historyRequest?.searchParams.get("columns") ?? "").split(","),
  ).toEqual([
    "state",
    "wait_event",
    "query_duration_us",
    "transaction_duration_us",
    "cpu",
    "rss",
    "read_bytes_per_second",
    "write_bytes_per_second",
  ]);
  expect(
    BigInt(historyRequest?.searchParams.get("to") ?? "0") -
      BigInt(historyRequest?.searchParams.get("from") ?? "0"),
  ).toBe(21_600_000_000n);
  expect(detail.textContent).not.toMatch(
    /AQACKw|activity:|process:18422|\/v1\/|gaps|gated|proof|provenance|best_effort|pid_neighbor/i,
  );
});

test("every returned process and the query and wait continuations stay actionable", async () => {
  await i18n.changeLanguage("en");
  const onOpenEntity = vi.fn();
  const onFindStatements = vi.fn();
  const onOpenWaits = vi.fn();
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      return Promise.resolve(
        response(url.searchParams.has("at") ? point() : history()),
      );
    }),
  );

  render(
    <ActivityDetail
      view={activityView}
      entity="activity:AQACKwAAAAgZ5adgJlgGAA"
      at="1722400000000000"
      span={3_600}
      onClose={() => {}}
      onOpenEntity={onOpenEntity}
      onFindStatements={onFindStatements}
      onOpenWaits={onOpenWaits}
    />,
    { wrapper },
  );

  const related = await screen.findByTestId("activity-related-evidence");
  const processButtons = within(related).getAllByRole("button", {
    name: /Open related process/i,
  });
  expect(processButtons).toHaveLength(2);
  const [firstProcess, secondProcess] = processButtons;
  expect(firstProcess).toBeDefined();
  expect(secondProcess).toBeDefined();
  if (firstProcess === undefined || secondProcess === undefined) return;
  fireEvent.click(firstProcess);
  fireEvent.click(secondProcess);
  expect(onOpenEntity).toHaveBeenNthCalledWith(
    1,
    "processes",
    "process:18422:a",
    "1722400000000000",
  );
  expect(onOpenEntity).toHaveBeenNthCalledWith(
    2,
    "processes",
    "process:18422:b",
    "1722399990000000",
  );

  fireEvent.click(
    within(related).getByRole("button", {
      name: "Find this query in Statements",
    }),
  );
  expect(onFindStatements).toHaveBeenCalledWith("9180220441127101");

  fireEvent.click(
    within(related).getByRole("button", {
      name: "Open this PID in waits & locks",
    }),
  );
  expect(onOpenWaits).toHaveBeenCalledWith(18422);
});
