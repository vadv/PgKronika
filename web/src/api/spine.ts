import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";
import type { LiveQueryOptions } from "./timeline";

export interface TimelineSpineArgs {
  from: string;
  to: string;
  /** Bucket count of the aligned grid (wire parameter `buckets`). */
  buckets?: number;
}

export function useTimelineSpine(
  args: TimelineSpineArgs,
  opts?: LiveQueryOptions,
) {
  return useQuery({
    queryKey: ["timeline-spine", args.from, args.to, args.buckets ?? null],
    // `from`/`to` travel through component state as decimal strings; the
    // wire parameters are int64 µs.
    queryFn: () =>
      apiGet("/v1/timeline/spine", {
        params: {
          query: {
            from: Number(args.from),
            to: Number(args.to),
            ...(args.buckets !== undefined ? { buckets: args.buckets } : {}),
          },
        },
      }),
    placeholderData: keepPreviousData,
    refetchInterval: opts?.refetchInterval,
  });
}
