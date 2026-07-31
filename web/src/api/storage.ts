import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export function useStorage() {
  return useQuery({
    queryKey: ["storage"],
    queryFn: () => apiGet("/v1/storage"),
  });
}
