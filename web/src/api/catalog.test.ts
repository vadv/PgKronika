import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { useCatalog } from "./catalog";
import type { ProjectionCatalog } from "./types";

afterEach(() => vi.unstubAllGlobals());

test("useCatalog fetches catalog without query parameters", async () => {
  const body: ProjectionCatalog = { revision: 1, views: [] };
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response(JSON.stringify(body), {
      status: 200,
      headers: { "content-type": "application/json" },
    }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(() => useCatalog(), { wrapper });
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(result.current.data?.revision).toBe(1);
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  expect(new URL(req.url).pathname).toBe("/v1/ui/catalog");
  expect(new URL(req.url).search).toBe("");
});
