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
import { ForensicSearch } from "./ForensicSearch";

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

const pidColumn: ColumnSpec = {
  code: "pid",
  type: "i64",
  lazy: false,
  requires: [],
  availability: "available",
};
const views = [
  makeViewSpec({ code: "activity", columns: [pidColumn] }),
  makeViewSpec({ code: "statements", columns: [] }),
];

function stubResult() {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify(
          makeFrameResponse({
            view: "activity",
            snapshot_ts_us: "1722400000000000",
            columns: [makeFrameColumn({ code: "pid", type: "i64" })],
            rows: [
              makeFrameRow({
                entity: "AQBwaWQtMTg0MjI",
                label: "pid 18422 · api@orders",
                cells: [18422],
              }),
            ],
            page: { matched: 1, returned: 1, next: null },
            quality: {
              status: "partial",
              snapshots: 1,
              gaps: [{ from_us: "1", to_us: "2" }],
              gated: ["optional_source"],
              unavailable_revision: [],
              resource_limited: ["source_limit"],
              active_tail: true,
            },
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );
}

function stubNoResults() {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify(
          makeFrameResponse({
            view: "activity",
            snapshot_ts_us: "1722400000000000",
            columns: [makeFrameColumn({ code: "pid", type: "i64" })],
            rows: [],
            page: { matched: 0, returned: 0, next: null },
            quality: {
              status: "partial",
              snapshots: 1,
              gaps: [],
              gated: [],
              unavailable_revision: [],
              resource_limited: ["source_limit"],
              active_tail: true,
            },
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );
}

function stubUnavailableSource() {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify(
          makeFrameResponse({
            view: "activity",
            rows: [],
            page: { matched: 0, returned: 0, next: null },
            quality: {
              status: "partial",
              snapshots: 0,
              gaps: [],
              gated: [],
              unavailable_revision: ["pg_stat_activity_revision"],
              resource_limited: [],
              active_tail: false,
            },
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );
}

function stubFormattedProcessResult() {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify(
          makeFrameResponse({
            view: "processes",
            snapshot_ts_us: "1722400000000000",
            columns: [
              makeFrameColumn({ code: "pid", type: "i64" }),
              makeFrameColumn({ code: "cpu", type: "f64" }),
              makeFrameColumn({
                code: "read_bytes_per_second",
                type: "f64",
                unit: "bytes_per_second",
              }),
            ],
            rows: [
              makeFrameRow({
                entity: "process:45",
                label: "pg_kronika-web / 45",
                cells: [45, 0.33992881890532123, 4096],
              }),
            ],
            page: { matched: 1, returned: 1, next: null },
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );
}

test("closed search renders no dialog", () => {
  const { container } = render(
    <ForensicSearch
      open={false}
      views={views}
      at="1722400000000000"
      span={3600}
      onClose={() => {}}
      onSelect={() => {}}
    />,
    { wrapper },
  );
  expect(container.firstChild).toBeNull();
});

test("groups a server result with reason and keyboard-opens its detail", async () => {
  stubResult();
  const onSelect = vi.fn();
  render(
    <ForensicSearch
      open
      views={views}
      at="1722400000000000"
      span={3600}
      onClose={() => {}}
      onSelect={onSelect}
    />,
    { wrapper },
  );
  const input = screen.getByRole("searchbox");
  expect(input.getAttribute("name")).toBe("forensic-search");
  expect(input.getAttribute("autocomplete")).toBe("off");
  expect(input.getAttribute("spellcheck")).toBe("false");
  fireEvent.change(input, { target: { value: "pid:18422" } });
  const result = await screen.findByRole("button", {
    name: /pid 18422 · api@orders/,
  });
  expect(screen.getByText("pid = 18422")).toBeDefined();
  expect(screen.getByText(/1.*1/)).toBeDefined();
  expect(screen.getByRole("dialog").textContent).not.toMatch(
    /partial|gaps|gated|optional_source|source_limit|active tail/i,
  );

  fireEvent.keyDown(input, { key: "ArrowDown" });
  await waitFor(() => expect(document.activeElement).toBe(result));
  fireEvent.keyDown(result, { key: "Enter" });
  expect(onSelect).toHaveBeenCalledWith("activity", "AQBwaWQtMTg0MjI");
});

test("formats compact search evidence with the response column metadata", async () => {
  stubFormattedProcessResult();
  render(
    <ForensicSearch
      open
      views={[
        makeViewSpec({
          code: "processes",
          columns: [
            pidColumn,
            {
              code: "cpu",
              type: "f64",
              lazy: false,
              requires: [],
              availability: "available",
            },
            {
              code: "read_bytes_per_second",
              type: "f64",
              unit: "bytes_per_second",
              lazy: false,
              requires: [],
              availability: "available",
            },
          ],
        }),
      ]}
      at="1722400000000000"
      span={3600}
      onClose={() => {}}
      onSelect={() => {}}
    />,
    { wrapper },
  );
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "pid:45" },
  });

  const result = await screen.findByRole("button", {
    name: /pg_kronika-web \/ 45/,
  });
  expect(result.textContent).toContain("45 · 0.34 · 4 KiB/s");
  expect(result.textContent).not.toContain("0.33992881890532123");
});

test("shows unsupported evidence keys and Escape closes", () => {
  const onClose = vi.fn();
  render(
    <ForensicSearch
      open
      views={views}
      at="1722400000000000"
      span={3600}
      onClose={onClose}
      onSelect={() => {}}
    />,
    { wrapper },
  );
  const input = screen.getByRole("searchbox");
  fireEvent.change(input, { target: { value: "device:8:0" } });
  expect(screen.getByRole("alert").textContent).toContain("device");
  fireEvent.keyDown(input, { key: "Escape" });
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("shows a simple empty state after every group settles", async () => {
  stubNoResults();
  render(
    <ForensicSearch
      open
      views={views}
      at="1722400000000000"
      span={3600}
      onClose={() => {}}
      onSelect={() => {}}
    />,
    { wrapper },
  );
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "pid:99999" },
  });
  expect(
    await screen.findByText("No matches for the selected period"),
  ).toBeDefined();
  expect(screen.getByRole("dialog").textContent).not.toMatch(
    /partial|gaps|gated|limited|active tail|proof/i,
  );
});

test("shows a calm per-source message when that source has no data", async () => {
  stubUnavailableSource();
  render(
    <ForensicSearch
      open
      views={views}
      at="1722400000000000"
      span={3600}
      onClose={() => {}}
      onSelect={() => {}}
    />,
    { wrapper },
  );
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "pid:18422" },
  });
  expect(
    await screen.findByText("No data for this source in the selected period"),
  ).toBeDefined();
  expect(screen.getByRole("dialog").textContent).not.toMatch(
    /unavailable_revision|pg_stat_activity_revision|partial|gated/i,
  );
});

test("has a visible close control, traps Tab and dismisses via the backdrop", async () => {
  const onClose = vi.fn();
  render(
    <ForensicSearch
      open
      views={views}
      at="1722400000000000"
      span={3600}
      onClose={onClose}
      onSelect={() => {}}
    />,
    { wrapper },
  );
  const input = screen.getByRole("searchbox");
  const close = screen.getByRole("button", { name: "Close forensic search" });
  await waitFor(() => expect(document.activeElement).toBe(input));
  fireEvent.keyDown(input, { key: "Tab", shiftKey: true });
  expect(document.activeElement).toBe(close);
  fireEvent.keyDown(close, { key: "Tab" });
  expect(document.activeElement).toBe(input);
  fireEvent.click(close);
  expect(onClose).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByTestId("forensic-search-backdrop"));
  expect(onClose).toHaveBeenCalledTimes(2);
});
