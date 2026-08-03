import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { ColumnSpec, HeatmapResponse } from "../api/types";
import {
  makeFrameColumn,
  makeFrameResponse,
  makeFrameRow,
  makeHeatmapQuality,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { TableView, type TableViewProps } from "./TableView";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

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

test("exposes a bounded ranked matrix with an independent scroll body", async () => {
  stubFrame(frameBody());
  renderTable();
  await waitFor(() => expect(screen.getByText("select 1")).toBeDefined());

  const matrix = document.querySelector('[data-shell-region="ranked-matrix"]');
  const body = screen.getByTestId("ranked-matrix-body");
  expect(matrix).not.toBeNull();
  expect(body.style.minHeight).toBe("0");
  expect(body.style.overflow).toBe("auto");
  expect(body.querySelector("table")).not.toBeNull();
  expect(body.querySelector("tbody tr")?.getAttribute("style")).toContain(
    "height: 28px",
  );
});

test("virtualizes a thousand loaded rows and moves the DOM window on scroll", async () => {
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(280);
  stubFrame(
    makeFrameResponse({
      view: "statements",
      columns: [makeFrameColumn({ code: "xact", type: "i64" })],
      rows: Array.from({ length: 1_000 }, (_, index) =>
        makeFrameRow({ entity: `stmt:${index}`, cells: [index] }),
      ),
      page: { matched: 1_000, returned: 1_000 },
    }),
  );
  renderTable({
    view: makeViewSpec({ code: "statements", columns }),
  });

  const body = await screen.findByTestId("ranked-matrix-body");
  await waitFor(() =>
    expect(body.querySelectorAll("tr[data-entity]").length).toBeGreaterThan(0),
  );
  expect(body.dataset.loadedRows).toBe("1000");
  expect(body.querySelectorAll("tr[data-entity]").length).toBeLessThanOrEqual(
    24,
  );
  expect(body.querySelector('[data-entity="stmt:0"]')).not.toBeNull();
  const first = body.querySelector('[data-entity="stmt:0"]') as HTMLElement;
  first.focus();
  fireEvent.keyDown(first, { key: "ArrowDown" });
  await waitFor(() =>
    expect(document.activeElement?.getAttribute("data-entity")).toBe("stmt:1"),
  );

  fireEvent.scroll(body, { target: { scrollTop: 1_400 } });
  await waitFor(() =>
    expect(body.querySelector('[data-entity="stmt:0"]')).toBeNull(),
  );
  expect(body.querySelector('[data-entity="stmt:50"]')).not.toBeNull();
});

test("couples each virtualized statement row to its exact temporal evidence", async () => {
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(280);
  const statementColumns: ColumnSpec[] = [
    {
      availability: "available",
      code: "queryid",
      lazy: false,
      requires: [],
      type: "i64",
    },
    {
      availability: "available",
      code: "database",
      lazy: false,
      requires: [],
      type: "text",
    },
    {
      availability: "available",
      code: "user",
      lazy: false,
      requires: [],
      type: "text",
    },
    {
      availability: "available",
      code: "total",
      lazy: false,
      requires: [],
      type: "f64",
      unit: "duration_ms",
    },
  ];
  stubFrame(
    makeFrameResponse({
      view: "statements",
      columns: [
        makeFrameColumn({ code: "queryid", type: "i64" }),
        makeFrameColumn({ code: "database", type: "text" }),
        makeFrameColumn({ code: "user", type: "text" }),
        makeFrameColumn({
          code: "total",
          type: "f64",
          unit: "duration_ms",
        }),
      ],
      rows: Array.from({ length: 1_000 }, (_, index) =>
        makeFrameRow({
          entity: `stmt:${index}`,
          label: `statement ${index}`,
          cells: [String(10_000 + index), "orders", "app_rw", index + 1],
        }),
      ),
      page: { matched: 1_000, returned: 1_000 },
    }),
  );
  const heatmap: HeatmapResponse = {
    grid: { from_us: "100", to_us: "200", bucket_count: 96 },
    ranking: { exact: true, unseen_upper: 0 },
    quality: makeHeatmapQuality({ snapshots: 96 }),
    rows: [
      {
        entity: "stmt:0",
        label: "statement 0",
        unit: "ms",
        score: { lower: 0, upper: 95 },
        values: Array.from({ length: 96 }, (_, index) => index),
      },
    ],
  };

  renderTable({
    view: makeViewSpec({ code: "statements", columns: statementColumns }),
    timeMatrix: {
      data: heatmap,
      pending: false,
      error: false,
      metricLabel: "total time",
      cursorUs: "150",
      baselineUs: null,
      onRetry: () => {},
    },
  });

  const body = await screen.findByTestId("ranked-matrix-body");
  await waitFor(() =>
    expect(body.querySelectorAll("tr[data-entity]").length).toBeGreaterThan(1),
  );
  const renderedRows = body.querySelectorAll("tr[data-entity]").length;
  expect(screen.getAllByTestId("temporal-row")).toHaveLength(renderedRows);
  expect(renderedRows).toBeLessThanOrEqual(24);
  expect(
    body
      .querySelector('[data-entity="stmt:0"]')
      ?.querySelector('[data-evidence="available"]'),
  ).not.toBeNull();
  expect(
    body
      .querySelector('[data-entity="stmt:1"]')
      ?.querySelector('[data-evidence="unavailable"]'),
  ).not.toBeNull();
  expect(
    screen.getAllByTestId("time-matrix-bucket").length,
  ).toBeLessThanOrEqual(24 * 96);
  expect(screen.queryByText("table.spark")).toBeNull();
  expect(
    body.querySelector('[data-entity="stmt:0"]')?.getAttribute("style"),
  ).toContain("height: 34px");
});

test("five server pages stay deduplicated and DOM-bounded", async () => {
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(280);
  const allRows = Array.from({ length: 1_000 }, (_, index) =>
    makeFrameRow({ entity: `stmt:${index}`, cells: [index] }),
  );
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url =
        typeof input === "string"
          ? new URL(input, "http://localhost")
          : new URL(input instanceof Request ? input.url : input.href);
      const cursor = url.searchParams.get("cursor");
      const offset = cursor === null ? 0 : Number(cursor.slice(2));
      // Repeat the page boundary once: the client must not let an unstable
      // continuation duplicate an entity or grow beyond `matched`.
      const start = offset === 200 ? 199 : offset;
      const pageRows = allRows.slice(start, start + 200);
      const next = offset + 200 < allRows.length ? `o:${offset + 200}` : null;
      return Promise.resolve(
        new Response(
          JSON.stringify(
            makeFrameResponse({
              view: "statements",
              columns: [makeFrameColumn({ code: "xact", type: "i64" })],
              rows: pageRows,
              page: {
                matched: 1_000,
                returned: pageRows.length,
                ...(next === null ? {} : { next }),
              },
            }),
          ),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      );
    }),
  );
  renderTable({ view: makeViewSpec({ code: "statements", columns }) });
  const body = await screen.findByTestId("ranked-matrix-body");
  await waitFor(() => expect(body.dataset.loadedRows).toBe("200"));

  for (const expected of [399, 599, 799, 999]) {
    fireEvent.click(screen.getByRole("button", { name: /table.more/ }));
    await waitFor(() => expect(body.dataset.loadedRows).toBe(String(expected)));
    expect(body.querySelectorAll("tr[data-entity]").length).toBeLessThanOrEqual(
      24,
    );
  }
  expect(screen.queryByRole("button", { name: /table.more/ })).toBeNull();
});

