import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { SummaryResponse } from "./types";

export function useSummary(at: string) {
  return useQuery({
    queryKey: ["summary", at],
    queryFn: () =>
      apiFetch<SummaryResponse>(
        `/v1/views/summary?at=${encodeURIComponent(at)}`,
      ),
  });
}
