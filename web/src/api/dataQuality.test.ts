import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeDataQualityResponse } from "../testkit/apiFixtures";
import { useDataQuality } from "./dataQuality";

afterEach(() => vi.unstubAllGlobals());

function stubDataQuality(body: unknown) {
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

test("useDataQuality builds query and returns parsed data", async () => {
  const body = makeDataQualityResponse();
  stubDataQuality(body);
  const { result } = renderHook(
    () => useDataQuality({ from: "1722400000000000", to: "1722403600000000" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/data/quality?from=1722400000000000&to=1722403600000000",
  );
  expect(result.current.data).toEqual(body);
});
