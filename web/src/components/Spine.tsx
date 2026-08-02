import { useState } from "react";
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
import { SPANS } from "../state/url";
import type { TimeRange } from "../state/timeGeometry";
import { Tooltip } from "./Tooltip";
import {
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
  /** Window length in seconds (900 / 3600 / 21600 / 86400). */
  span: number;
  /** Baseline cursor (int64 µs string); null = no baseline. */
  baseline: string | null;
  /** Canonical provider-owned selected range. */
  range: TimeRange;
  onSelectAt: (at: string) => void;
  onSelectSpan: (span: number) => void;
  onSelectBaseline: (baseline: string | null) => void;
  onToggleLive: () => void;
}

const SVG_HEIGHT = 40;
const SVG_WIDTH = 1000;
/** Verdict ribbon band (top of the strip). */
const RIBBON_Y = 2;
const RIBBON_H = 8;
/** Event glyph row baseline. */
const GLYPH_Y = 20;
/** Load sparkline band (bottom of the strip). */
const SPARK_Y = 26;
const SPARK_H = 12;
const LIVE_REFRESH_MS = 5000;
const KEYBOARD_STEP_US = 300 * 1_000_000;
/** Grid resolution requested from `/v1/timeline/spine` and the ribbon. */
const SPINE_BUCKETS = 96;
/** Repeat shift-click within this share of the window clears the baseline. */
const BASELINE_CLEAR_FRACTION = 0.02;

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

function formatMin(minutes: number): string {
  return new Intl.NumberFormat(undefined, {
    maximumFractionDigits: minutes < 10 ? 1 : 0,
  }).format(minutes);
}

