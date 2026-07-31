import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { useSummary } from "./summary";

afterEach(() => vi.unstubAllGlobals());

test("useSummary requests /v1/views/summary?at=", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"at_us":"1","views":[],"quality":{"status":"complete","snapshots":0,"gaps":[],"gated":[],"unavailable_revision":[],"resource_limited":[]}}', { status: 200 }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(() => useSummary("1722400000000000"), { wrapper });
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe(
    "/v1/views/summary?at=1722400000000000",
  );
});