test("a selected row does not yank the viewport when a later page loads", async () => {
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(280);
  const allRows = Array.from({ length: 1_000 }, (_, index) =>
    makeFrameRow({ entity: `stmt:${index}`, cells: [index] }),
  );
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = new URL(
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : input.href,
        "http://localhost",
      );
      const cursor = url.searchParams.get("cursor");
      const offset = cursor === null ? 0 : Number(cursor.slice(2));
      const pageRows = allRows.slice(offset, offset + 200);
      const next = offset + 200 < allRows.length ? `o:${offset + 200}` : null;
      return Promise.resolve(
        new Response(
          JSON.stringify(
            makeFrameResponse({
              view: "statements",
              columns: [makeFrameColumn({ code: "xact", type: "i64" })],
              rows: pageRows,
              page: {
                matched: 1_000,
                returned: pageRows.length,
                ...(next === null ? {} : { next }),
              },
            }),
          ),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      );
    }),
  );
  // The selected row sits in the first page and starts in view.
  renderTable({
    view: makeViewSpec({ code: "statements", columns }),
    entity: "stmt:5",
  });
  const body = await screen.findByTestId("ranked-matrix-body");
  await waitFor(() => expect(body.dataset.loadedRows).toBe("200"));

  // The reader scrolls down, leaving the selected row above the window.
  fireEvent.scroll(body, { target: { scrollTop: 4_000 } });
  await waitFor(() =>
    expect(body.querySelector('[data-entity="stmt:5"]')).toBeNull(),
  );

  fireEvent.click(screen.getByRole("button", { name: /table.more/ }));
  await waitFor(() => expect(body.dataset.loadedRows).toBe("400"));
  expect(body.scrollTop).toBe(4_000);
});

