import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeContextResponse,
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeSpineResponse,
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

test("OS panel keeps host pressure, process evidence and quality scopes separate", async () => {
  const fetchMock = vi.fn().mockResolvedValue(
    response(
      makeSpineResponse({
        series: [
          {
            code: "load_per_cpu",
            unit: "ratio",
            aggregation: "max",
            values: [0.4, 1.2],
          },
          {
            code: "psi_io_some",
            unit: "percent",
            aggregation: "max",
            values: [4, 18],
          },
        ],
        quality: {
          status: "partial",
          snapshots: 2,
          gaps: [],
          gated: [],
          resource_limited: ["host_signals"],
          active_tail: true,
        },
      }),
    ),
  );
  vi.stubGlobal("fetch", fetchMock);
  render(
    <InfrastructureEvidencePanel
      view={makeViewSpec({ code: "processes", scope: "host" })}
      preset="pressure"
      at="1722400000000000"
      span={3600}
      from="1722396400000000"
      to="1722400000000000"
      context={makeContextResponse({
        host: { logical_cpu_count: 32, kernel_version: "6.8.0" },
      })}
      onOpenEntity={() => {}}
    />,
    { wrapper },
  );

  expect(await screen.findByTestId("host-pressure-evidence")).toBeDefined();
  expect(screen.getByText(/load.*CPU/i)).toBeDefined();
  expect(screen.getByText(/psiIo/i)).toBeDefined();
  expect(screen.getByTestId("host-scope-guard").getAttribute("data-cpus")).toBe(
    "32",
  );
  await waitFor(() => {
    expect(screen.getByTestId("host-pressure-evidence").textContent).toContain(
      "1.2",
    );
    expect(
      screen.getByTestId("host-quality").getAttribute("data-limited"),
    ).toBe("1");
  });
  const url = new URL((fetchMock.mock.calls[0]?.[0] as Request).url);
  expect(url.pathname).toBe("/v1/timeline/spine");
  expect(url.searchParams.get("buckets")).toBe("24");
});

test("Tables panel fetches only a bounded active-vacuum lane set", async () => {
  const onOpenEntity = vi.fn();
  const fetchMock = vi.fn().mockResolvedValue(
    response(
      makeFrameResponse({
        view: "vacuum",
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
  expect(screen.getByTestId("index-table-provenance").textContent).toContain(
    "same_snapshot_database_relation_oid",
  );

  rerender(
    <InfrastructureEvidencePanel
      view={makeViewSpec({
        code: "vacuum",
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
      context={undefined}
      onOpenEntity={() => {}}
    />,
  );
  expect(screen.getByTestId("vacuum-lifetime-warning")).toBeDefined();
  expect(fetchMock).not.toHaveBeenCalled();
});
