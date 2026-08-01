import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";
import type { LiveQueryOptions } from "./timeline";

export interface IncidentsArgs {
  from: string;
  to: string;
}

export function useIncidents(args: IncidentsArgs, opts?: LiveQueryOptions) {
  return useQuery({
    queryKey: ["incidents", args.from, args.to],
    // `from`/`to` travel through component state as decimal strings; the
    // wire parameters are int64 µs.
    queryFn: () =>
      apiGet("/v1/incidents", {
        params: {
          query: { from: Number(args.from), to: Number(args.to) },
        },
      }),
    placeholderData: keepPreviousData,
    refetchInterval: opts?.refetchInterval,
  });
}
