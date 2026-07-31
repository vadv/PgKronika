import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { useHeatmap } from "./heatmap";

afterEach(() => vi.unstubAllGlobals());

test("useHeatmap builds query with all params", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"grid":{"from_us":"0","to_us":"1","bucket_count":56},"ranking":{"exact":false,"unseen_upper":0},"rows":[],"quality":{"status":"partial","snapshots":0,"gaps":[],"gated":[],"unavailable_revision":[],"resource_limited":[]}}', { status: 200 }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(
    () => useHeatmap({ view: "statements", metric: "time", from: "0", to: "86400000000", buckets: 56, top: 8 }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe(
    "/v1/timeline/heatmap?view=statements&metric=time&from=0&to=86400000000&buckets=56&top=8",
  );
});
