import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export function useUiContext(at: string) {
  return useQuery({
    queryKey: ["ui-context", at],
    // `at` travels through component state as a decimal string; the wire
    // parameter is int64 µs.
    queryFn: () =>
      apiGet("/v1/ui/context", {
        params: { query: { at: Number(at) } },
      }),
  });
}
