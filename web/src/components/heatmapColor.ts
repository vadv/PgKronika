const STOPS = [
  "var(--heat-0)",
  "var(--heat-1)",
  "var(--heat-2)",
  "var(--heat-3)",
  "var(--heat-4)",
] as const;

/** Stepped heat scale: null = empty cell, t∈[0,1] maps to --heat-0..4 tokens. */
export function heatColor(t: number | null): string {
  if (t === null) return "transparent";
  if (t < 0.2) return STOPS[0];
  if (t < 0.4) return STOPS[1];
  if (t < 0.6) return STOPS[2];
  if (t < 0.8) return STOPS[3];
  return STOPS[4];
}
