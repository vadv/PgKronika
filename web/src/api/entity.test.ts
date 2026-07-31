import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeEntityPointResponse } from "../testkit/apiFixtures";
import { useEntity } from "./entity";

afterEach(() => vi.unstubAllGlobals());

function stubEntity(body: unknown) {
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

test("useEntity builds query with path params and returns parsed data", async () => {
  const body = makeEntityPointResponse();
  stubEntity(body);
  const { result } = renderHook(
    () =>
      useEntity({ view: "activity", entity: "db-1", at: "1722400000000000" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/entity/activity/db-1?at=1722400000000000",
  );
  expect(result.current.data).toEqual(body);
});

test("useEntity omits unset at (history mode)", async () => {
  stubEntity(makeEntityPointResponse());
  const { result } = renderHook(
    () => useEntity({ view: "activity", entity: "db-1" }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe("/v1/entity/activity/db-1");
});
