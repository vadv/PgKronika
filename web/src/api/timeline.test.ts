import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeEventFact,
  makeEventsResponse,
  makeHealthResponse,
} from "../testkit/apiFixtures";
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
  expect(result.current.data).toEqual({
    ...body,
    fetched_pages: 1,
    truncated: false,
  });
});

test("useTimelineEvents follows a bounded event cursor when maxPages is requested", async () => {
  const first = makeEventsResponse({
    events: [makeEventFact({ event_instance_id: "event-1" })],
    next_cursor: "cursor-2",
  });
  const second = makeEventsResponse({
    events: [makeEventFact({ event_instance_id: "event-2" })],
    next_cursor: null,
  });
  vi.stubGlobal(
    "fetch",
    vi
      .fn()
      .mockResolvedValueOnce(
        new Response(JSON.stringify(first), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify(second), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
  );
  const { result } = renderHook(
    () =>
      useTimelineEvents({
        from: "0",
        to: "86400000000",
        limit: 50,
        maxPages: 4,
      }),
    { wrapper },
  );

  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(
    result.current.data?.events.map((event) => event.event_instance_id),
  ).toEqual(["event-1", "event-2"]);
  expect(result.current.data?.next_cursor).toBeNull();
  const calls = vi
    .mocked(fetch)
    .mock.calls.map(([input]) => new URL((input as Request).url));
  expect(calls).toHaveLength(2);
  expect(calls[1]?.searchParams.get("cursor")).toBe("cursor-2");
});
