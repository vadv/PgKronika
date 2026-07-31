import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export function useCatalog() {
  return useQuery({
    queryKey: ["catalog"],
    queryFn: () => apiGet("/v1/ui/catalog"),
    staleTime: Infinity,
  });
}
