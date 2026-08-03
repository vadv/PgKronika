import { useInfiniteQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { apiGet } from "../api/client";
import type { FrameRowDto } from "../api/types";

const SEARCH_PAGE_SIZE = 20;

export interface ForensicSearchGroupArgs {
  view: string;
  at: string;
  span: number;
  q: string;
  enabled: boolean;
}

/** One independently pageable server projection in the global palette. */
export function useForensicSearchGroup(args: ForensicSearchGroupArgs) {
  const query = useInfiniteQuery({
    queryKey: ["forensic-search", args.view, args.at, args.span, args.q],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      apiGet("/v1/frame/{view}", {
        params: {
          path: { view: args.view },
          query: {
            at: Number(args.at),
            span: `${args.span}s`,
            q: args.q,
            limit: SEARCH_PAGE_SIZE,
            ...(pageParam !== null ? { cursor: pageParam } : {}),
          },
        },
      }),
    getNextPageParam: (lastPage) => lastPage.page.next ?? undefined,
    enabled: args.enabled && args.q !== "",
  });

  const rows = useMemo(() => {
    const unique = new Map<string, FrameRowDto>();
    for (const page of query.data?.pages ?? []) {
      for (const row of page.rows) {
        if (!unique.has(row.entity)) unique.set(row.entity, row);
      }
    }
    return [...unique.values()];
  }, [query.data?.pages]);

  return {
    ...query,
    rows,
    matched: query.data?.pages[0]?.page.matched ?? 0,
    snapshotTsUs: query.data?.pages[0]?.snapshot_ts_us ?? null,
    quality: query.data?.pages[0]?.quality,
  };
}
