import { keepPreviousData, useQuery } from "@tanstack/react-query";
import type { EventsResponseDto } from "./types";
import { apiGet } from "./client";

/** Live polling for the spine strip: keys stay anchored to the bucket grid,
 * freshness comes from the interval — and the previous answer stays visible
 * across a key change (no "warming" flash on a bucket boundary). */
export interface LiveQueryOptions {
  refetchInterval?: number;
}

const liveOptions = (opts?: LiveQueryOptions) => ({
  placeholderData: keepPreviousData,
  refetchInterval: opts?.refetchInterval,
});

export interface TimelineHealthArgs {
  from: string;
  to: string;
  /** Bucket step in µs. */
  step?: number;
}

export function useTimelineHealth(
  args: TimelineHealthArgs,
  opts?: LiveQueryOptions,
) {
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
    ...liveOptions(opts),
  });
}

export interface TimelineEventsArgs {
  from: string;
  to: string;
  limit?: number;
  /** Follow at most this many cursor pages; hard-capped for browser memory. */
  maxPages?: number;
}

export type BoundedEventsResponse = EventsResponseDto & {
  fetched_pages: number;
  truncated: boolean;
};

const MAX_EVENT_REQUEST_PAGES = 4;

export function useTimelineEvents(
  args: TimelineEventsArgs,
  opts?: LiveQueryOptions,
) {
  const maxPages = Math.min(
    MAX_EVENT_REQUEST_PAGES,
    Math.max(1, args.maxPages ?? 1),
  );
  return useQuery({
    queryKey: [
      "timeline-events",
      args.from,
      args.to,
      args.limit ?? null,
      maxPages,
    ],
    queryFn: async (): Promise<BoundedEventsResponse> => {
      let cursor: string | undefined;
      let first: EventsResponseDto | undefined;
      const events: EventsResponseDto["events"] = [];
      let fetchedPages = 0;
      do {
        const page = await apiGet("/v1/timeline/events", {
          params: {
            query: {
              from: Number(args.from),
              to: Number(args.to),
              ...(args.limit !== undefined ? { limit: args.limit } : {}),
              ...(cursor !== undefined ? { cursor } : {}),
            },
          },
        });
        first ??= page;
        events.push(...page.events);
        fetchedPages += 1;
        cursor = page.next_cursor ?? undefined;
      } while (cursor !== undefined && fetchedPages < maxPages);
      if (first === undefined)
        throw new Error("timeline events returned no page");
      return {
        ...first,
        events,
        next_cursor: cursor ?? null,
        fetched_pages: fetchedPages,
        truncated: cursor !== undefined,
      };
    },
    ...liveOptions(opts),
  });
}
