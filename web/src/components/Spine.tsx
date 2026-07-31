import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useTimelineSpine } from "../api/spine";
import { useTimelineEvents } from "../api/timeline";
import type { EventFact, SpineSeries } from "../api/types";
import { SPANS } from "../state/url";

export interface SpineProps {
  /** Cursor timestamp (int64 µs, decimal string); null = LIVE. */
  at: string | null;
  /** Window length in seconds (900 / 3600 / 21600 / 86400). */
  span: number;
  /** Baseline cursor (int64 µs string); null = no baseline. */
  baseline: string | null;
  onSelectAt: (at: string | null) => void;
  onSelectSpan: (span: number) => void;
  onSelectBaseline: (baseline: string | null) => void;
}

/** The spine always shows the trailing 24 h of recording. */
/** Left gutter in px; aligns the axis with the heatmap rows below. */
export const SPINE_GUTTER_PX = 158;
const SVG_HEIGHT = 60;
const SVG_WIDTH = 1000;
const TICK_COUNT = 24;
const LIVE_REFRESH_MS = 5000;
const KEYBOARD_STEP_US = 300 * 1_000_000;
/** Grid resolution requested from `/v1/timeline/spine`. */
const SPINE_BUCKETS = 96;
/** Repeat shift-click within this share of the window clears the baseline. */
const BASELINE_CLEAR_FRACTION = 0.02;

function eventSymbol(event: EventFact): string {
  const kind = event.event_kind.toLowerCase();
  if (kind.includes("checkpoint")) return "▲";
  if (kind.includes("error") || kind.includes("deadlock")) return "●";
  if (kind.includes("autovacuum")) return "⚑";
  if (kind.includes("marker") || kind.includes("annotation")) return "◆";
  return "●";
}

function pointY(fraction: number): number {
  const clamped = Math.min(1, Math.max(0, fraction));
  return SVG_HEIGHT - 6 - clamped * (SVG_HEIGHT - 12);
}

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

/** Contiguous non-null runs of the series, as SVG `points` strings. */
function seriesSegments(
  series: SpineSeries,
  geom: BucketGeometry,
  fromUs: number,
  windowUs: number,
): string[] {
  const n = series.values.length;
  if (n === 0) return [];
  // Values arrive in the series' own unit, so scale by the observed max
  // instead of assuming a 0..1 fraction.
  const max = series.values.reduce<number>(
    (m, v) => (v !== null && v > m ? v : m),
    0,
  );
  const scale = max > 0 ? max : 1;
  const segments: string[] = [];
  let current: string[] = [];
  series.values.forEach((v, i) => {
    if (v === null) {
      if (current.length > 0) segments.push(current.join(" "));
      current = [];
      return;
    }
    const x = bucketX(geom, i, fromUs, windowUs);
    const y = pointY(v / scale);
    current.push(`${x.toFixed(1)},${y.toFixed(1)}`);
  });
  if (current.length > 0) segments.push(current.join(" "));
  return segments;
}

