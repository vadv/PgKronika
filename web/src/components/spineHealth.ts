/** Pure logic behind the redesigned spine strip: verdict ribbon bucketing,
 * the window health score and the event glyph mapping. Kept component-free
 * so the formula and the mappings are unit-testable without a DOM.
 *
 * Data discipline: the only verdicts here come from `/v1/timeline/health`
 * (server-computed `overall_state` per point). Client-side numbers are pure
 * aggregation of those verdicts — no thresholds on the data itself. The
 * 90/70 cutoffs below color the score chip only; they never touch data. */
import type {
  EventFact,
  HealthPointResponse,
  IncidentResponse,
} from "../api/types";

/** Verdict of one ribbon bucket. `gap` = no server verdict for the slice. */
export type BucketVerdict = "ok" | "warn" | "crit" | "gap";

/** Pin the live window end to the absolute bucket grid: boundaries sit on
 * multiples of `bucketSpanUs`, so two renders a second apart produce the
 * identical grid and only the filling of the last (forming) bucket moves. */
export function anchorWindowEnd(nowUs: number, bucketSpanUs: number): number {
  return Math.ceil(nowUs / bucketSpanUs) * bucketSpanUs;
}

/** Score input: the forming tail bucket of a live window is excluded, so the
 * score only moves on a completed-bucket boundary or an incident change. */
export function scoreVerdicts(
  verdicts: BucketVerdict[],
  hasFormingTail: boolean,
): BucketVerdict[] {
  return hasFormingTail ? verdicts.slice(0, -1) : verdicts;
}

/** Wire `overall_state` → ribbon verdict; anything unknown is an honest gap. */
export function mapHealthState(state: string): BucketVerdict {
  switch (state) {
    case "normal":
      return "ok";
    case "degraded":
      return "warn";
    case "critical":
      return "crit";
    default:
      return "gap";
  }
}

/** Spread health points over the ribbon grid by interval midpoint; buckets
 * no point covers stay `gap` (the strip must show the hole, not hide it). */
export function bucketVerdicts(
  points: HealthPointResponse[],
  fromUs: number,
  toUs: number,
  buckets: number,
): BucketVerdict[] {
  const out: BucketVerdict[] = Array.from({ length: buckets }, () => "gap");
  const windowUs = toUs - fromUs;
  if (windowUs <= 0) return out;
  for (const point of points) {
    const mid = (point.interval.from_us + point.interval.to_us) / 2;
    const index = Math.floor(((mid - fromUs) / windowUs) * buckets);
    if (index < 0 || index >= buckets) continue;
    const verdict = mapHealthState(point.overall_state);
    // A point never downgrades a bucket another point already marked worse.
    const rank = { ok: 0, warn: 1, crit: 2, gap: -1 } as const;
    if (rank[verdict] > rank[out[index] ?? "gap"]) out[index] = verdict;
  }
  return out;
}

export interface WindowScore {
  /** 0–100, floored at 0. */
  score: number;
  /** Minutes of critical verdict inside the window. */
  critMin: number;
  /** Minutes of degraded verdict inside the window. */
  warnMin: number;
  /** Incidents overlapping the window. */
  incidents: number;
}

/** Owner-approved draft formula, aggregated from server verdicts only:
 * 100 − crit·min×3 − warn·min×0.5 − incidents×5, floored at 0. */
export function windowScore(
  verdicts: BucketVerdict[],
  windowSeconds: number,
  incidents: number,
): WindowScore {
  const bucketMin = windowSeconds / 60 / Math.max(1, verdicts.length);
  const critMin = verdicts.filter((v) => v === "crit").length * bucketMin;
  const warnMin = verdicts.filter((v) => v === "warn").length * bucketMin;
  const score = Math.max(
    0,
    Math.round(100 - critMin * 3 - warnMin * 0.5 - incidents * 5),
  );
  return { score, critMin, warnMin, incidents };
}

/** Incidents overlapping [fromUs, toUs] (wire interval is int64 µs numbers). */
export function countWindowIncidents(
  incidents: IncidentResponse[],
  fromUs: number,
  toUs: number,
): number {
  return incidents.filter(
    (inc) => inc.interval.from <= toUs && inc.interval.to >= fromUs,
  ).length;
}

export type ChipTone = "ok" | "warn" | "crit";

/** Chip coloring only — these cutoffs never classify data. */
export function chipTone(score: number): ChipTone {
  if (score >= 90) return "ok";
  if (score >= 70) return "warn";
  return "crit";
}

export type GlyphTone = "crit" | "warn" | "info" | "dim";

export interface EventGlyph {
  glyph: string;
  tone: GlyphTone;
}

/** Presentation-only mapping from the event kind code to a glyph:
 * ◆ errors, ▲ pressure signals, ● info/checkpoints, ○ background
 * maintenance. The API carries no per-event severity, so glyph tones reuse
 * the severity palette but never claim a verdict. */
export function eventGlyph(event: EventFact): EventGlyph {
  const kind = event.event_kind.toLowerCase();
  if (
    kind.includes("error") ||
    kind.includes("deadlock") ||
    kind.includes("crash") ||
    kind.includes("panic") ||
    kind.includes("oom") ||
    kind.includes("checksum") ||
    kind.includes("fatal") ||
    kind.includes("sigkill")
  ) {
    return { glyph: "◆", tone: "crit" };
  }
  if (kind.startsWith("pg.maintenance.") || kind.startsWith("collector.")) {
    return { glyph: "○", tone: "dim" };
  }
  if (kind.includes("checkpoint")) {
    return { glyph: "●", tone: "info" };
  }
  if (
    kind.includes("lock") ||
    kind.includes("slow") ||
    kind.includes("temp_file") ||
    kind.includes("memory") ||
    kind.includes("capacity") ||
    kind.includes("sessions_") ||
    kind.includes("recovery_conflict") ||
    kind.includes("replication") ||
    kind.includes("signal_termination") ||
    kind.includes("shutdown_requested")
  ) {
    return { glyph: "▲", tone: "warn" };
  }
  return { glyph: "●", tone: "info" };
}

/** Short reason for a non-ok bucket: the floor class when present, else the
 * domain with the highest penalty. Codes are localized by the caller. */
export function bucketReason(point: HealthPointResponse | null): {
  floor: string | null;
  domain: string | null;
} {
  if (point === null) return { floor: null, domain: null };
  const floor = point.floor_evidence[0]?.class ?? null;
  let domain: string | null = null;
  let worst = 0;
  for (const d of point.domains) {
    if (d.penalty > worst) {
      worst = d.penalty;
      domain = d.domain;
    }
  }
  return { floor, domain };
}
