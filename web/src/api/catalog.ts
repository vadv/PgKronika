import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { ProjectionCatalog } from "./types";

export function useCatalog() {
  return useQuery({
    queryKey: ["catalog"],
    queryFn: () => apiFetch<ProjectionCatalog>("/v1/ui/catalog"),
    staleTime: Infinity,
  });
}
