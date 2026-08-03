import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { eventKindLabel, metricLabel } from "../api/codes";
import { isWarmingUp } from "../api/client";
import { useIncidents } from "../api/incidents";
import { useTimelineSpine } from "../api/spine";
import { useTimelineEvents, useTimelineHealth } from "../api/timeline";
import type { EventFact, HealthPointResponse, SpineSeries } from "../api/types";
import {
  formatByUnit,
  formatDurationUs,
  formatTimestampUs,
} from "../design/format";
import type { TimeRange } from "../state/timeGeometry";
import { Tooltip } from "./Tooltip";
import { ProvenancePopover } from "./ProvenancePopover";
import {
  aggregateEventBuckets,
  bucketReason,
  bucketVerdicts,
  chipTone,
  countWindowIncidents,
  eventGlyph,
  scoreVerdicts,
  windowScore,
  type BucketVerdict,
} from "./spineHealth";

export interface SpineProps {
  /** Cursor timestamp (int64 µs, decimal string); null = LIVE. */
  at: string | null;
  /** Window length in integer seconds (1..86400). */
  span: number;
  /** Baseline cursor (int64 µs string); null = no baseline. */
  baseline: string | null;
  /** Canonical provider-owned selected range. */
  range: TimeRange;
  /** Provider-owned ephemeral hover and draft selection. */
  hoverUs?: string | null;
  brushDraft?: TimeRange | null;
  onSelectAt: (at: string) => void;
  onSelectBaseline: (baseline: string | null) => void;
  onHover?: (hoverUs: string | null) => void;
  onBrushDraft?: (range: TimeRange | null) => void;
  onCommitRange?: (range: TimeRange) => void;
  /** Accessible explanation: coincidence and collection quality, not cause. */
  meaning?: string;
  /** The public HealthLine opts into persistent score provenance; direct
   * Spine consumers remain a query/render backend without extra chrome. */
  showProvenance?: boolean;
  /** @deprecated Navigation owns mode and prepared spans. */
  controls?: "embedded" | "external";
  /** @deprecated Navigation owns prepared spans. */
  onSelectSpan?: (span: number) => void;
  /** @deprecated Navigation owns Live/Replay. */
  onToggleLive?: () => void;
}

const SVG_HEIGHT = 40;
const SVG_WIDTH = 1000;
/** Verdict ribbon band (top of the strip). */
const RIBBON_Y = 2;
const RIBBON_H = 8;
/** Event density row baseline. */
const EVENT_LANE_BOTTOM = 23;
const EVENT_BUCKETS = 48;
/** Load sparkline band (bottom of the strip). */
const SPARK_Y = 26;
const SPARK_H = 12;
const LIVE_REFRESH_MS = 5000;
const HOUR_US = 3_600_000_000n;
const CURSOR_STEP_DIVISOR = 48n;
/** Grid resolution requested from `/v1/timeline/spine` and the ribbon. */
const SPINE_BUCKETS = 96;
/** Repeat shift-click within this share of the window clears the baseline. */
const BASELINE_CLEAR_FRACTION = 0.02;
/** Pointer jitter below this distance is a click, not a fabricated range. */
const BRUSH_THRESHOLD_PX = 4;

interface PointerGesture {
  pointerId: number;
  startClientX: number;
  startUs: string;
  dragging: boolean;
  maxDisplacementPx: number;
}

const VISUALLY_HIDDEN: React.CSSProperties = {
  position: "absolute",
  width: "1px",
  height: "1px",
  padding: 0,
  margin: "-1px",
  overflow: "hidden",
  clip: "rect(0, 0, 0, 0)",
  whiteSpace: "nowrap",
  border: 0,
};

// Primary series: the first one that carries at least one real sample. The
// API orders series by relevance (host load first, PSI after it), so we do
// not hardcode a metric code — we only skip series that came back empty.
function pickPrimarySeries(series: SpineSeries[]): SpineSeries | null {
  return (
    series.find((s) => s.values.some((v) => v !== null)) ?? series[0] ?? null
  );
}

interface BucketGeometry {
  gridFromUs: number;
  bucketSpanUs: number;
}

function bucketX(
  geom: BucketGeometry,
  index: number,
  fromUs: number,
  windowUs: number,
): number {
  const mid = geom.gridFromUs + (index + 0.5) * geom.bucketSpanUs;
  return ((mid - fromUs) / windowUs) * SVG_WIDTH;
}

/** Contiguous non-null runs of the load series as sparkline segments inside
 * the bottom band; a lone point becomes a dot — never a dangling diagonal. */