test("keeps first-page columns and total while a continuation is pending", async () => {
  let resolveContinuation: ((response: Response) => void) | undefined;
  const continuation = new Promise<Response>((resolve) => {
    resolveContinuation = resolve;
  });
  const first = makeFrameResponse({
    view: "activity",
    columns: [
      makeFrameColumn({ code: "xact", type: "i64" }),
      makeFrameColumn({ code: "query", type: "text" }),
    ],
    rows: [makeFrameRow({ entity: "db:1", cells: [5, "select 1"] })],
    page: { matched: 2, returned: 1, next: "cursor-1" },
  });
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : input.href;
      return url.includes("cursor=cursor-1")
        ? continuation
        : Promise.resolve(
            new Response(JSON.stringify(first), {
              status: 200,
              headers: { "content-type": "application/json" },
            }),
          );
    }),
  );
  renderTable();
  const table = await screen.findByRole("table", { name: "activity" });
  await waitFor(() => expect(screen.getByText("select 1")).toBeDefined());
  expect(table.getAttribute("aria-rowcount")).toBe("3");

  fireEvent.click(screen.getByRole("button", { name: /table.more/ }));
  await waitFor(() =>
    expect(
      (screen.getByRole("button", { name: /table.more/ }) as HTMLButtonElement)
        .disabled,
    ).toBe(true),
  );
  expect(screen.getByText("select 1")).toBeDefined();
  expect(screen.getByRole("button", { name: "xact" })).toBeDefined();
  expect(table.getAttribute("aria-rowcount")).toBe("3");

  resolveContinuation?.(
    new Response(
      JSON.stringify(
        makeFrameResponse({
          view: "activity",
          columns: first.columns,
          rows: [makeFrameRow({ entity: "db:2", cells: [6, "select 2"] })],
          page: { matched: 2, returned: 1 },
        }),
      ),
      { status: 200, headers: { "content-type": "application/json" } },
    ),
  );
  await waitFor(() => expect(screen.getByText("select 2")).toBeDefined());
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
        }),
      ],
      page: { matched: 1, returned: 1 },
    }),
  );
  renderTable();
  const cell = await screen.findByText("42");
  expect(cell.style.color).toBe("var(--sev-crit-fg)");
  expect(cell.style.background).toBe("var(--sev-crit-bg)");
  expect(cell.getAttribute("title")).toBe("verdict.why");
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
