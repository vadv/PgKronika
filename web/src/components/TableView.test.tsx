import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { ColumnSpec } from "../api/types";
import {
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { TableView, type TableViewProps } from "./TableView";

afterEach(() => vi.unstubAllGlobals());

function wrapper({ children }: { children: ReactNode }) {
  return createElement(
    QueryClientProvider,
    { client: new QueryClient() },
    children,
  );
}

const columns: ColumnSpec[] = [
  {
    availability: "available",
    code: "xact",
    lazy: false,
    requires: [],
    type: "i64",
  },
  {
    availability: "available",
    code: "query",
    lazy: false,
    requires: [],
    type: "text",
  },
];

function stubFrame(body: unknown) {
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

function frameBody() {
  return makeFrameResponse({
    view: "activity",
    columns: [
      makeFrameColumn({ code: "xact", type: "i64" }),
      makeFrameColumn({ code: "query", type: "text" }),
    ],
    rows: [
      makeFrameRow({
        entity: "db:1",
        cells: [5, "select 1"],
        spark: { complete: true, values: [1, 2, 3] },
      }),
    ],
    page: { matched: 1, returned: 1 },
  });
}

function renderTable(overrides: Partial<TableViewProps> = {}) {
  const props: TableViewProps = {
    view: makeViewSpec({ code: "activity", columns }),
    at: "1722400000000000",
    span: 3600,
    preset: null,
    q: null,
    sort: null,
    order: null,
    entity: null,
    onSort: () => {},
    onSelectRow: () => {},
    ...overrides,
  };
  return render(<TableView {...props} />, { wrapper });
}

test("renders frame rows once loaded and reports the matched count", async () => {
  const onMatched = vi.fn();
  stubFrame(frameBody());
  renderTable({ onMatched });
  await waitFor(() => expect(screen.getByText("select 1")).toBeDefined());
  expect(screen.getByText("5")).toBeDefined();
  expect(screen.getByRole("table", { name: "activity" })).toBeDefined();
  await waitFor(() => expect(onMatched).toHaveBeenCalledWith(1));
});

test("sort header click cycles desc, asc, cleared", async () => {
  const onSort = vi.fn();
  stubFrame(frameBody());
  const element = (sort: string | null, order: "asc" | "desc" | null) => (
    <TableView
      view={makeViewSpec({ code: "activity", columns })}
      at="1722400000000000"
      span={3600}
      preset={null}
      q={null}
      sort={sort}
      order={order}
      entity={null}
      onSort={onSort}
      onSelectRow={() => {}}
    />
  );
  const { rerender } = render(element(null, null), { wrapper });
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "xact" })).toBeDefined(),
  );
  fireEvent.click(screen.getByRole("button", { name: "xact" }));
  expect(onSort).toHaveBeenCalledWith("xact", "desc");

  rerender(element("xact", "desc"));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "xact ↓" })).toBeDefined(),
  );
  fireEvent.click(screen.getByRole("button", { name: "xact ↓" }));
  expect(onSort).toHaveBeenCalledWith("xact", "asc");

  rerender(element("xact", "asc"));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "xact ↑" })).toBeDefined(),
  );
  fireEvent.click(screen.getByRole("button", { name: "xact ↑" }));
  expect(onSort).toHaveBeenCalledWith(null, null);
});

test("row click and Enter select the entity", async () => {
  const onSelectRow = vi.fn();
  stubFrame(frameBody());
  renderTable({ onSelectRow });
  await waitFor(() => expect(screen.getByText("select 1")).toBeDefined());
  const row = screen.getByText("select 1").closest("tr");
  expect(row).not.toBeNull();
  fireEvent.click(row as HTMLElement);
  expect(onSelectRow).toHaveBeenCalledWith("db:1");
  fireEvent.keyDown(row as HTMLElement, { key: "Enter" });
  expect(onSelectRow).toHaveBeenCalledTimes(2);
});

test("shows the empty state when the frame has no rows", async () => {
  stubFrame(makeFrameResponse({ rows: [] }));
  renderTable();
  await waitFor(() => expect(screen.getByText("table.empty")).toBeDefined());
});

test("verdict cell carries level color and the mechanical why in the title", async () => {
  stubFrame(
    makeFrameResponse({
      columns: [makeFrameColumn({ code: "xact", type: "i64" })],
      rows: [
        makeFrameRow({
          cells: [42],
          classifications: [
            {
              column: "xact",
              metric: "xact_age",
              result: {
                level: "critical",
                status: "classified",
                boundary: { operator: ">", value: 10 },
                evidence: { kind: "scalar", observed: 42 },
              },
            },
          ],
          cell_statuses: [{ status: "available", reason: null }],
        }),
      ],
      page: { matched: 1, returned: 1 },
    }),
  );
  renderTable();
  const cell = await screen.findByText("42");
  expect(cell.style.color).toBe("var(--sev-crit-fg)");
  expect(cell.style.background).toBe("var(--sev-crit-bg)");
  expect(cell.getAttribute("title")).toBe("critical: > 10 · observed 42");
});

test("410 cursor expiry refetches the first page automatically", async () => {
  const page1 = () =>
    makeFrameResponse({
      columns: [makeFrameColumn({ code: "xact", type: "i64" })],
      rows: [makeFrameRow({ entity: "db:1", cells: [5] })],
      page: { matched: 2, returned: 1, next: "cursor-1" },
    });
  const fetchMock = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    if (url.includes("cursor=cursor-1")) {
      return Promise.resolve(
        new Response(JSON.stringify({ code: "cursor_expired" }), {
          status: 410,
          headers: { "content-type": "application/json" },
        }),
      );
    }
    return Promise.resolve(
      new Response(JSON.stringify(page1()), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
  });
  vi.stubGlobal("fetch", fetchMock);
  // No retries: the expiry must surface immediately, not after backoff.
  const noRetryWrapper = ({ children }: { children: ReactNode }) =>
    createElement(
      QueryClientProvider,
      {
        client: new QueryClient({
          defaultOptions: { queries: { retry: false } },
        }),
      },
      children,
    );
  const props: TableViewProps = {
    view: makeViewSpec({ code: "activity", columns }),
    at: "1722400000000000",
    span: 3600,
    preset: null,
    q: null,
    sort: null,
    order: null,
    entity: null,
    onSort: () => {},
    onSelectRow: () => {},
  };
  render(<TableView {...props} />, { wrapper: noRetryWrapper });
  await waitFor(() => expect(screen.getByText("5")).toBeDefined());
  fireEvent.click(screen.getByRole("button", { name: /table.more/ }));
  // The expiry notice appears while the fresh first page is requested…
  await waitFor(() =>
    expect(screen.getByText("table.cursor_expired")).toBeDefined(),
  );
  // …and is replaced by the restored first page of the same intent.
  await waitFor(() =>
    expect(screen.queryByText("table.cursor_expired")).toBeNull(),
  );
  expect(screen.getByText("5")).toBeDefined();
  const cursorCalls = fetchMock.mock.calls.filter(([u]) => {
    const url =
      typeof u === "string" ? u : u instanceof Request ? u.url : u.href;
    return url.includes("cursor=cursor-1");
  }).length;
  expect(cursorCalls).toBe(1);
});
