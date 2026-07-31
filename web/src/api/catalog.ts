import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export function useCatalog() {
  return useQuery({
    queryKey: ["catalog"],
    queryFn: () => apiGet("/v1/ui/catalog"),
    // Availability changes (gated → available) must become visible within
    // the same SPA session; the server's ETag makes revalidation cheap.
    staleTime: 30_000,
    refetchOnWindowFocus: true,
  });
}
