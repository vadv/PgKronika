import { useQuery } from "@tanstack/react-query";
import { ApiError, apiGet } from "./client";

export interface FrameArgs {
  view: string;
  /** Cursor timestamp (int64 µs, decimal string). */
  at: string;
  /** Window length in seconds; the wire parameter is a duration string. */
  span?: number;
  preset?: string | null;
  database?: string | null;
  q?: string | null;
  sort?: string | null;
  order?: "asc" | "desc" | null;
  limit?: number;
  cursor?: string | null;
}

export function useFrame(args: FrameArgs) {
  return useQuery({
    queryKey: [
      "frame",
      args.view,
      args.at,
      args.span ?? null,
      args.preset ?? null,
      args.database ?? null,
      args.q ?? null,
      args.sort ?? null,
      args.order ?? null,
      args.limit ?? null,
      args.cursor ?? null,
    ],
    // `at` travels through URL state as a decimal string; the wire
    // parameter is int64 µs. `span` is seconds in state, a duration
    // string (`<n>s`) on the wire.
    queryFn: () =>
      apiGet("/v1/frame/{view}", {
        params: {
          path: { view: args.view },
          query: {
            at: Number(args.at),
            ...(args.span !== undefined ? { span: `${args.span}s` } : {}),
            ...(args.preset != null ? { preset: args.preset } : {}),
            ...(args.database != null ? { database: args.database } : {}),
            ...(args.q != null ? { q: args.q } : {}),
            ...(args.sort != null ? { sort: args.sort } : {}),
            ...(args.order != null ? { order: args.order } : {}),
            ...(args.limit !== undefined ? { limit: args.limit } : {}),
            ...(args.cursor != null ? { cursor: args.cursor } : {}),
          },
        },
      }),
    // 4xx is a typed answer (expired cursor, bad constraint), not a transient
    // failure — retrying only delays the honest recovery path.
    retry: (count, error) =>
      !(
        error instanceof ApiError &&
        error.status >= 400 &&
        error.status < 500
      ) && count < 3,
  });
}
