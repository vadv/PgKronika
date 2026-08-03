import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { WorkloadEvidencePanel } from "./WorkloadEvidencePanel";

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

test("Plans change timeline is bounded and publishes both fork attribution methods", async () => {
  const fetchMock = vi.fn().mockResolvedValue(
    response(
      makeFrameResponse({
        view: "plans",
        columns: [
          makeFrameColumn({ code: "planid", type: "i64" }),
          makeFrameColumn({ code: "queryid", type: "i64" }),
          makeFrameColumn({ code: "first_call", type: "timestamp" }),
          makeFrameColumn({ code: "last_call", type: "timestamp" }),
        ],
        rows: [
          makeFrameRow({
            entity: "plan:77",
            label: "77",
            cells: [77, 42, "1722390000000000", "1722400000000000"],
          }),
        ],
        page: { matched: 1, returned: 1, next: null },
      }),
    ),
  );
  vi.stubGlobal("fetch", fetchMock);
  render(
    <WorkloadEvidencePanel
      view={makeViewSpec({
        code: "plans",
        joins: [
          {
            left: "plans",
            right: "statements",
            kind: "best_effort",
            fields: ["queryid", "dbid", "userid"],
            cardinality: "many_to_one",
            provenance: "ossc_queryid_dbid_userid_attribution",
          },
          {
            left: "plans",
            right: "statements",
            kind: "best_effort",
            fields: ["queryid_stat_statements", "dbid", "userid"],
            cardinality: "many_to_one",
            provenance: "vadv_queryid_stat_statements_dbid_userid_attribution",
          },
        ],
      })}
      preset="change_timeline"
      at="1722400000000000"
      span={3600}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );

  expect(
    screen.getByText("ossc_queryid_dbid_userid_attribution"),
  ).toBeDefined();
  expect(
    screen.getByText("vadv_queryid_stat_statements_dbid_userid_attribution"),
  ).toBeDefined();
  expect(await screen.findByRole("button", { name: /77/ })).toBeDefined();
  const url = new URL((fetchMock.mock.calls[0]?.[0] as Request).url);
  expect(url.pathname).toBe("/v1/frame/plans");
  expect(url.searchParams.get("preset")).toBe("change_timeline");
  expect(url.searchParams.get("limit")).toBe("3");
});
