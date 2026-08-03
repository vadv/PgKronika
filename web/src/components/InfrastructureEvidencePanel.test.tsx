import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeContextResponse,
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { InfrastructureEvidencePanel } from "./InfrastructureEvidencePanel";

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

test("Tables panel fetches only a bounded active-vacuum lane set", async () => {
  const onOpenEntity = vi.fn();
  const fetchMock = vi.fn().mockResolvedValue(
    response(
      makeFrameResponse({
        view: "vacuum",
        snapshot_ts_us: "1722399999000000",
        columns: [
          makeFrameColumn({ code: "relation", type: "text" }),
          makeFrameColumn({ code: "phase", type: "text" }),
          makeFrameColumn({ code: "progress", type: "f64", unit: "ratio" }),
        ],
        rows: [
          makeFrameRow({
            entity: "vacuum:7",
            label: "public.orders",
            cells: ["public.orders", "scanning heap", 0.42],
          }),
        ],
        page: { matched: 1, returned: 1, next: null },
      }),
    ),
  );
  vi.stubGlobal("fetch", fetchMock);
  render(
    <InfrastructureEvidencePanel
      view={makeViewSpec({
        code: "tables",
        joins: [
          {
            left: "tables",
            right: "vacuum",
            kind: "temporal",
            fields: ["datid", "relid", "ts"],
            cardinality: "zero_or_many",
            provenance: "same_snapshot_database_relation_oid",
          },
        ],
      })}
      preset="vacuum_risk"
      at="1722400000000000"
      span={3600}
      from="1722396400000000"
      to="1722400000000000"
      context={undefined}
      onOpenEntity={onOpenEntity}
    />,
    { wrapper },
  );

  const lane = await screen.findByRole("button", { name: /public.orders/i });
  fireEvent.click(lane);
  expect(onOpenEntity).toHaveBeenCalledWith("vacuum", "vacuum:7");
  const url = new URL((fetchMock.mock.calls[0]?.[0] as Request).url);
  expect(url.pathname).toBe("/v1/frame/vacuum");
  expect(url.searchParams.get("limit")).toBe("3");
  expect(url.searchParams.get("preset")).toBe("progress");
  const panel = screen.getByTestId("infrastructure-evidence-panel");
  expect(panel.getAttribute("data-snapshot-ts")).toBe("1722399999000000");
  expect(panel.getAttribute("data-snapshot-match")).toBe("false");
  expect(panel.getAttribute("data-snapshot-delta-us")).toBe("1000000");
  expect(panel.getAttribute("data-snapshot-provenance")).toBeNull();
  expect(panel.textContent).not.toMatch(
    /independent|not joined|lifetime|provenance/i,
  );
});

test("Index and Vacuum panels disclose temporal context and lifetime limits", () => {
  const fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  const { rerender } = render(
    <InfrastructureEvidencePanel
      view={makeViewSpec({
        code: "indexes",
        joins: [
          {
            left: "indexes",
            right: "tables",
            kind: "temporal",
            fields: ["datid", "relid", "ts"],
            cardinality: "zero_or_one",
            provenance: "same_snapshot_database_relation_oid",
          },
        ],
      })}
      preset="table_context"
      at="1722400000000000"
      span={3600}
      from="1722396400000000"
      to="1722400000000000"
      context={undefined}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );
  expect(screen.queryByTestId("index-table-provenance")).toBeNull();
  expect(
    screen.getByTestId("infrastructure-evidence-panel").textContent,
  ).not.toMatch(
    /same_snapshot_database_relation_oid|best_effort|temporal|proof|claim/i,
  );

  rerender(
    <InfrastructureEvidencePanel
      view={makeViewSpec({
        code: "vacuum",
        columns: [
          {
            availability: "available",
            code: "dead_tuples",
            lazy: false,
            requires: [],
            type: "i64",
          },
          {
            availability: "available",
            code: "dead_item_ids",
            lazy: false,
            requires: [],
            type: "i64",
          },
        ],
        capabilities: {
          detail: true,
          history: false,
          related: true,
        },
      })}
      preset="progress"
      at="1722400000000000"
      span={3600}
      from="1722396400000000"
      to="1722400000000000"
      context={makeContextResponse({ instance: { pg_version_num: 160004 } })}
      onOpenEntity={() => {}}
    />,
  );
  expect(screen.queryByTestId("vacuum-lifetime-warning")).toBeNull();
  expect(screen.getByTestId("vacuum-context-summary")).toBeDefined();
  expect(
    screen.getByTestId("infrastructure-evidence-panel").textContent,
  ).not.toMatch(/provenance|proof|lifetime|PID reuse|datid|relid/i);
  expect(
    screen.getByTestId("vacuum-pre17-generation").getAttribute("data-status"),
  ).toBe("available");
  expect(
    screen.getByTestId("vacuum-pg17-generation").getAttribute("data-status"),
  ).toBe("not_applicable");
  expect(fetchMock).not.toHaveBeenCalled();
});
