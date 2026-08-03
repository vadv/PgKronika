import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeFrameResponse, makeFrameRow } from "../testkit/apiFixtures";
import { useForensicSearchGroup } from "./group";

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

test("search group follows the server cursor and deduplicates entities", async () => {
  const fetch = vi
    .fn()
    .mockResolvedValueOnce(
      new Response(
        JSON.stringify(
          makeFrameResponse({
            view: "activity",
            rows: [makeFrameRow({ entity: "pid:1", label: "pid 1" })],
            page: { matched: 3, returned: 1, next: "cursor-2" },
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    )
    .mockResolvedValueOnce(
      new Response(
        JSON.stringify(
          makeFrameResponse({
            view: "activity",
            rows: [
              makeFrameRow({ entity: "pid:1", label: "pid 1" }),
              makeFrameRow({ entity: "pid:2", label: "pid 2" }),
            ],
            page: { matched: 3, returned: 2, next: null },
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    );
  vi.stubGlobal("fetch", fetch);

  const { result } = renderHook(
    () =>
      useForensicSearchGroup({
        view: "activity",
        at: "1722400000000000",
        span: 3600,
        q: "pid=1",
        enabled: true,
      }),
    { wrapper },
  );

  await waitFor(() => expect(result.current.rows).toHaveLength(1));
  expect(result.current.matched).toBe(3);
  expect(result.current.hasNextPage).toBe(true);
  const first = new URL((fetch.mock.calls[0]?.[0] as Request).url);
  expect(first.pathname + first.search).toBe(
    "/v1/frame/activity?at=1722400000000000&span=3600s&q=pid%3D1&limit=20",
  );

  await act(async () => {
    await result.current.fetchNextPage();
  });
  await waitFor(() => expect(result.current.rows).toHaveLength(2));
  const second = new URL((fetch.mock.calls[1]?.[0] as Request).url);
  expect(second.searchParams.get("cursor")).toBe("cursor-2");
  expect(result.current.hasNextPage).toBe(false);
});

test("disabled or empty search never talks to the server", async () => {
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  renderHook(
    () =>
      useForensicSearchGroup({
        view: "activity",
        at: "1722400000000000",
        span: 3600,
        q: "",
        enabled: false,
      }),
    { wrapper },
  );
  await Promise.resolve();
  expect(fetch).not.toHaveBeenCalled();
});
