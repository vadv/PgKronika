import { useQuery } from "@tanstack/react-query";
import { apiGet } from "./client";

export interface DataQualityArgs {
  from: string;
  to: string;
}

export function useDataQuality(args: DataQualityArgs) {
  return useQuery({
    queryKey: ["data-quality", args.from, args.to],
    // `from`/`to` travel through component state as decimal strings; the
    // wire parameters are int64 µs.
    queryFn: () =>
      apiGet("/v1/data/quality", {
        params: {
          query: { from: Number(args.from), to: Number(args.to) },
        },
      }),
  });
}