export function Spine(props: SpineProps) {
  const { t } = useTranslation();
  const live = props.at === null;
  const [hoverUs, setHoverUs] = useState<number | null>(null);
  // Wire range follows the zoom span (exact BigInt math); the 24 h bound is
  // enforced by the span whitelist in the URL codec.
  const windowUs = BigInt(props.span) * 1_000_000n;
  const windowNum = props.span * 1_000_000;
  const bucketSpanUs = windowNum / SPINE_BUCKETS;
  const toBig = BigInt(props.range.toUs);
  const fromBig = BigInt(props.range.fromUs);
  const from = fromBig.toString();
  const to = toBig.toString();
  const prevFrom = (fromBig - windowUs).toString();
  // Absolute timestamps are converted only for the existing bounded SVG
  // geometry; every API query above keeps the exact provider-owned strings.
  const toUs = Number(toBig);
  const fromUs = toUs - windowNum;
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

  const cursorX = (us: number) => ((us - fromUs) / windowNum) * SVG_WIDTH;
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
  const baselineUs = props.baseline !== null ? Number(props.baseline) : null;
  const visibleEvents = (events.data?.events ?? []).filter((e) => {
    const ts = e.occurred_at_us ?? e.sort_ts_us;
    return ts >= fromUs && ts <= toUs;
  });

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
  const hasPrevious = previousVerdicts.some((v) => v !== "gap");
  const delta = hasPrevious ? score.score - previousScore.score : null;
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

  const pickUs = (e: React.MouseEvent<SVGSVGElement>): number => {
    const rect = e.currentTarget.getBoundingClientRect();
    const fraction = (e.clientX - rect.left) / rect.width;
    return Math.round(fromUs + fraction * windowNum);
  };

  const onStripClick = (e: React.MouseEvent<SVGSVGElement>) => {
    const us = pickUs(e);
    if (e.shiftKey) {
      if (
        baselineUs !== null &&
        Math.abs(us - baselineUs) <= BASELINE_CLEAR_FRACTION * windowNum
      ) {
        props.onSelectBaseline(null);
      } else {
        props.onSelectBaseline(String(us));
      }
    } else {
      props.onSelectAt(String(us));
    }
  };

  const onStripKeyDown = (e: React.KeyboardEvent<SVGSVGElement>) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      // Own the key fully: the global handler must not also apply its step.
      e.preventDefault();
      e.stopPropagation();
      const delta =
        e.key === "ArrowLeft" ? -KEYBOARD_STEP_US : KEYBOARD_STEP_US;
      props.onSelectAt(String(toUs + delta));
    }
  };

  const cursorLabel = formatTimestampUs(toUs);

  // "Warming up" is only honest on a cold start — not one successful
  // response yet. With any data on screen (kept or fresh), a 503 window is
  // a real ribbon with honest gap cells, never a placeholder.
  const cold = health.data === undefined && spine.data === undefined;
  const retrying = [health, spine, incidents].some(
    (q) => q.isPending || (q.error !== null && isWarmingUp(q.error)),
  );
  const warming = cold && retrying;
  const failed =
    cold &&
    !warming &&
    [health, spine].some((q) => q.error !== null && !isWarmingUp(q.error));
  const empty =
    !cold &&
    !failed &&
    health.data !== undefined &&
    spine.data !== undefined &&
    verdicts.every((v) => v === "gap") &&
    allSeries.every((s) => s.values.every((v) => v === null));

  const glyphOf = (e: EventFact) => eventGlyph(e);

  return (
    <section
      aria-label={t("spine.caption")}
      style={{
        display: "flex",
        alignItems: "center",
        gap: "8px",
        flexWrap: "wrap",
        padding: "4px 12px",
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
      <button
        type="button"
        aria-pressed={!live}
        onClick={props.onToggleLive}
        style={{
          fontSize: "var(--text-xs)",
          fontWeight: 600,
          letterSpacing: "var(--tracking-caps)",
          color: live ? "var(--sev-ok-fg)" : "var(--sev-warn-fg)",
          background: live ? "var(--sev-ok-bg)" : "var(--sev-warn-bg)",
          border: `1px solid ${live ? "var(--sev-ok)" : "var(--sev-warn)"}`,
          borderRadius: "var(--radius-sm)",
          padding: "1px 8px",
          cursor: "pointer",
        }}
      >
        {live ? t("spine.live") : t("spine.replay")}
      </button>
      <div
        role="group"
        aria-label={t("spine.zoom")}
        style={{
          display: "inline-flex",
          alignItems: "center",
          background: "var(--bg)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-sm)",
          overflow: "hidden",
        }}
      >
        {SPANS.map((s) => (
          <button
            key={s}
            type="button"
            aria-pressed={props.span === s}
            onClick={() => props.onSelectSpan(s)}
            style={{
              fontSize: "var(--text-xs)",
              padding: "2px 8px",
              border: "none",
              background: props.span === s ? "var(--active-bg)" : "transparent",
              color:
                props.span === s ? "var(--accent-strong)" : "var(--fg-dim)",
              cursor: "pointer",
              transition:
                "color var(--transition-fast), background var(--transition-fast)",
            }}
          >
            {t(`spine.span.${s}`)}
          </button>
        ))}
      </div>
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
                  color: CHIP_FG[tone],
                }}
              >
                {score.score}
              </span>
              <span
                style={{
                  display: "block",
                  fontSize: "var(--text-xs)",
                  color: "var(--fg-dim)",
                }}
              >
                {t("spine.score.caption")}
              </span>
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
          aria-valuemin={fromUs}
          aria-valuemax={toUs}
          aria-valuenow={toUs}
          viewBox={`0 0 ${SVG_WIDTH} ${SVG_HEIGHT}`}
          preserveAspectRatio="none"
          onClick={onStripClick}
          onKeyDown={onStripKeyDown}
          onMouseMove={(e) => {
            const rect = e.currentTarget.getBoundingClientRect();
            const fraction = (e.clientX - rect.left) / rect.width;
            setHoverUs(Math.round(fromUs + fraction * windowNum));
          }}
          onMouseLeave={() => setHoverUs(null)}
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
          </defs>
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
          {baselineUs !== null &&
            baselineUs >= fromUs &&
            baselineUs <= toUs && (
              <line
                data-testid="spine-baseline"
                x1={cursorX(baselineUs)}
                x2={cursorX(baselineUs)}
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
          {hoverUs !== null && (
            <line
              x1={cursorX(hoverUs)}
              x2={cursorX(hoverUs)}
              y1={0}
              y2={SVG_HEIGHT}
              stroke="var(--fg-dim)"
              strokeWidth="1"
              strokeDasharray="3 3"
              vectorEffect="non-scaling-stroke"
            />
          )}
          {visibleEvents.map((e) => {
            const ts = e.occurred_at_us ?? e.sort_ts_us;
            const g = glyphOf(e);
            return (
              <text
                key={e.event_instance_id}
                data-event-kind={e.event_kind}
                x={cursorX(ts)}
                y={GLYPH_Y}
                fontSize="10"
                textAnchor="middle"
                fill={GLYPH_FILL[g.tone]}
              >
                <title>{`${eventKindLabel(t, e.event_kind)} · ${new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "medium" }).format(new Date(ts / 1000))}`}</title>
                {g.glyph}
              </text>
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
