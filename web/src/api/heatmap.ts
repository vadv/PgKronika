import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { HeatmapResponse } from "./types";

export interface HeatmapArgs {
  view: string;
  metric: string;
  from: string;
  to: string;
  buckets?: number;
  top?: number;
}

export function useHeatmap(args: HeatmapArgs) {
  const params = new URLSearchParams({
    view: args.view,
    metric: args.metric,
    from: args.from,
    to: args.to,
  });
  if (args.buckets !== undefined) params.set("buckets", String(args.buckets));
  if (args.top !== undefined) params.set("top", String(args.top));
  const qs = params.toString();
  return useQuery({
    queryKey: [
      "heatmap",
      args.view,
      args.metric,
      args.from,
      args.to,
      args.buckets ?? null,
      args.top ?? null,
    ],
    queryFn: () => apiFetch<HeatmapResponse>(`/v1/timeline/heatmap?${qs}`),
  });
}
