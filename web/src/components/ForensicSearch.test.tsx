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
  fireEvent.change(input, { target: { value: "pid:18422" } });
  const result = await screen.findByRole("button", {
    name: /pid 18422 · api@orders/,
  });
  expect(screen.getByText("pid = 18422")).toBeDefined();
  expect(screen.getByText(/1.*1/)).toBeDefined();

  fireEvent.keyDown(input, { key: "ArrowDown" });
  await waitFor(() => expect(document.activeElement).toBe(result));
  fireEvent.keyDown(result, { key: "Enter" });
  expect(onSelect).toHaveBeenCalledWith("activity", "AQBwaWQtMTg0MjI");
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

test("shows an honest aggregate empty state after every group settles", async () => {
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
    await screen.findByText("No server-side matches in this time window"),
  ).toBeDefined();
});
