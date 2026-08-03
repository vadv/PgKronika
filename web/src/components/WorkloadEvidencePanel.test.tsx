import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
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

test("Activity always identifies point evidence and fetches bounded lock lanes only for Waits & Locks", async () => {
  const fetchMock = vi.fn((input: RequestInfo | URL) => {
    void input;
    return Promise.resolve(
      response(
        makeFrameResponse({
          view: "locks",
          columns: [
            makeFrameColumn({ code: "pid", type: "i64" }),
            makeFrameColumn({ code: "blocked_by", type: "text" }),
            makeFrameColumn({ code: "wait_age_us", type: "f64" }),
            makeFrameColumn({ code: "target", type: "text" }),
          ],
          rows: [
            makeFrameRow({
              entity: "lock:18422",
              label: "pid 18422",
              cells: [18422, "18111", 1_200_000, "public.orders"],
            }),
          ],
          page: { matched: 1, returned: 1, next: null },
        }),
      ),
    );
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <WorkloadEvidencePanel
      view={makeViewSpec({ code: "activity" })}
      preset="waits_locks"
      at="1722400000000000"
      span={3600}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );

  expect(screen.getByTestId("activity-point-evidence")).toBeDefined();
  expect(await screen.findByText(/18422.*18111/)).toBeDefined();
  await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
  const url = new URL((fetchMock.mock.calls[0]?.[0] as Request).url);
  expect(url.pathname).toBe("/v1/frame/locks");
  expect(url.searchParams.get("preset")).toBe("tree");
  expect(url.searchParams.get("limit")).toBe("6");
  expect(url.searchParams.get("span")).toBe("3600s");
});

test("Activity non-lock lenses do not spend a lock-frame request", () => {
  const fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  render(
    <WorkloadEvidencePanel
      view={makeViewSpec({ code: "activity" })}
      preset="cpu"
      at="1722400000000000"
      span={3600}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );
  expect(screen.getByTestId("activity-process-evidence")).toBeDefined();
  expect(fetchMock).not.toHaveBeenCalled();
});

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
  expect(url.searchParams.get("limit")).toBe("5");
});
