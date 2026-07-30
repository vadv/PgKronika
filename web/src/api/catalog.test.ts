import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { useCatalog } from "./catalog";

afterEach(() => vi.unstubAllGlobals());

test("useCatalog fetches catalog for source", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"revision":1,"views":[]}', { status: 200 }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(() => useCatalog("local"), { wrapper });
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(result.current.data?.revision).toBe(1);
  expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe(
    "/v1/ui/catalog?source=local",
  );
});
