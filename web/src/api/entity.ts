import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export interface EntityPointArgs {
  view: string;
  /** Typed entity token from a frame/search row. */
  entity: string;
  /** Cursor timestamp (int64 µs, decimal string). */
  at: string;
  /** Ask the server for provenance-backed related entities. */
  includeRelated?: boolean;
}

export interface EntityHistoryArgs {
  view: string;
  entity: string;
  /** Bounded half-open history window, microseconds as decimal strings. */
  from: string;
  to: string;
  columns: string[];
  limit?: number;
  /** Opaque history continuation token (transient). */
  cursor?: string | null;
  enabled?: boolean;
}

export async function fetchEntityPoint(args: EntityPointArgs) {
  const response = await apiGet("/v1/entity/{view}/{entity}", {
    params: {
      path: { view: args.view, entity: args.entity },
      query: {
        at: Number(args.at),
        ...(args.includeRelated ? { include: "related" } : {}),
      },
    },
  });
  if (!("fields" in response)) {
    throw new Error("entity point endpoint returned history mode");
  }
  return response;
}

export function useEntityPoint(args: EntityPointArgs) {
  return useQuery({
    queryKey: [
      "entity-point",
      args.view,
      args.entity,
      args.at,
      args.includeRelated ?? false,
    ],
    queryFn: () => fetchEntityPoint(args),
    enabled: args.entity !== "",
  });
}

export function useEntityHistory(args: EntityHistoryArgs) {
  return useQuery({
    queryKey: [
      "entity-history",
      args.view,
      args.entity,
      args.from,
      args.to,
      args.columns.join(","),
      args.limit ?? null,
      args.cursor ?? null,
    ],
    queryFn: async () => {
      const response = await apiGet("/v1/entity/{view}/{entity}", {
        params: {
          path: { view: args.view, entity: args.entity },
          query: {
            from: Number(args.from),
            to: Number(args.to),
            columns: args.columns.join(","),
            ...(args.limit !== undefined ? { limit: args.limit } : {}),
            ...(args.cursor != null ? { cursor: args.cursor } : {}),
          },
        },
      });
      if (!("snapshots" in response)) {
        throw new Error("entity history endpoint returned point mode");
      }
      return response;
    },
    enabled:
      args.enabled !== false &&
      args.entity !== "" &&
      args.columns.length > 0 &&
      args.from !== "" &&
      args.to !== "",
  });
}

/** Compatibility name for point-only callers while the universal detail is
 * rolled through every screen. Its type no longer permits a history shape. */
export const useEntity = useEntityPoint;
