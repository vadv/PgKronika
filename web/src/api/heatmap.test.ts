import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeHeatmapQuality } from "../testkit/apiFixtures";
import { useHeatmap } from "./heatmap";
import type { HeatmapResponse } from "./types";

afterEach(() => vi.unstubAllGlobals());

test("useHeatmap builds query with all params", async () => {
  const body: HeatmapResponse = {
    grid: { from_us: "0", to_us: "1", bucket_count: 56 },
    ranking: { exact: false, unseen_upper: 0 },
    rows: [],
    quality: makeHeatmapQuality({ status: "partial" }),
  };
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response(JSON.stringify(body), {
      status: 200,
      headers: { "content-type": "application/json" },
    }),
  ));
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(
    () => useHeatmap({ view: "statements", metric: "time", from: "0", to: "86400000000", buckets: 56, top: 8 }),
    { wrapper },
  );
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  const url = new URL(req.url);
  expect(url.pathname + url.search).toBe(
    "/v1/timeline/heatmap?view=statements&metric=time&from=0&to=86400000000&buckets=56&top=8",
  );
});
