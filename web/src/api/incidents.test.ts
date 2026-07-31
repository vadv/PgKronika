import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeIncidentsResponse } from "../testkit/apiFixtures";
import { useIncidents } from "./incidents";

afterEach(() => vi.unstubAllGlobals());

test("useIncidents builds query and returns parsed data", async () => {
  const body = makeIncidentsResponse({ from: 0, to: 86_400_000_000 });
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(
    () => useIncidents({ from: "0", to: "86400000000" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe("/v1/incidents?from=0&to=86400000000");
  expect(result.current.data).toEqual(body);
});
