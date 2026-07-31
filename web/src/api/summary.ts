import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export function useSummary(at: string) {
  return useQuery({
    queryKey: ["summary", at],
    // `at` travels through URL state as a decimal string; the wire
    // parameter is int64 µs.
    queryFn: () =>
      apiGet("/v1/views/summary", { params: { query: { at: Number(at) } } }),
  });
}
