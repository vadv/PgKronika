import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export interface EntityArgs {
  view: string;
  /** Typed entity token from a frame row. */
  entity: string;
  /** Cursor timestamp (int64 µs, decimal string); omit for history mode. */
  at?: string | null;
}

export function useEntity(args: EntityArgs) {
  return useQuery({
    queryKey: ["entity", args.view, args.entity, args.at ?? null],
    // `at` travels through URL state as a decimal string; the wire
    // parameter is int64 µs.
    queryFn: () =>
      apiGet("/v1/entity/{view}/{entity}", {
        params: {
          path: { view: args.view, entity: args.entity },
          query: {
            ...(args.at != null ? { at: Number(args.at) } : {}),
          },
        },
      }),
  });
}
