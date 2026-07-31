import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeFrameResponse } from "../testkit/apiFixtures";
import { useFrame } from "./frame";

afterEach(() => vi.unstubAllGlobals());

function stubFrame(body: unknown) {
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

test("useFrame builds query with all params", async () => {
  stubFrame(makeFrameResponse({ view: "statements" }));
  const { result } = renderHook(
    () =>
      useFrame({
        view: "statements",
        at: "1722400000000000",
        span: 3600,
        preset: "cpu",
        database: "app",
        q: "active",
        sort: "time",
        order: "desc",
        limit: 200,
        cursor: "abc",
      }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/frame/statements?at=1722400000000000&span=3600s&preset=cpu&database=app&q=active&sort=time&order=desc&limit=200&cursor=abc",
  );
});

test("useFrame omits unset params and returns parsed data", async () => {
  const body = makeFrameResponse({ view: "activity" });
  stubFrame(body);
  const { result } = renderHook(
    () => useFrame({ view: "activity", at: "1722400000000000" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/frame/activity?at=1722400000000000",
  );
  expect(result.current.data).toEqual(body);
});