export function Spine(props: SpineProps) {
  const { t } = useTranslation();
  const live = props.at === null;
  const [nowUs, setNowUs] = useState(() => Date.now() * 1000);

  useEffect(() => {
    if (!live) return;
    const id = setInterval(() => setNowUs(Date.now() * 1000), LIVE_REFRESH_MS);
    return () => clearInterval(id);
  }, [live]);

  const toUs = props.at !== null ? Number(props.at) : nowUs;
  // Wire range follows the zoom span (exact BigInt math); the 24 h bound is
  // enforced by the span whitelist in the URL codec.
  const windowUs = BigInt(props.span) * 1_000_000n;
  const toBig = props.at !== null ? BigInt(props.at) : BigInt(nowUs);
  const from = (toBig - windowUs).toString();
  const to = toBig.toString();
  const windowNum = props.span * 1_000_000;
  const fromUs = toUs - windowNum;

  const spine = useTimelineSpine({ from, to, buckets: SPINE_BUCKETS });
  const events = useTimelineEvents({ from, to, limit: 50 });

  const cursorX = (us: number) => ((us - fromUs) / windowNum) * SVG_WIDTH;
  const primary = spine.data ? pickPrimarySeries(spine.data.series) : null;
  const geom: BucketGeometry | null = spine.data
    ? {
        gridFromUs: Number(spine.data.grid.from_us),
        bucketSpanUs:
          (Number(spine.data.grid.to_us) - Number(spine.data.grid.from_us)) /
          Math.max(1, spine.data.grid.bucket_count),
      }
    : null;
  const segments =
    primary !== null && geom !== null
      ? seriesSegments(primary, geom, fromUs, windowNum)
      : [];
  const baselineUs = props.baseline !== null ? Number(props.baseline) : null;
  const visibleEvents = (events.data?.events ?? []).filter((e) => {
    const ts = e.occurred_at_us ?? e.sort_ts_us;
    return ts >= fromUs && ts <= toUs;
  });

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

  const cursorDate = new Date(toUs / 1000);
  const cursorLabel = new Intl.DateTimeFormat(undefined, {
    dateStyle: "short",
    timeStyle: "medium",
  }).format(cursorDate);

  return (
    <section
      aria-label={t("spine.caption")}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "4px",
        padding: "4px 8px",
        background: "var(--bg-raised)",
        borderBottom: "1px solid var(--border)",
        fontFamily: "var(--ui-font)",
      }}
    >
      <div style={{ display: "flex", alignItems: "baseline", gap: "8px" }}>
        <span style={{ color: "var(--fg-dim)" }}>{t("spine.caption")}</span>
        <button
          type="button"
          aria-pressed={!live}
          onClick={() =>
            props.onSelectAt(live ? String(Date.now() * 1000) : null)
          }
          style={{
            fontFamily: "var(--mono-font)",
            color: live ? "var(--sev-ok)" : "var(--sev-warn)",
            background: "none",
            border: `1px solid ${live ? "var(--sev-ok)" : "var(--sev-warn)"}`,
            cursor: "pointer",
          }}
        >
          {live ? t("spine.live") : t("spine.replay")}
        </button>
        <div role="group" aria-label={t("spine.zoom")}>
          {SPANS.map((s) => (
            <button
              key={s}
              type="button"
              aria-pressed={props.span === s}
              onClick={() => props.onSelectSpan(s)}
              style={{
                fontFamily: "var(--mono-font)",
                color: props.span === s ? "var(--accent)" : "var(--fg)",
                background: "none",
                border: "none",
                borderBottom:
                  props.span === s
                    ? "2px solid var(--accent)"
                    : "2px solid transparent",
                cursor: "pointer",
              }}
            >
              {t(`spine.span.${s}`)}
            </button>
          ))}
        </div>
        <span
          data-testid="spine-cursor-time"
          style={{ marginInlineStart: "auto", color: "var(--fg-dim)" }}
        >
          {cursorLabel}
        </span>
        {primary !== null && (
          <span
            data-testid="spine-metric"
            style={{ fontFamily: "var(--mono-font)", color: "var(--fg-dim)" }}
          >
            {t("spine.load", { code: primary.code, unit: primary.unit })}
          </span>
        )}
      </div>
      <div style={{ display: "flex", alignItems: "stretch" }}>
        <div
          data-testid="spine-gutter"
          style={{ width: `${SPINE_GUTTER_PX}px`, flex: "none" }}
        />
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
          style={{
            flex: 1,
            height: `${SVG_HEIGHT}px`,
            display: "block",
            background: "var(--bg)",
            cursor: "crosshair",
          }}
        >
          {Array.from({ length: TICK_COUNT + 1 }, (_, i) => (
            <line
              key={i}
              data-tick
              x1={(i / TICK_COUNT) * SVG_WIDTH}
              x2={(i / TICK_COUNT) * SVG_WIDTH}
              y1={SVG_HEIGHT - 6}
              y2={SVG_HEIGHT}
              stroke="var(--border)"
              strokeWidth="1"
            />
          ))}
          {segments.map((points, i) => (
            <polyline
              key={i}
              data-testid="spine-health-line"
              points={points}
              fill="none"
              stroke="var(--accent)"
              strokeWidth="1.5"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {primary !== null &&
            geom !== null &&
            primary.values.map((v, i) => {
              if (v !== null) return null;
              // Honest missing: the bucket has no sample, the polyline breaks
              // here; the dot carries the wire status for hover.
              const status = primary.value_statuses[i];
              return (
                <circle
                  key={i}
                  data-testid="spine-missing-point"
                  cx={bucketX(geom, i, fromUs, windowNum)}
                  cy={SVG_HEIGHT - 8}
                  r={2}
                  fill="var(--sev-warn)"
                >
                  <title>
                    {status
                      ? `${t("spine.missing")}: ${status.reason ?? status.status}`
                      : t("spine.missing")}
                  </title>
                </circle>
              );
            })}
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
          {visibleEvents.map((e) => {
            const ts = e.occurred_at_us ?? e.sort_ts_us;
            return (
              <text
                key={e.event_instance_id}
                data-event-kind={e.event_kind}
                x={cursorX(ts)}
                y={SVG_HEIGHT - 10}
                fontSize="10"
                textAnchor="middle"
                fill="var(--accent)"
              >
                {/* Event markers are neutral: the API carries no event
                    severity; verdicts belong to incidents. */}
                <title>{`${e.event_kind} · ${new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "medium" }).format(new Date(ts / 1000))}`}</title>
                {eventSymbol(e)}
              </text>
            );
          })}
        </svg>
      </div>
    </section>
  );
}
