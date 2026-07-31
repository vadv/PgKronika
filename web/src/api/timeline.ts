import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export interface TimelineHealthArgs {
  from: string;
  to: string;
  /** Bucket step in µs. */
  step?: number;
}

export function useTimelineHealth(args: TimelineHealthArgs) {
  return useQuery({
    queryKey: ["timeline-health", args.from, args.to, args.step ?? null],
    // `from`/`to` travel through component state as decimal strings; the
    // wire parameters are int64 µs.
    queryFn: () =>
      apiGet("/v1/timeline/health", {
        params: {
          query: {
            from: Number(args.from),
            to: Number(args.to),
            ...(args.step !== undefined ? { step: args.step } : {}),
          },
        },
      }),
  });
}

export interface TimelineEventsArgs {
  from: string;
  to: string;
  limit?: number;
}

export function useTimelineEvents(args: TimelineEventsArgs) {
  return useQuery({
    queryKey: ["timeline-events", args.from, args.to, args.limit ?? null],
    queryFn: () =>
      apiGet("/v1/timeline/events", {
        params: {
          query: {
            from: Number(args.from),
            to: Number(args.to),
            ...(args.limit !== undefined ? { limit: args.limit } : {}),
          },
        },
      }),
  });
}
