import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeEntityHistoryResponse,
  makeEntityPointResponse,
} from "../testkit/apiFixtures";
import { useEntityHistory, useEntityPoint } from "./entity";

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

test("useEntityPoint builds a point-only query with related evidence", async () => {
  const body = makeEntityPointResponse();
  stubEntity(body);
  const { result } = renderHook(
    () =>
      useEntityPoint({
        view: "activity",
        entity: "db-1",
        at: "1722400000000000",
        includeRelated: true,
      }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/entity/activity/db-1?at=1722400000000000&include=related",
  );
  expect(result.current.data).toEqual(body);
});

test("useEntityHistory sends the bounded range, columns and continuation", async () => {
  const body = makeEntityHistoryResponse();
  stubEntity(body);
  const { result } = renderHook(
    () =>
      useEntityHistory({
        view: "activity",
        entity: "db-1",
        from: "1722396400000000",
        to: "1722400000000000",
        columns: ["cpu", "rss"],
        limit: 200,
        cursor: "next-page",
      }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/entity/activity/db-1?from=1722396400000000&to=1722400000000000&columns=cpu%2Crss&limit=200&cursor=next-page",
  );
  expect(result.current.data).toEqual(body);
});