function sparkSegments(
  series: SpineSeries,
  geom: BucketGeometry,
  fromUs: number,
  windowUs: number,
): { lines: string[]; dots: Array<[number, number]> } {
  const n = series.values.length;
  const max = series.values.reduce<number>(
    (m, v) => (v !== null && v > m ? v : m),
    0,
  );
  const scale = max > 0 ? max : 1;
  const y = (v: number) =>
    SPARK_Y + SPARK_H - Math.min(1, Math.max(0, v / scale)) * SPARK_H;
  const lines: string[] = [];
  const dots: Array<[number, number]> = [];
  let current: string[] = [];
  const flush = () => {
    if (current.length >= 2) lines.push(current.join(" "));
    else if (current.length === 1) {
      const first = current[0];
      if (first !== undefined) {
        const [x, yy] = first.split(",").map(Number);
        if (x !== undefined && yy !== undefined) dots.push([x, yy]);
      }
    }
    current = [];
  };
  for (let i = 0; i < n; i++) {
    const v = series.values[i];
    if (v === null || v === undefined) {
      flush();
      continue;
    }
    current.push(
      `${bucketX(geom, i, fromUs, windowUs).toFixed(1)},${y(v).toFixed(1)}`,
    );
  }
  flush();
  return { lines, dots };
}

const VERDICT_FILL: Record<Exclude<BucketVerdict, "gap">, string> = {
  ok: "var(--sev-ok-quiet)",
  warn: "var(--sev-warn)",
  crit: "var(--sev-crit)",
};

const GLYPH_FILL = {
  crit: "var(--sev-crit)",
  warn: "var(--sev-warn)",
  info: "var(--accent)",
  dim: "var(--fg-dim)",
} as const;

const CHIP_FG = {
  ok: "var(--sev-ok-fg)",
  warn: "var(--sev-warn-fg)",
  crit: "var(--sev-crit-fg)",
} as const;

const VERDICT_FG: Record<BucketVerdict, string> = {
  ok: "var(--sev-ok-fg)",
  warn: "var(--sev-warn-fg)",
  crit: "var(--sev-crit-fg)",
  gap: "var(--fg-dim)",
};

function formatMin(minutes: number): string {
  return new Intl.NumberFormat(undefined, {
    maximumFractionDigits: minutes < 10 ? 1 : 0,
  }).format(minutes);
}

