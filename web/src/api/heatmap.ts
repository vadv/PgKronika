import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export interface HeatmapArgs {
  view: string;
  metric: string;
  from: string;
  to: string;
  buckets?: number;
  top?: number;
  enabled?: boolean;
}

export function useHeatmap(args: HeatmapArgs) {
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
    // `from`/`to` travel through component state as decimal strings; the
    // wire parameters are int64 µs.
    queryFn: () =>
      apiGet("/v1/timeline/heatmap", {
        params: {
          query: {
            view: args.view,
            metric: args.metric,
            from: Number(args.from),
            to: Number(args.to),
            ...(args.buckets !== undefined ? { buckets: args.buckets } : {}),
            ...(args.top !== undefined ? { top: args.top } : {}),
          },
        },
      }),
    enabled: args.enabled ?? true,
  });
}
