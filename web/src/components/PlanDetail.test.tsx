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
import { PlanDetail } from "./PlanDetail";

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
  ["planid", "i64"],
  ["queryid", "i64"],
  ["plan", "text"],
  ["calls", "f64", "count"],
  ["mean", "f64", "ms"],
  ["rows", "f64", "count"],
  ["shared_hit", "f64", "blocks"],
  ["shared_read", "f64", "blocks"],
  ["first_call", "timestamp"],
  ["last_call", "timestamp"],
] as const;

const planView = makeViewSpec({
  code: "plans",
  capabilities: { detail: true, history: true, related: true },
  columns: columns.map(([code, type, unit]) => ({
    code,
    type,
    ...(unit === undefined ? {} : { unit }),
    lazy: code === "plan",
    requires: [],
    availability: "available" as const,
  })),
});

function point() {
  return makeEntityPointResponse({
    view: "plans",
    entity: "plan:84102200",
    label: "84102200",
    fields: [
      { code: "planid", value: "84102200" },
      { code: "queryid", value: "9180220441127101" },
      {
        code: "plan",
        value:
          "Update on orders  (cost=0.43..8.45 rows=0 width=0)\n  ->  Index Scan using orders_pkey on orders  (cost=0.43..8.45 rows=1 width=38)\n        Index Cond: (id = $2)",
      },
      { code: "calls", value: 18_420 },
      { code: "mean", value: 6.8 },
      { code: "rows", value: 18_210 },
      { code: "shared_hit", value: 982_400 },
      { code: "shared_read", value: 12_680 },
      { code: "first_call", value: "1722396400000000" },
      { code: "last_call", value: "1722400000000000" },
    ],
    related: [
      {
        relation: "plan_statement",
        view: "statements",
        entity: "stmt:7101:a",
        snapshot_ts_us: "1722400000000000",
        provenance: {
          kind: "best_effort",
          method: "ossc_queryid_dbid_userid_attribution",
          fields: ["queryid", "dbid", "userid"],
        },
      },
      {
        relation: "plan_statement",
        view: "statements",
        entity: "stmt:7101:b",
        snapshot_ts_us: "1722399990000000",
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
    view: "plans",
    entity: "plan:84102200",
    label: "84102200",
    columns: ["calls", "mean", "rows", "shared_hit", "shared_read"],
    snapshots: [
      {
        ts_us: "1722396400000000",
        values: [4_200, 4.4, 4_120, 420_000, 1_800],
      },
      {
        ts_us: "1722398200000000",
        values: [11_200, 5.1, 11_000, 710_000, 4_600],
      },
      {
        ts_us: "1722400000000000",
        values: [18_420, 6.8, 18_210, 982_400, 12_680],
      },
    ],
  });
}

test("selected Plan becomes a bounded full-canvas forensic workspace", async () => {
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
    <PlanDetail
      view={planView}
      entity="plan:84102200"
      at="1722400000000000"
      span={86_400}
      onClose={() => {}}
      onOpenEntity={() => {}}
      onFindStatements={() => {}}
      onFindPlans={() => {}}
    />,
    { wrapper },
  );

  const detail = screen.getByTestId("plan-detail");
  expect(screen.getByTestId("plan-entity-strip")).toBeDefined();
  const temporal = await screen.findByTestId("plan-temporal-field");
  expect(within(temporal).getAllByTestId("plan-temporal-lane")).toHaveLength(4);
  expect(within(temporal).getAllByTestId("plan-observation-cell")).toHaveLength(
    3,
  );
  expect(screen.getByTestId("plan-body-evidence")).toBeDefined();
  expect(screen.getByTestId("plan-metric-matrix")).toBeDefined();
  expect(screen.getByTestId("plan-related-evidence")).toBeDefined();
  expect(screen.getAllByText(/Index Scan using orders_pkey/).length).toBe(1);

  await waitFor(() => expect(requests).toHaveLength(2));
  const historyRequest = requests.find((url) => url.searchParams.has("from"));
  expect(historyRequest?.searchParams.get("limit")).toBe("96");
  expect(
    (historyRequest?.searchParams.get("columns") ?? "").split(","),
  ).toEqual(["calls", "mean", "rows", "shared_hit", "shared_read"]);
  expect(
    BigInt(historyRequest?.searchParams.get("to") ?? "0") -
      BigInt(historyRequest?.searchParams.get("from") ?? "0"),
  ).toBe(21_600_000_000n);
  expect(detail.textContent).not.toMatch(
    /plan:84102200|stmt:7101|\/v1\/|gaps|gated|proof|provenance|best_effort|attribution/i,
  );
});

test("all Statement candidates and both query continuations stay actionable", async () => {
  await i18n.changeLanguage("en");
  const onOpenEntity = vi.fn();
  const onFindStatements = vi.fn();
  const onFindPlans = vi.fn();
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
    <PlanDetail
      view={planView}
      entity="plan:84102200"
      at="1722400000000000"
      span={3_600}
      onClose={() => {}}
      onOpenEntity={onOpenEntity}
      onFindStatements={onFindStatements}
      onFindPlans={onFindPlans}
    />,
    { wrapper },
  );

  const related = await screen.findByTestId("plan-related-evidence");
  const statementButtons = within(related).getAllByRole("button", {
    name: /Open related Statement/i,
  });
  expect(statementButtons).toHaveLength(2);
  statementButtons.forEach((button) => fireEvent.click(button));
  expect(onOpenEntity).toHaveBeenNthCalledWith(
    1,
    "statements",
    "stmt:7101:a",
    "1722400000000000",
  );
  expect(onOpenEntity).toHaveBeenNthCalledWith(
    2,
    "statements",
    "stmt:7101:b",
    "1722399990000000",
  );

  fireEvent.click(
    within(related).getByRole("button", {
      name: "Find this query in Statements",
    }),
  );
  fireEvent.click(
    within(related).getByRole("button", {
      name: "Show other Plans for this query",
    }),
  );
  expect(onFindStatements).toHaveBeenCalledWith("9180220441127101");
  expect(onFindPlans).toHaveBeenCalledWith("9180220441127101");
});
