import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { metricLabel, statusLabel } from "../api/codes";
import { useTimelineSpine } from "../api/spine";
import { useTimelineEvents } from "../api/timeline";
import type { EventFact, SpineSeries } from "../api/types";
import { formatByUnit, formatTimestampUs } from "../design/format";
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
const SVG_HEIGHT = 28;
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
  return SVG_HEIGHT - 4 - clamped * (SVG_HEIGHT - 8);
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

/** Contiguous non-null runs of the series. Runs of 2+ points become
 * polylines; a lone point becomes a dot — never a dangling diagonal
 * fragment floating over empty space. */
function seriesSegments(
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
  const lines: string[] = [];
  const dots: Array<[number, number]> = [];
  let current: string[] = [];
  const flush = () => {
    if (current.length >= 2) lines.push(current.join(" "));
    else if (current.length === 1) {
      const first = current[0];
      if (first !== undefined) {
        const [x, y] = first.split(",").map(Number);
        if (x !== undefined && y !== undefined) dots.push([x, y]);
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
    const x = bucketX(geom, i, fromUs, windowUs);
    const y = pointY(v / scale);
    current.push(`${x.toFixed(1)},${y.toFixed(1)}`);
  }
  flush();
  return { lines, dots };
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

  const [hoverUs, setHoverUs] = useState<number | null>(null);
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
      ? seriesSegments(primary, geom, fromUs, windowNum)
      : { lines: [], dots: [] };
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

  const cursorLabel = formatTimestampUs(toUs);

  const bucketCount = spine.data?.grid.bucket_count ?? 0;

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
        onClick={() =>
          props.onSelectAt(live ? String(Date.now() * 1000) : null)
        }
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
        {Array.from({ length: TICK_COUNT + 1 }, (_, i) => (
          <line
            key={i}
            data-tick
            x1={(i / TICK_COUNT) * SVG_WIDTH}
            x2={(i / TICK_COUNT) * SVG_WIDTH}
            y1={SVG_HEIGHT - 4}
            y2={SVG_HEIGHT}
            stroke="var(--border)"
            strokeWidth="1"
          />
        ))}
        {lines.map((points, i) => (
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
        {primary !== null &&
          geom !== null &&
          primary.values.map((v, i) => {
            if (v !== null) return null;
            // Honest missing: the bucket has no sample, the polyline breaks
            // here; the dot at the baseline marks the hole.
            return (
              <circle
                key={i}
                data-testid="spine-missing-point"
                cx={bucketX(geom, i, fromUs, windowNum)}
                cy={SVG_HEIGHT - 2}
                r={1.5}
                fill="var(--sev-warn)"
              />
            );
          })}
        {baselineUs !== null && baselineUs >= fromUs && baselineUs <= toUs && (
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
          return (
            <text
              key={e.event_instance_id}
              data-event-kind={e.event_kind}
              x={cursorX(ts)}
              y={SVG_HEIGHT - 6}
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
        {/* Bucket overlay: every grid cell owns its slice of the strip and
            carries the honest tooltip (values of all series, or the exact
            no-data reason) — no misaligned hover math. */}
        {geom !== null &&
          Array.from({ length: bucketCount }, (_, i) => {
            const start = geom.gridFromUs + i * geom.bucketSpanUs;
            const when = new Intl.DateTimeFormat(undefined, {
              hour: "2-digit",
              minute: "2-digit",
            }).format(new Date(start / 1000));
            const parts = allSeries.map((s) => {
              const v = s.values[i];
              const label = metricLabel(t, "os", s.code);
              if (v === null || v === undefined) {
                const status = s.value_statuses[i];
                const reason =
                  status !== undefined
                    ? statusLabel(t, status.reason ?? status.status)
                    : t("spine.missing");
                return `${label}: ${t("spine.missing")} (${reason})`;
              }
              return `${label}: ${formatByUnit(v, s.unit)}`;
            });
            return (
              <rect
                key={i}
                data-testid="spine-bucket"
                x={(i / bucketCount) * SVG_WIDTH}
                y={0}
                width={SVG_WIDTH / bucketCount}
                height={SVG_HEIGHT}
                fill="transparent"
              >
                <title>{`${when} · ${parts.join(" · ")}`}</title>
              </rect>
            );
          })}
      </svg>
      <span
        data-testid="spine-cursor-time"
        style={{
          color: "var(--fg-dim)",
          fontFamily: "var(--mono-font)",
          fontSize: "var(--text-sm)",
        }}
      >
        {cursorLabel}
      </span>
      {primary !== null && (
        <span
          data-testid="spine-metric"
          style={{
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-sm)",
            color: "var(--fg-dim)",
          }}
        >
          {t("spine.load", {
            code: metricLabel(t, "os", primary.code),
            unit: primary.unit,
          })}
        </span>
      )}
    </section>
  );
}
