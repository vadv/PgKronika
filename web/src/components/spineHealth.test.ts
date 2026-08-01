import { expect, test } from "vitest";
import { makeEventFact, makeHealthPoint } from "../testkit/apiFixtures";
import {
  anchorWindowEnd,
  bucketReason,
  bucketVerdicts,
  chipTone,
  countWindowIncidents,
  eventGlyph,
  mapHealthState,
  scoreVerdicts,
  windowScore,
} from "./spineHealth";

const FROM = 1_000_000_000_000;
const TO = FROM + 3_600_000_000; // 1 h window

test("mapHealthState maps wire states, unknown becomes gap", () => {
  expect(mapHealthState("normal")).toBe("ok");
  expect(mapHealthState("degraded")).toBe("warn");
  expect(mapHealthState("critical")).toBe("crit");
  expect(mapHealthState("unknown")).toBe("gap");
  expect(mapHealthState("")).toBe("gap");
});

test("bucketVerdicts spreads points by midpoint and keeps holes as gap", () => {
  const points = [
    makeHealthPoint({
      interval: { from_us: FROM, to_us: FROM + 900_000_000 },
      overall_state: "normal",
    }),
    makeHealthPoint({
      interval: { from_us: FROM + 1_800_000_000, to_us: FROM + 2_700_000_000 },
      overall_state: "critical",
    }),
  ];
  const verdicts = bucketVerdicts(points, FROM, TO, 4);
  expect(verdicts).toEqual(["ok", "gap", "crit", "gap"]);
});

test("bucketVerdicts keeps the worst verdict when points share a bucket", () => {
  const points = [
    makeHealthPoint({
      interval: { from_us: FROM, to_us: FROM + 450_000_000 },
      overall_state: "normal",
    }),
    makeHealthPoint({
      interval: { from_us: FROM + 450_000_000, to_us: FROM + 900_000_000 },
      overall_state: "degraded",
    }),
  ];
  // Both midpoints land in the first of 2 buckets; degraded must win.
  expect(bucketVerdicts(points, FROM, TO, 2)[0]).toBe("warn");
});

test("windowScore applies the owner-approved formula with a floor at 0", () => {
  // 60 one-minute buckets: 4 crit, 5 warn, rest ok; 1 incident in window.
  const verdicts = Array.from({ length: 60 }, (_, i) =>
    i < 4 ? "crit" : i < 9 ? "warn" : ("ok" as const),
  );
  const result = windowScore(verdicts, 3600, 1);
  // 100 − 4×3 − 5×0.5 − 1×5 = 80.5 → 81 (rounded).
  expect(result.score).toBe(81);
  expect(result.critMin).toBe(4);
  expect(result.warnMin).toBe(5);
  expect(result.incidents).toBe(1);
});

test("windowScore floors at 0 and counts sub-minute buckets honestly", () => {
  const verdicts = Array.from({ length: 96 }, () => "crit" as const);
  // 15-minute window: 96 buckets = 0.15625 min each.
  const result = windowScore(verdicts, 900, 0);
  // 100 − 15×3 = 55.
  expect(result.score).toBe(55);
  expect(result.critMin).toBeCloseTo(15);
  // 24 h of solid critical sinks the score to the floor.
  expect(windowScore(verdicts, 86_400, 0).score).toBe(0);
});

test("countWindowIncidents counts overlap, ignores outside intervals", () => {
  const inc = (from: number, to: number) =>
    ({ interval: { from, to } }) as never;
  const incidents = [
    inc(FROM - 10, FROM + 10), // crosses the left edge
    inc(TO - 10, TO + 10), // crosses the right edge
    inc(TO + 10, TO + 20), // fully outside
  ];
  expect(countWindowIncidents(incidents, FROM, TO)).toBe(2);
});

test("chipTone colors by the 90/70 cutoffs", () => {
  expect(chipTone(90)).toBe("ok");
  expect(chipTone(72)).toBe("warn");
  expect(chipTone(69)).toBe("crit");
});

test("eventGlyph maps kinds to the approved glyph set", () => {
  const glyphOf = (kind: string) =>
    eventGlyph(makeEventFact({ event_kind: kind }));
  expect(glyphOf("pg.log.error_group_observed")).toEqual({
    glyph: "◆",
    tone: "crit",
  });
  expect(glyphOf("pg.database.deadlock_delta")).toEqual({
    glyph: "◆",
    tone: "crit",
  });
  expect(glyphOf("pg.checkpoint.completed")).toEqual({
    glyph: "●",
    tone: "info",
  });
  expect(glyphOf("pg.maintenance.autovacuum_reported")).toEqual({
    glyph: "○",
    tone: "dim",
  });
  expect(glyphOf("pg.lock.wait_reported")).toEqual({
    glyph: "▲",
    tone: "warn",
  });
  expect(glyphOf("pg.query.slow_group_reported")).toEqual({
    glyph: "▲",
    tone: "warn",
  });
});

test("bucketReason prefers the floor class, then the worst domain", () => {
  const withFloor = makeHealthPoint({
    floor_evidence: [{ class: "oom_kill", supporting_fact_id: "f1" }],
    domains: [{ domain: "cpu_pressure", penalty: 0.4, driving_factor_ids: [] }],
  });
  expect(bucketReason(withFloor)).toEqual({
    floor: "oom_kill",
    domain: "cpu_pressure",
  });
  const noEvidence = makeHealthPoint({ domains: [] });
  expect(bucketReason(noEvidence)).toEqual({ floor: null, domain: null });
  expect(bucketReason(null)).toEqual({ floor: null, domain: null });
});

test("anchorWindowEnd pins the grid: renders 1s apart share the same end", () => {
  const bucket = 225_000_000; // 6 h span / 96 buckets
  const t0 = 96 * bucket * 42 + 17_000_000; // mid-bucket instant
  const end0 = anchorWindowEnd(t0, bucket);
  const end1 = anchorWindowEnd(t0 + 1_000_000, bucket); // 1 s later
  expect(end1).toBe(end0);
  // The boundary is a multiple of the bucket span (absolute epoch grid).
  expect(end0 % bucket).toBe(0);
  // Crossing a bucket boundary moves the grid by exactly one bucket.
  expect(anchorWindowEnd(end0 + 1, bucket)).toBe(end0 + bucket);
});

test("scoreVerdicts drops the forming tail bucket of a live window", () => {
  const verdicts = ["ok", "crit", "warn"] as const;
  expect(scoreVerdicts([...verdicts], true)).toEqual(["ok", "crit"]);
  expect(scoreVerdicts([...verdicts], false)).toEqual([...verdicts]);
});
