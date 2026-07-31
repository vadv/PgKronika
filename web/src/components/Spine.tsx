import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useTimelineEvents, useTimelineHealth } from "../api/timeline";
import type { EventFact, HealthPointResponse } from "../api/types";
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
const WINDOW_US = 24 * 3600 * 1_000_000;
/** Left gutter in px; aligns the axis with the heatmap rows below. */
export const SPINE_GUTTER_PX = 158;
const SVG_HEIGHT = 60;
const SVG_WIDTH = 1000;
const TICK_COUNT = 24;
const LIVE_REFRESH_MS = 5000;
const KEYBOARD_STEP_US = 300 * 1_000_000;
/** Repeat shift-click within this share of the window clears the baseline. */
const BASELINE_CLEAR_FRACTION = 0.02;

const CRIT_CLASSES = [
  "panic",
  "sigkill",
  "crash",
  "disk_full",
  "out_of_memory",
  "deadlock",
  "termination",
];

function eventSymbol(event: EventFact): string {
  const kind = event.event_kind.toLowerCase();
  if (kind.includes("checkpoint")) return "▲";
  if (kind.includes("error") || kind.includes("deadlock")) return "●";
  if (kind.includes("autovacuum")) return "⚑";
  if (kind.includes("marker") || kind.includes("annotation")) return "◆";
  return "●";
}

function severityColor(event: EventFact): string {
  const cls = event.notable_class.toLowerCase();
  if (CRIT_CLASSES.some((c) => cls.includes(c))) return "var(--sev-crit)";
  if (cls === "info" || cls === "ok") return "var(--sev-ok)";
  return "var(--sev-warn)";
}

function pointY(score: number): number {
  const clamped = Math.min(1, Math.max(0, score));
  return SVG_HEIGHT - 6 - clamped * (SVG_HEIGHT - 12);
}

function healthPolyline(points: HealthPointResponse[], fromUs: number): string {
  return points
    .filter((p) => p.overall_score !== null)
    .map((p) => {
      const mid = (p.interval.from_us + p.interval.to_us) / 2;
      const x = ((mid - fromUs) / WINDOW_US) * SVG_WIDTH;
      // `overall_score` is a 0..1 fraction on the wire (null = no data).
      const y = pointY(p.overall_score ?? 0);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
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
  const fromUs = toUs - WINDOW_US;
  const from = String(fromUs);
  const to = String(toUs);

  const health = useTimelineHealth({ from, to });
  const events = useTimelineEvents({ from, to, limit: 50 });

  const cursorX = (us: number) => ((us - fromUs) / WINDOW_US) * SVG_WIDTH;
  const polyline = healthPolyline(health.data?.points ?? [], fromUs);
  const baselineUs = props.baseline !== null ? Number(props.baseline) : null;
  const visibleEvents = (events.data?.events ?? []).filter((e) => {
    const ts = e.occurred_at_us ?? e.sort_ts_us;
    return ts >= fromUs && ts <= toUs;
  });

  const pickUs = (e: React.MouseEvent<SVGSVGElement>): number => {
    const rect = e.currentTarget.getBoundingClientRect();
    const fraction = (e.clientX - rect.left) / rect.width;
    return Math.round(fromUs + fraction * WINDOW_US);
  };

  const onStripClick = (e: React.MouseEvent<SVGSVGElement>) => {
    const us = pickUs(e);
    if (e.shiftKey) {
      if (
        baselineUs !== null &&
        Math.abs(us - baselineUs) <= BASELINE_CLEAR_FRACTION * WINDOW_US
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
      e.preventDefault();
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
          {polyline !== "" && (
            <polyline
              data-testid="spine-health-line"
              points={polyline}
              fill="none"
              stroke="var(--accent)"
              strokeWidth="1.5"
              vectorEffect="non-scaling-stroke"
            />
          )}
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
                fill={severityColor(e)}
              >
                {eventSymbol(e)}
              </text>
            );
          })}
        </svg>
      </div>
    </section>
  );
}
