import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeEventsResponse, makeHealthResponse } from "../testkit/apiFixtures";
import { useTimelineEvents, useTimelineHealth } from "./timeline";

afterEach(() => vi.unstubAllGlobals());

function stubJson(body: unknown) {
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

test("useTimelineHealth builds query with step", async () => {
  const body = makeHealthResponse();
  stubJson(body);
  const { result } = renderHook(
    () => useTimelineHealth({ from: "0", to: "86400000000", step: 60_000_000 }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/timeline/health?from=0&to=86400000000&step=60000000",
  );
  expect(result.current.data).toEqual(body);
});

test("useTimelineHealth omits step when unset", async () => {
  stubJson(makeHealthResponse());
  const { result } = renderHook(
    () => useTimelineHealth({ from: "0", to: "86400000000" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/timeline/health?from=0&to=86400000000",
  );
});

test("useTimelineEvents builds query with limit", async () => {
  const body = makeEventsResponse();
  stubJson(body);
  const { result } = renderHook(
    () => useTimelineEvents({ from: "0", to: "86400000000", limit: 100 }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/timeline/events?from=0&to=86400000000&limit=100",
  );
  expect(result.current.data).toEqual(body);
});