export function Spine(props: SpineProps) {
  const { t } = useTranslation();
  const live = props.at === null;
  const gestureRef = useRef<PointerGesture | null>(null);
  // Wire range follows the zoom span (exact BigInt math); the URL codec
  // enforces the public 1 second..24 hour bound.
  const windowUs = BigInt(props.span) * 1_000_000n;
  const windowNum = props.span * 1_000_000;
  // The endpoint parses `step` as u64. Ceil keeps arbitrary accepted spans
  // integral and yields at most the requested 96 health buckets.
  const bucketSpanUs = Math.ceil(windowNum / SPINE_BUCKETS);
  const toBig = BigInt(props.range.toUs);
  const fromBig = BigInt(props.range.fromUs);
  const from = fromBig.toString();
  const to = toBig.toString();
  const prevFrom = (fromBig - windowUs).toString();
  // Absolute timestamps are converted only for the existing bounded SVG
  // geometry; every API query above keeps the exact provider-owned strings.
  const fromUs = Number(fromBig);
  const toUs = fromUs + windowNum;
  // LIVE: the last bucket is still forming — hatched, out of the score.
  const hasFormingTail = live;

  // Query keys ride the anchored grid, so they only change on a bucket
  // boundary; within a bucket freshness comes from the refetch interval
  // (the snapshot cadence), and keepPreviousData holds the last answer
  // across a boundary — no placeholder flash.
  const liveOpts = { refetchInterval: live ? LIVE_REFRESH_MS : undefined };
  // Health and incidents are queried over the doubled window so the score
  // delta against the previous window costs no extra requests.
  const health = useTimelineHealth(
    { from: prevFrom, to, step: bucketSpanUs },
    liveOpts,
  );
  const spine = useTimelineSpine(
    { from, to, buckets: SPINE_BUCKETS },
    liveOpts,
  );
  const events = useTimelineEvents({ from, to, limit: 50 }, liveOpts);
  const incidents = useIncidents({ from: prevFrom, to }, liveOpts);

  const timestampX = (timestampUs: string | number): number => {
    const value =
      typeof timestampUs === "number"
        ? BigInt(Math.round(timestampUs))
        : BigInt(timestampUs);
    return (Number(value - fromBig) / windowNum) * SVG_WIDTH;
  };
  const allSeries = spine.data?.series ?? [];
  const primary = pickPrimarySeries(allSeries);
  const geom: BucketGeometry | null = spine.data
    ? {
        gridFromUs: Number(spine.data.grid.from_us),
        bucketSpanUs:
          (Number(spine.data.grid.to_us) - Number(spine.data.grid.from_us)) /
          Math.max(1, spine.data.grid.bucket_count),
      }
    : null;
  const { lines, dots } =
    primary !== null && geom !== null
      ? sparkSegments(primary, geom, fromUs, windowNum)
      : { lines: [], dots: [] };
  const baselineUs = props.baseline;
  const visibleEvents = (events.data?.events ?? []).filter((e) => {
    const ts = e.occurred_at_us ?? e.sort_ts_us;
    return ts >= fromUs && ts <= toUs;
  });
  const eventBuckets = aggregateEventBuckets(
    visibleEvents,
    fromUs,
    toUs,
    EVENT_BUCKETS,
  );
  const maxEventCount = Math.max(
    1,
    ...eventBuckets.map((bucket) => bucket?.count ?? 0),
  );
  const visibleEventCount = visibleEvents.reduce(
    (total, event) => total + Math.max(1, event.occurrence_count),
    0,
  );

  // Health points split into the current window and the previous one by
  // interval midpoint; buckets without a point stay honest gaps.
  const points = health.data?.points ?? [];
  const pointMid = (p: HealthPointResponse) =>
    (p.interval.from_us + p.interval.to_us) / 2;
  const currentPoints = points.filter((p) => pointMid(p) >= fromUs);
  const previousPoints = points.filter((p) => pointMid(p) < fromUs);
  const verdicts = bucketVerdicts(currentPoints, fromUs, toUs, SPINE_BUCKETS);
  const previousVerdicts = bucketVerdicts(
    previousPoints,
    fromUs - windowNum,
    fromUs,
    SPINE_BUCKETS,
  );
  const allIncidents = incidents.data?.incidents ?? [];
  // Score input excludes the forming tail bucket: the score only moves on a
  // completed-bucket boundary or an incident opening/closing (variant 2A).
  const scored = scoreVerdicts(verdicts, hasFormingTail);
  const score = windowScore(
    scored,
    (props.span * scored.length) / SPINE_BUCKETS,
    countWindowIncidents(allIncidents, fromUs, toUs),
  );
  const previousScore = windowScore(
    previousVerdicts,
    props.span,
    countWindowIncidents(allIncidents, fromUs - windowNum, fromUs),
  );
  const observedBuckets = scored.filter((verdict) => verdict !== "gap").length;
  const hasCurrent = observedBuckets > 0;
  const hasPrevious = previousVerdicts.some((v) => v !== "gap");
  const rawDelta =
    hasCurrent && hasPrevious ? score.score - previousScore.score : null;
  const tone = chipTone(score.score);

  const currentValue = (() => {
    if (primary === null) return null;
    for (let i = primary.values.length - 1; i >= 0; i--) {
      const v = primary.values[i];
      if (v !== null && v !== undefined) return v;
    }
    return null;
  })();

  const counts = {
    crit: scored.filter((v) => v === "crit").length,
    warn: scored.filter((v) => v === "warn").length,
    ok: scored.filter((v) => v === "ok").length,
  };

  // Gap reason from the spine quality report: a hole carries its cause
  // (producer restart, index build), never a bare "no data".
  const gapReasonFor = (bucketStart: number, bucketEnd: number): string => {
    const gaps = spine.data?.quality.gaps ?? [];
    const hit = gaps.find(
      (g) => Number(g.from_us) < bucketEnd && Number(g.to_us) > bucketStart,
    );
    if (hit === undefined) return "";
    return t(`spine.gap.${hit.reason}`, { defaultValue: "" });
  };

  /** Health point covering a bucket (for the tooltip reason). */
  const pointForBucket = (index: number): HealthPointResponse | null => {
    const start = fromUs + (index / SPINE_BUCKETS) * windowNum;
    const end = fromUs + ((index + 1) / SPINE_BUCKETS) * windowNum;
    return (
      currentPoints.find(
        (p) => p.interval.from_us < end && p.interval.to_us > start,
      ) ?? null
    );
  };

  const pickUs = (target: SVGSVGElement, clientX: number): string => {
    const rect = target.getBoundingClientRect();
    const fraction = Math.min(
      1,
      Math.max(0, rect.width > 0 ? (clientX - rect.left) / rect.width : 0),
    );
    return (fromBig + BigInt(Math.round(fraction * windowNum))).toString();
  };

  const selectPoint = (us: string, shiftKey: boolean) => {
    if (shiftKey) {
      if (
        baselineUs !== null &&
        Number(
          BigInt(us) >= BigInt(baselineUs)
            ? BigInt(us) - BigInt(baselineUs)
            : BigInt(baselineUs) - BigInt(us),
        ) <=
          BASELINE_CLEAR_FRACTION * windowNum
      ) {
        props.onSelectBaseline(null);
      } else {
        props.onSelectBaseline(us);
      }
    } else {
      props.onSelectAt(us);
    }
  };

  const rangeBetween = (leftUs: string, rightUs: string): TimeRange =>
    BigInt(leftUs) <= BigInt(rightUs)
      ? { fromUs: leftUs, toUs: rightUs }
      : { fromUs: rightUs, toUs: leftUs };

  const onStripPointerDown = (e: React.PointerEvent<SVGSVGElement>) => {
    if (e.button !== 0) return;
    const startUs = pickUs(e.currentTarget, e.clientX);
    gestureRef.current = {
      pointerId: e.pointerId,
      startClientX: e.clientX,
      startUs,
      dragging: false,
      maxDisplacementPx: 0,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onStripPointerMove = (e: React.PointerEvent<SVGSVGElement>) => {
    const currentUs = pickUs(e.currentTarget, e.clientX);
    props.onHover?.(currentUs);
    const gesture = gestureRef.current;
    if (gesture === null || gesture.pointerId !== e.pointerId) return;
    gesture.maxDisplacementPx = Math.max(
      gesture.maxDisplacementPx,
      Math.abs(e.clientX - gesture.startClientX),
    );
    if (!gesture.dragging && gesture.maxDisplacementPx >= BRUSH_THRESHOLD_PX) {
      gesture.dragging = true;
    }
    if (gesture.dragging) {
      props.onBrushDraft?.(rangeBetween(gesture.startUs, currentUs));
    }
  };

  const finishPointer = (e: React.PointerEvent<SVGSVGElement>) => {
    const gesture = gestureRef.current;
    if (gesture === null || gesture.pointerId !== e.pointerId) return;
    const currentUs = pickUs(e.currentTarget, e.clientX);
    const finalDisplacementPx = Math.abs(e.clientX - gesture.startClientX);
    const dragging =
      gesture.dragging ||
      Math.max(gesture.maxDisplacementPx, finalDisplacementPx) >=
        BRUSH_THRESHOLD_PX;
    gestureRef.current = null;
    if (e.currentTarget.hasPointerCapture?.(e.pointerId) ?? true) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    props.onBrushDraft?.(null);
    props.onHover?.(null);
    if (dragging) {
      props.onCommitRange?.(rangeBetween(gesture.startUs, currentUs));
    } else {
      selectPoint(currentUs, e.shiftKey);
    }
  };

  const cancelPointer = (e: React.PointerEvent<SVGSVGElement>) => {
    if (gestureRef.current?.pointerId !== e.pointerId) return;
    gestureRef.current = null;
    if (e.currentTarget.hasPointerCapture?.(e.pointerId) ?? true) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    props.onBrushDraft?.(null);
    props.onHover?.(null);
  };

  const onStripKeyDown = (e: React.KeyboardEvent<SVGSVGElement>) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      // Own the key fully: the global handler must not also apply its step.
      e.preventDefault();
      e.stopPropagation();
      const step = e.shiftKey
        ? HOUR_US
        : (BigInt(Math.max(1, props.span)) * 1_000_000n) / CURSOR_STEP_DIVISOR;
      const delta = e.key === "ArrowLeft" ? -step : step;
      props.onSelectAt((toBig + delta).toString());
    }
  };

  const cursorLabel = formatTimestampUs(toUs);

  // "Warming up" is only honest on a cold start — not one successful
  // response yet. With any data on screen (kept or fresh), a 503 window is
  // a real ribbon with honest gap cells, never a placeholder.
  const evidenceQueries = [health, spine, events] as const;
  const cold = evidenceQueries.every((query) => query.data === undefined);
  const retrying = [...evidenceQueries, incidents].some(
    (q) => q.isPending || (q.error !== null && isWarmingUp(q.error)),
  );
  const warming = cold && retrying;
  const sourceFailures = [
    health.error !== null && !isWarmingUp(health.error) ? "health" : null,
    spine.error !== null && !isWarmingUp(spine.error) ? "spine" : null,
    events.error !== null && !isWarmingUp(events.error) ? "events" : null,
    incidents.error !== null && !isWarmingUp(incidents.error)
      ? "incidents"
      : null,
  ].filter((code): code is string => code !== null);
  // Score provenance is narrower than the persistent Health-line evidence:
  // the local formula consumes only server health verdicts and incidents.
  // Spine series and event glyph availability cannot change this score.
  const scoreHealthUnavailable =
    health.error !== null && !isWarmingUp(health.error);
  const scoreIncidentsUnavailable = incidents.error !== null;
  const scoreHealthPending =
    health.data === undefined && !scoreHealthUnavailable;
  const scoreIncidentsPending =
    incidents.data === undefined && !scoreIncidentsUnavailable;
  const failed =
    cold &&
    !warming &&
    [health, spine, events].every(
      (query) => query.error !== null && !isWarmingUp(query.error),
    );
  const empty =
    !cold &&
    !failed &&
    health.data !== undefined &&
    spine.data !== undefined &&
    events.data !== undefined &&
    verdicts.every((v) => v === "gap") &&
    allSeries.every((s) => s.values.every((v) => v === null)) &&
    visibleEvents.length === 0;

  const sourceNames = sourceFailures.map((code) =>
    t(`healthLine.source.${code}`),
  );
  const sourceList = sourceNames.join(", ");
  const sourcesLoading = [...evidenceQueries, incidents].some(
    (query) => query.isPending && query.data === undefined,
  );
  const scoreCoverageComplete =
    scored.length > 0 && observedBuckets === scored.length;
  const scoreDisplayable =
    hasCurrent &&
    scoreCoverageComplete &&
    !scoreHealthUnavailable &&
    !scoreIncidentsUnavailable &&
    !scoreHealthPending &&
    !scoreIncidentsPending;
  const scoreState = scoreDisplayable
    ? null
    : scoreHealthUnavailable ||
        scoreIncidentsUnavailable ||
        scoreHealthPending ||
        scoreIncidentsPending ||
        hasCurrent
      ? "partial"
      : "gap";
  const scoreStateReason = scoreHealthUnavailable
    ? t("healthLine.provenance.healthUnavailable")
    : scoreIncidentsUnavailable
      ? t("spine.score.incidentsUnavailable")
      : scoreHealthPending || scoreIncidentsPending
        ? t("healthLine.provenance.inputsLoading")
        : hasCurrent && !scoreCoverageComplete
          ? t("healthLine.provenance.incompleteCoverage", {
              observed: observedBuckets,
              total: scored.length,
            })
          : t("healthLine.provenance.noVerdicts");
  const delta = scoreDisplayable ? rawDelta : null;
  const sourceStatus = failed
    ? t("healthLine.sources.unavailable")
    : sourceFailures.length > 0
      ? t("healthLine.sources.partial", { sources: sourceList })
      : sourcesLoading
        ? t("healthLine.sources.loading")
        : t("healthLine.sources.complete");
  const currentVerdict = verdicts[verdicts.length - 1] ?? "gap";
  const currentVerdictLabel = t(`healthLine.verdict.${currentVerdict}`);
  const gapCount = scored.filter((verdict) => verdict === "gap").length;
  const evidenceSummary = t("healthLine.evidenceSummary", {
    ok: counts.ok,
    warn: counts.warn,
    crit: counts.crit,
    gap: gapCount,
    current: currentVerdictLabel,
    window: hasFormingTail
      ? t("healthLine.window.forming")
      : t("healthLine.window.complete"),
    sources: sourceStatus,
  });

  const glyphOf = (e: EventFact) => eventGlyph(e);

  return (
    <section
      aria-label={t("spine.caption")}
      aria-describedby="health-line-meaning health-line-evidence-summary"
      data-testid="health-line"
      data-shell-region="health-line"
      style={{
        display: "flex",
        alignItems: "center",
        gap: "8px",
        flexWrap: "nowrap",
        height: "60px",
        minHeight: "60px",
        boxSizing: "border-box",
        padding: "4px 8px",
        background: "var(--bg-raised)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-md)",
        fontFamily: "var(--ui-font)",
      }}
    >
      <span
        style={{
          fontSize: "var(--text-xs)",
          fontWeight: 600,
          textTransform: "uppercase",
          letterSpacing: "var(--tracking-caps)",
          color: "var(--fg-dim)",
        }}
      >
        {t("spine.caption")}
      </span>
      <span
        id="health-line-meaning"
        data-testid="health-line-meaning"
        style={VISUALLY_HIDDEN}
      >
        {props.meaning ?? t("healthLine.coincidence")}
      </span>
      <span
        id="health-line-evidence-summary"
        role="status"
        style={VISUALLY_HIDDEN}
      >
        {evidenceSummary}
      </span>
      {sourceFailures.length > 0 && !failed && (
        <span
          data-testid="health-line-source-state"
          title={t("healthLine.partial", { sources: sourceList })}
          style={{
            maxWidth: "220px",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            color: "var(--sev-warn-fg)",
            fontSize: "var(--text-xs)",
          }}
        >
          {`⚠ ${t("healthLine.partial", { sources: sourceList })}`}
        </span>
      )}
      {!warming && !failed && !empty && (
        <Tooltip
          content={
            <span>
              {t("spine.score.formula")}
              <br />
              {t("spine.score.breakdown", {
                crit: formatMin(score.critMin),
                warn: formatMin(score.warnMin),
                count: score.incidents,
              })}
              {incidents.error !== null && (
                <>
                  <br />
                  {t("spine.score.incidentsUnavailable")}
                </>
              )}
            </span>
          }
        >
          <span
            data-testid="spine-score"
            aria-label={t("spine.score.aria")}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: "8px",
              whiteSpace: "nowrap",
              lineHeight: 1.2,
            }}
          >
            <span style={{ textAlign: "center" }}>
              <span
                style={{
                  display: "block",
                  fontFamily: "var(--mono-font)",
                  fontSize: 20,
                  fontWeight: 600,
                  color: scoreDisplayable ? CHIP_FG[tone] : "var(--fg-dim)",
                }}
              >
                {scoreDisplayable ? score.score : "—"}
              </span>
              <span
                style={{
                  display: "block",
                  fontSize: "var(--text-xs)",
                  color: VERDICT_FG[currentVerdict],
                }}
              >
                {`${t("spine.score.caption")} · ${t("healthLine.currentVerdict", { verdict: currentVerdictLabel })}`}
              </span>
              {scoreState !== null && (
                <span
                  data-testid="health-score-state"
                  title={scoreStateReason}
                  style={{
                    display: "block",
                    color:
                      scoreState === "partial"
                        ? "var(--sev-warn-fg)"
                        : "var(--fg-dim)",
                    fontSize: "var(--text-xs)",
                  }}
                >
                  {t(`semantic.state.${scoreState}.label`)}
                </span>
              )}
            </span>
            <span
              style={{
                fontSize: "var(--text-xs)",
                color: "var(--fg-dim)",
              }}
            >
              {delta === null ? (
                <span data-testid="spine-score-delta">
                  {t("spine.score.noPrev")}
                </span>
              ) : (
                <>
                  {t(`spine.span.${props.span}`)}
                  <br />
                  <span
                    data-testid="spine-score-delta"
                    style={{
                      fontFamily: "var(--mono-font)",
                      color:
                        delta < 0 ? "var(--sev-crit-fg)" : "var(--sev-ok-fg)",
                    }}
                  >
                    {`${delta < 0 ? "▼" : "▲"}${Math.abs(delta)}`}
                  </span>{" "}
                  {t("spine.score.vsPrev")}
                </>
              )}
            </span>
          </span>
        </Tooltip>
      )}
      {!warming && !failed && !empty && props.showProvenance === true && (
        <ProvenancePopover
          triggerLabel={t("healthLine.provenance.trigger")}
          renderTrigger={() => t("healthLine.provenance.short")}
          record={{
            definition: `${t("healthLine.provenance.definition")} ${props.meaning ?? t("healthLine.coincidence")}`,
            value: scoreDisplayable ? score.score : "—",
            unit: t("healthLine.provenance.unit"),
            window: `${from}–${to}`,
            aggregation: t("healthLine.provenance.localAggregate"),
            formula: t("spine.score.formula"),
            // This comparison belongs to the score itself: the preceding
            // same-width window. It is not the operator's heatmap baseline.
            baseline: `${prevFrom}–${from}`,
            source: t("healthLine.provenance.source"),
            coverage: `${observedBuckets}/${scored.length} · ${t("healthLine.provenance.completedBuckets")}`,
            sampling: `${bucketSpanUs} µs · ${t("healthLine.provenance.formingTailExcluded")}`,
            verdictRule: t("healthLine.provenance.verdictRule"),
            revision: health.data?.health_policy_version,
            state: scoreState ?? undefined,
            reason: scoreState === null ? undefined : scoreStateReason,
          }}
        />
      )}
      {warming || failed || empty ? (
        <span
          data-testid="spine-state"
          style={{
            flex: 1,
            minWidth: "200px",
            fontSize: "var(--text-xs)",
            color: "var(--fg-dim)",
          }}
        >
          {failed
            ? t("spine.error")
            : empty
              ? t("spine.missing")
              : t("loading.warming")}
        </span>
      ) : (
        <svg
          role="slider"
          tabIndex={0}
          aria-label={t("spine.timeline")}
          aria-describedby="health-line-evidence-summary"
          {...({
            // React's ARIA types only admit `number`, but converting an int64
            // µs timestamp would lose precision. Attribute values are strings
            // in the DOM, so preserve the provider-owned decimal boundary.
            "aria-valuemin": from,
            "aria-valuemax": to,
            "aria-valuenow": to,
          } as unknown as React.AriaAttributes)}
          aria-valuetext={cursorLabel}
          viewBox={`0 0 ${SVG_WIDTH} ${SVG_HEIGHT}`}
          preserveAspectRatio="none"
          onPointerDown={onStripPointerDown}
          onPointerMove={onStripPointerMove}
          onPointerUp={finishPointer}
          onPointerCancel={cancelPointer}
          onLostPointerCapture={cancelPointer}
          onKeyDown={onStripKeyDown}
          onPointerLeave={() => {
            if (gestureRef.current === null) props.onHover?.(null);
          }}
          style={{
            flex: 1,
            minWidth: "200px",
            height: `${SVG_HEIGHT}px`,
            display: "block",
            background: "var(--bg)",
            cursor: "crosshair",
          }}
        >
          {/* Forming-bucket hatch: diagonal hairlines over the dimmed
              verdict of the still-open tail bucket. */}
          <defs>
            <pattern
              id="spine-forming-hatch"
              width="3"
              height="3"
              patternUnits="userSpaceOnUse"
              patternTransform="rotate(45)"
            >
              <line
                x1="0"
                y1="0"
                x2="0"
                y2="3"
                stroke="var(--fg-dim)"
                strokeWidth="0.7"
                opacity="0.6"
              />
            </pattern>
            <pattern
              id="spine-gap-hatch"
              width="5"
              height="5"
              patternUnits="userSpaceOnUse"
              patternTransform="rotate(45)"
            >
              <line
                x1="0"
                y1="0"
                x2="0"
                y2="5"
                stroke="var(--fg-dim)"
                strokeWidth="0.45"
                opacity="0.45"
              />
            </pattern>
          </defs>
          {!live && (
            <rect
              data-testid="health-selected-range"
              x={0}
              y={0}
              width={SVG_WIDTH}
              height={SVG_HEIGHT}
              fill="var(--active-bg)"
              opacity="0.16"
              pointerEvents="none"
            />
          )}
          {/* Verdict ribbon: one cell per bucket, calm cells quiet, warn/crit
              full-saturation; a bucket with no server verdict is an explicit
              gap substrate (raised band + hairline), never a silent hole. */}
          {verdicts.map((v, i) => {
            const w = SVG_WIDTH / SPINE_BUCKETS;
            const x = i * w;
            const cellW = Math.max(w - 1, 0.5);
            const bucketStart = Math.round(
              fromUs + (i / SPINE_BUCKETS) * windowNum,
            );
            const bucketEnd = Math.round(
              fromUs + ((i + 1) / SPINE_BUCKETS) * windowNum,
            );
            const fmt = new Intl.DateTimeFormat(undefined, {
              hour: "2-digit",
              minute: "2-digit",
              hour12: false,
            });
            const when = `${fmt.format(new Date(bucketStart / 1000))}–${fmt.format(new Date(bucketEnd / 1000))} · ${t("spine.bucketSpan", { duration: formatDurationUs(bucketEnd - bucketStart) })}`;
            const forming = hasFormingTail && i === SPINE_BUCKETS - 1;
            const point = pointForBucket(i);
            const reason = bucketReason(point);
            const reasonText =
              reason.floor !== null
                ? t(`health.floor.${reason.floor}`)
                : reason.domain !== null
                  ? t(`health.domain.${reason.domain}`)
                  : null;
            const bucketEvents = visibleEvents.filter((e) => {
              const ts = e.occurred_at_us ?? e.sort_ts_us;
              return ts >= bucketStart && ts < bucketEnd;
            });
            const gapReason =
              v === "gap" ? gapReasonFor(bucketStart, bucketEnd) : "";
            const verdictLabel =
              v === "gap"
                ? `${t("spine.missing")}${gapReason !== "" ? `: ${gapReason}` : ""}`
                : t(`spine.verdict.${v}`);
            const tip = [
              when,
              `${verdictLabel}${reasonText !== null ? `: ${reasonText}` : ""}${forming ? ` · ${t("spine.forming")}` : ""}`,
              ...bucketEvents.map(
                (e) => `${glyphOf(e).glyph} ${eventKindLabel(t, e.event_kind)}`,
              ),
            ].join("\n");
            return (
              <g key={i}>
                {v === "gap" ? (
                  <rect
                    data-testid="spine-ribbon-gap"
                    x={x}
                    y={RIBBON_Y}
                    width={cellW}
                    height={RIBBON_H}
                    fill="var(--bg-raised)"
                    stroke="var(--border)"
                    strokeWidth="0.5"
                  >
                    <title>{tip}</title>
                  </rect>
                ) : (
                  <rect
                    data-testid={`spine-ribbon-${v}`}
                    x={x}
                    y={RIBBON_Y}
                    width={cellW}
                    height={RIBBON_H}
                    rx={1}
                    fill={VERDICT_FILL[v]}
                    opacity={forming ? 0.5 : 1}
                  >
                    <title>{tip}</title>
                  </rect>
                )}
                {forming && (
                  <rect
                    data-testid="spine-ribbon-forming"
                    x={x}
                    y={RIBBON_Y}
                    width={cellW}
                    height={RIBBON_H}
                    fill="url(#spine-forming-hatch)"
                    pointerEvents="none"
                  />
                )}
              </g>
            );
          })}
          {/* Load sparkline under the ribbon, same X axis. */}
          {lines.map((points, i) => (
            <polyline
              key={i}
              data-testid="spine-load-line"
              points={points}
              fill="none"
              stroke="var(--accent)"
              strokeWidth="1.2"
              opacity="0.8"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {dots.map(([x, y], i) => (
            <circle
              key={i}
              data-testid="spine-lone-point"
              cx={x}
              cy={y}
              r={1.5}
              fill="var(--accent)"
            />
          ))}
          {props.brushDraft !== null && props.brushDraft !== undefined && (
            <rect
              data-testid="health-brush-draft"
              x={Math.max(0, timestampX(props.brushDraft.fromUs))}
              y={0}
              width={Math.max(
                0,
                Math.min(SVG_WIDTH, timestampX(props.brushDraft.toUs)) -
                  Math.max(0, timestampX(props.brushDraft.fromUs)),
              )}
              height={SVG_HEIGHT}
              fill="var(--accent)"
              opacity="0.18"
              pointerEvents="none"
            />
          )}
          {verdicts.map((verdict, index) =>
            verdict === "gap" ? (
              <rect
                key={`gap-hatch-${index}`}
                data-testid="spine-gap-hatch"
                x={(index * SVG_WIDTH) / SPINE_BUCKETS}
                y={RIBBON_Y}
                width={Math.max(SVG_WIDTH / SPINE_BUCKETS - 1, 0.5)}
                height={RIBBON_H}
                fill="url(#spine-gap-hatch)"
                pointerEvents="none"
              />
            ) : null,
          )}
          {baselineUs !== null &&
            BigInt(baselineUs) >= fromBig &&
            BigInt(baselineUs) <= toBig && (
              <line
                data-testid="spine-baseline"
                x1={timestampX(baselineUs)}
                x2={timestampX(baselineUs)}
                y1={0}
                y2={SVG_HEIGHT}
                stroke="var(--accent)"
                strokeWidth="1"
                strokeDasharray="4 3"
                vectorEffect="non-scaling-stroke"
              />
            )}
          {!live && (
            <line
              data-testid="spine-cursor"
              x1={SVG_WIDTH}
              x2={SVG_WIDTH}
              y1={0}
              y2={SVG_HEIGHT}
              stroke="var(--fg)"
              strokeWidth="1"
              vectorEffect="non-scaling-stroke"
            />
          )}
          {props.hoverUs !== null && props.hoverUs !== undefined && (
            <line
              data-testid="health-hover-cursor"
              x1={timestampX(props.hoverUs)}
              x2={timestampX(props.hoverUs)}
              y1={0}
              y2={SVG_HEIGHT}
              stroke="var(--fg-dim)"
              strokeWidth="1"
              strokeDasharray="3 3"
              vectorEffect="non-scaling-stroke"
            />
          )}
          {eventBuckets.map((bucket, index) => {
            if (bucket === null) return null;
            const width = SVG_WIDTH / EVENT_BUCKETS;
            const x = index * width;
            const height = 3 + (bucket.count / maxEventCount) * 8;
            const kinds = new Map<string, number>();
            for (const event of bucket.events) {
              kinds.set(
                event.event_kind,
                (kinds.get(event.event_kind) ?? 0) +
                  Math.max(1, event.occurrence_count),
              );
            }
            const detail = [...kinds.entries()]
              .sort((left, right) => right[1] - left[1])
              .slice(0, 3)
              .map(
                ([kind, count]) =>
                  `${eventKindLabel(t, kind)}${count > 1 ? ` ×${count}` : ""}`,
              )
              .join(" · ");
            return (
              <rect
                key={index}
                data-testid="spine-event-density"
                data-event-count={bucket.count}
                x={x + 1}
                y={EVENT_LANE_BOTTOM - height}
                width={Math.max(2, width - 2)}
                height={height}
                rx={1.5}
                fill={GLYPH_FILL[bucket.tone]}
                opacity={0.82}
              >
                <title>{`${t("healthLine.eventBucket", { count: bucket.count })} · ${detail}`}</title>
              </rect>
            );
          })}
        </svg>
      )}
      {!warming && !failed && !empty && (
        <span
          data-testid="spine-right"
          style={{
            display: "inline-flex",
            flexDirection: "column",
            alignItems: "flex-end",
            color: "var(--fg-dim)",
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-xs)",
            lineHeight: 1.4,
            whiteSpace: "nowrap",
          }}
        >
          <span data-testid="spine-cursor-time">{cursorLabel}</span>
          <span
            data-testid="spine-summary"
            title={t("spine.counts", {
              crit: counts.crit,
              warn: counts.warn,
              ok: counts.ok,
            })}
          >
            <span
              data-testid="health-line-quality"
              aria-label={t("healthLine.quality", {
                observed: observedBuckets,
                total: scored.length,
              })}
            >
              {`Q ${observedBuckets}/${scored.length}`}
            </span>{" "}
            ·{" "}
            <span style={{ color: "var(--fg)" }}>
              {t("healthLine.events", { count: visibleEventCount })}
            </span>{" "}
            ·{" "}
            {primary !== null && currentValue !== null && (
              <>
                {metricLabel(t, "os", primary.code)}{" "}
                {formatByUnit(currentValue, primary.unit)} ·{" "}
              </>
            )}
            <span style={{ color: "var(--sev-crit-fg)" }}>▲{counts.crit}</span>{" "}
            <span style={{ color: "var(--sev-warn-fg)" }}>●{counts.warn}</span>
          </span>
        </span>
      )}
    </section>
  );
}
