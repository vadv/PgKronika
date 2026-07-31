import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeSpineResponse } from "../testkit/apiFixtures";
import { useTimelineSpine } from "./spine";

afterEach(() => vi.unstubAllGlobals());

function stubSpine(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
}

function wrapper({ children }: { children: ReactNode }) {
  return createElement(
    QueryClientProvider,
    { client: new QueryClient() },
    children,
  );
}

test("useTimelineSpine builds query and returns parsed data", async () => {
  const body = makeSpineResponse();
  stubSpine(body);
  const { result } = renderHook(
    () =>
      useTimelineSpine({
        from: "1722400000000000",
        to: "1722403600000000",
        buckets: 60,
      }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/timeline/spine?from=1722400000000000&to=1722403600000000&buckets=60",
  );
  expect(result.current.data).toEqual(body);
});

test("useTimelineSpine omits unset buckets", async () => {
  stubSpine(makeSpineResponse());
  const { result } = renderHook(
    () =>
      useTimelineSpine({ from: "1722400000000000", to: "1722403600000000" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/timeline/spine?from=1722400000000000&to=1722403600000000",
  );
});
