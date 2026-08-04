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
import { StatementDetail } from "./StatementDetail";

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
  ["queryid", "i64"],
  ["query", "text"],
  ["database", "text"],
  ["user", "text"],
  ["calls", "f64"],
  ["total", "f64", "duration_ms"],
  ["ms_per_row", "f64", "ms"],
  ["mean", "f64", "ms"],
  ["time_pct", "f64", "percent"],
  ["plan_time_pct", "f64", "percent"],
  ["rows", "f64", "count"],
  ["hit_pct", "f64", "percent"],
  ["blks_read", "f64", "blocks"],
  ["temp_written", "f64", "blocks"],
  ["wal_bytes", "f64", "bytes"],
] as const;

const statementView = makeViewSpec({
  code: "statements",
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
    view: "statements",
    entity: "stmt:7101",
    label: "9180220441127101",
    fields: [
      { code: "queryid", value: "9180220441127101" },
      { code: "query", value: "UPDATE orders SET status=$1 WHERE id=$2" },
      { code: "database", value: "orders" },
      { code: "user", value: "app_rw" },
      { code: "calls", value: 12_400_000 },
      { code: "total", value: 8_420_000 },
      { code: "ms_per_row", value: 0.42 },
      { code: "mean", value: 679 },
      { code: "time_pct", value: 31.5 },
      { code: "plan_time_pct", value: 1.2 },
      { code: "rows", value: 9_920_000 },
      { code: "hit_pct", value: 99.4 },
      { code: "blks_read", value: 420_000 },
      { code: "temp_written", value: 0 },
      { code: "wal_bytes", value: 8_800_000 },
    ],
    related: [
      {
        relation: "statement_plan",
        view: "plans",
        entity: "plan:84102200",
        snapshot_ts_us: "1722400000000000",
        provenance: {
          kind: "best_effort",
          method: "ossc_queryid_dbid_userid_attribution",
          fields: ["queryid", "dbid", "userid"],
        },
      },
    ],
  });
}

function history() {
  return makeEntityHistoryResponse({
    view: "statements",
    entity: "stmt:7101",
    label: "9180220441127101",
    columns: [
      "total",
      "calls",
      "mean",
      "blks_read",
      "wal_bytes",
      "temp_written",
    ],
    snapshots: [
      {
        ts_us: "1722396400000000",
        values: [4_100_000, 8_200_000, 500, 260_000, 4_200_000, 0],
      },
      {
        ts_us: "1722398200000000",
        values: [6_320_000, 10_100_000, 626, 330_000, 6_100_000, 140],
      },
      {
        ts_us: "1722400000000000",
        values: [8_420_000, 12_400_000, 679, 420_000, 8_800_000, 480],
      },
    ],
  });
}

test("selected statement becomes a bounded full-canvas forensic workspace", async () => {
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
    <StatementDetail
      view={statementView}
      entity="stmt:7101"
      at="1722400000000000"
      span={86_400}
      onClose={() => {}}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );

  const detail = screen.getByTestId("statement-detail");
  expect(screen.getByTestId("statement-entity-strip")).toBeDefined();
  const temporal = await screen.findByTestId("statement-temporal-field");
  expect(
    within(temporal).getAllByTestId("statement-temporal-lane"),
  ).toHaveLength(4);
  expect(screen.getByTestId("statement-impact-center")).toBeDefined();
  expect(screen.getByTestId("statement-history-matrix")).toBeDefined();
  expect(screen.getByTestId("statement-related-evidence")).toBeDefined();
  expect(
    screen.getByText("UPDATE orders SET status=$1 WHERE id=$2"),
  ).toBeDefined();
  expect(screen.getByText("12.4M × 679 ms")).toBeDefined();
  expect(screen.getByText("2.34 h total impact")).toBeDefined();

  await waitFor(() => expect(requests.length).toBe(2));
  const historyRequest = requests.find((url) => url.searchParams.has("from"));
  expect(historyRequest?.searchParams.get("limit")).toBe("96");
  expect(
    (historyRequest?.searchParams.get("columns") ?? "").split(","),
  ).toEqual([
    "total",
    "calls",
    "mean",
    "blks_read",
    "wal_bytes",
    "temp_written",
  ]);
  expect(
    BigInt(historyRequest?.searchParams.get("to") ?? "0") -
      BigInt(historyRequest?.searchParams.get("from") ?? "0"),
  ).toBe(21_600_000_000n);
  expect(detail.textContent).not.toMatch(
    /stmt:7101|\/v1\/|gaps|gated|proof|provenance|ossc_queryid/i,
  );
});

test("related plans are calm investigation links and missing values stay honest", async () => {
  await i18n.changeLanguage("en");
  const onOpenEntity = vi.fn();
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      const body = url.searchParams.has("at")
        ? makeEntityPointResponse({
            ...point(),
            fields: point().fields.map((field) =>
              field.code === "plan_time_pct"
                ? { ...field, value: null, status: "not_collected" }
                : field,
            ),
          })
        : history();
      return Promise.resolve(response(body));
    }),
  );

  render(
    <StatementDetail
      view={statementView}
      entity="stmt:7101"
      at="1722400000000000"
      span={3_600}
      onClose={() => {}}
      onOpenEntity={onOpenEntity}
    />,
    { wrapper },
  );

  const related = await screen.findByRole("button", {
    name: "Open related Plans evidence",
  });
  fireEvent.click(related);
  expect(onOpenEntity).toHaveBeenCalledWith(
    "plans",
    "plan:84102200",
    "1722400000000000",
  );
  expect(screen.getByText("not collected")).toBeDefined();
  expect(screen.getByTestId("statement-detail").textContent).not.toMatch(
    /best_effort|attribution|method|fields/i,
  );
});
