import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { ProjectionCatalog } from "./types";

export function useCatalog(source: string) {
  return useQuery({
    queryKey: ["catalog", source],
    queryFn: () =>
      apiFetch<ProjectionCatalog>(
        `/v1/ui/catalog?source=${encodeURIComponent(source)}`,
      ),
    staleTime: Infinity,
  });
}
