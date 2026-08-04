import { Fragment, useState } from "react";
import { useTranslation } from "react-i18next";
import { metricDesc, metricLabel } from "../api/codes";
import { useHeatmap } from "../api/heatmap";
import type { HeatmapRow, ViewSpec } from "../api/types";
import { breakableCode, formatByUnit, shortIdToken } from "../design/format";
import { heatColor } from "./heatmapColor";
import { HEATMAP_LABEL_COL_PX } from "../design/ui";
import type { TimeRange } from "../state/timeGeometry";
import { TipFormula, TipRow, Tooltip } from "./Tooltip";

/** Fraction of the global max below which a cell reads as "normal" — the
 * same cut the color scale makes between heat-1 and heat-2. */
const NORMAL_FRACTION = 0.4;

function timePercent(
  value: string,
  from: string,
  to: string,
  clip: boolean,
): number | null {
  try {
    const start = BigInt(from);
    const end = BigInt(to);
    if (end <= start) return null;
    const point = BigInt(value);
    if (!clip && (point < start || point > end)) return null;
    const scaled = Number(((point - start) * 100_000n) / (end - start)) / 1000;
    return Math.min(100, Math.max(0, scaled));
  } catch {
    return null;
  }
}

/** Heatmap row labels for token views are raw uint64 decimals — shorten to
 * the same hex token the table uses; the full value stays in the tooltip. */
function displayLabel(view: string, label: string): string {
  if (
    (view === "statements" || view === "plans") &&
    /^[0-9]+$/.test(label) &&
    label.length > 10
  ) {
    return shortIdToken(label);
  }
  return label;
}

function bucketTime(
  gridFromUs: number,
  bucketWidthUs: number,
  index: number,
): string {
  const start = new Date((gridFromUs + index * bucketWidthUs) / 1000);
  const end = new Date((gridFromUs + (index + 1) * bucketWidthUs) / 1000);
  const fmt = new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
  return `${fmt.format(start)}–${fmt.format(end)}`;
}

function HeatmapRowView(props: {
  row: HeatmapRow;
  view: string;
  metricText: string;
  gridFromUs: number | null;
  bucketWidthUs: number;
  max: number;
  presentation: "heatmap" | "events";
  onSelectEntity: (entity: string) => void;
}) {
  const { t } = useTranslation();
  const r = props.row;
  const label = displayLabel(props.view, r.label);
  return (
    <Fragment>
      <Tooltip content={<span>{breakableCode(r.label)}</span>}>
        <button
          onClick={() => props.onSelectEntity(r.entity)}
          style={{
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-sm)",
            lineHeight: "14px",
            color: "var(--fg)",
            background: "none",
            border: "none",
            padding: 0,
            textAlign: "start",
            cursor: "pointer",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {label}
        </button>
      </Tooltip>
      {r.values.map((v, i) => {
        const ratio = v === null || props.max <= 0 ? 0 : v / props.max;
        const eventValue =
          v === null
            ? "empty"
            : v === 0
              ? "zero"
              : ratio < NORMAL_FRACTION
                ? "quiet"
                : "peak";
        const eventLane = props.presentation === "events";
        const when =
          props.gridFromUs !== null
            ? bucketTime(props.gridFromUs, props.bucketWidthUs, i)
            : null;
        return (
          <Tooltip
            key={i}
            preferAbove
            content={
              <span style={{ display: "grid", gap: "2px" }}>
                <span
                  style={{
                    overflowWrap: "anywhere",
                    color: "var(--fg-strong)",
                  }}
                >
                  {breakableCode(r.label)}
                </span>
                <TipRow
                  label={props.metricText}
                  value={
                    v === null
                      ? t("data.noSnapshotInterval")
                      : formatByUnit(v, r.unit)
                  }
                  mono
                />
                {when !== null && (
                  <TipRow label={t("tooltip.bucket")} value={when} mono />
                )}
              </span>
            }
          >
            <div
              data-cell
              data-empty={v === null ? "true" : undefined}
              data-value={eventLane ? eventValue : undefined}
              style={{
                width: "10px",
                height: eventLane
                  ? eventValue === "peak"
                    ? "8px"
                    : "2px"
                  : "10px",
                margin: eventLane
                  ? eventValue === "peak"
                    ? "2px 0.5px 4px"
                    : "6px 0.5px"
                  : "2px 0.5px",
                borderRadius: eventLane ? "1px" : "2px",
                background:
                  eventLane && eventValue === "quiet"
                    ? "var(--fg-dim)"
                    : heatColor(v === null ? null : props.max > 0 ? ratio : 0),
                opacity:
                  eventLane && (eventValue === "zero" || eventValue === "empty")
                    ? 0
                    : eventLane && eventValue === "quiet"
                      ? 0.35
                      : 1,
              }}
            />
          </Tooltip>
        );
      })}
    </Fragment>
  );
}

export function HeatmapStrip(props: {
  view: ViewSpec;
  metric: string;
  from: string;
  to: string;
  selectedRange?: TimeRange;
  cursorUs?: string | null;
  hoverUs?: string | null;
  brushDraft?: TimeRange | null;
  baselineUs?: string | null;
  buckets?: number;
  presentation?: "heatmap" | "events";
  onMetricChange: (metric: string) => void;
  onSelectEntity: (entity: string) => void;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const heatmap = useHeatmap({
    view: props.view.code,
    metric: props.metric,
    from: props.from,
    to: props.to,
    buckets: props.buckets,
  });

  const metrics = props.view.metrics.filter(
    (m) => m.availability === "available",
  );
  const rows = heatmap.data?.rows ?? [];
  const bucketCount = heatmap.data?.grid.bucket_count ?? 0;
  const max = Math.max(
    0,
    ...rows.flatMap((r) => r.values.filter((v): v is number => v !== null)),
  );
  const gridFromUs = heatmap.data ? Number(heatmap.data.grid.from_us) : null;
  const bucketWidthUs =
    heatmap.data && heatmap.data.grid.bucket_count > 0
      ? (Number(heatmap.data.grid.to_us) - Number(heatmap.data.grid.from_us)) /
        heatmap.data.grid.bucket_count
      : 0;
  const overlayFrom = heatmap.data?.grid.from_us ?? props.from;
  const overlayTo = heatmap.data?.grid.to_us ?? props.to;
  const cursorPercent =
    props.cursorUs == null
      ? null
      : timePercent(props.cursorUs, overlayFrom, overlayTo, false);
  const hoverPercent =
    props.hoverUs == null
      ? null
      : timePercent(props.hoverUs, overlayFrom, overlayTo, false);
  const baselinePercent =
    props.baselineUs == null
      ? null
      : timePercent(props.baselineUs, overlayFrom, overlayTo, false);
  const selectedStart = props.selectedRange
    ? timePercent(props.selectedRange.fromUs, overlayFrom, overlayTo, true)
    : null;
  const selectedEnd = props.selectedRange
    ? timePercent(props.selectedRange.toUs, overlayFrom, overlayTo, true)
    : null;
  const brushStart = props.brushDraft
    ? timePercent(props.brushDraft.fromUs, overlayFrom, overlayTo, true)
    : null;
  const brushEnd = props.brushDraft
    ? timePercent(props.brushDraft.toUs, overlayFrom, overlayTo, true)
    : null;

  // Density: quiet rows collapse into one-line summaries; loud rows keep
  // their full grid. Empty (all-null) rows collapse separately — absence of
  // data is not "normal".
  const emptyRows = rows.filter((r) => r.values.every((v) => v === null));
  const normalRows = rows.filter(
    (r) =>
      !emptyRows.includes(r) &&
      r.values.every(
        (v) => v === null || (max > 0 ? v / max : 0) < NORMAL_FRACTION,
      ),
  );
  const visibleRows = rows.filter(
    (r) => !emptyRows.includes(r) && !normalRows.includes(r),
  );
  const shownRows = expanded ? rows : visibleRows;
  const collapsedCount = rows.length - visibleRows.length;

  const metricText = metricLabel(t, props.view.code, props.metric);

  return (
    <section
      data-heatmap-presentation={props.presentation ?? "heatmap"}
      style={{
        fontFamily: "var(--mono-font)",
        background: "var(--bg-raised)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-md)",
        boxShadow: "var(--shadow-raised)",
        padding: "6px 12px",
      }}
    >
      <div
        style={{
          display: "flex",
          gap: "8px",
          alignItems: "center",
          flexWrap: "wrap",
        }}
      >
        <span
          style={{
            fontFamily: "var(--ui-font)",
            fontSize: "var(--text-xs)",
            fontWeight: 600,
            textTransform: "uppercase",
            letterSpacing: "var(--tracking-caps)",
            color: "var(--fg-dim)",
          }}
        >
          {t("heatmap.metric")}
        </span>
        <div
          role="group"
          aria-label={t("heatmap.metric")}
          style={{
            display: "inline-flex",
            alignItems: "center",
            background: "var(--bg)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-sm)",
            overflow: "hidden",
          }}
        >
          {metrics.map((m) => {
            const label = metricLabel(t, props.view.code, m.code);
            const desc = metricDesc(t, props.view.code, m.code);
            return (
              <Tooltip
                key={m.code}
                content={
                  <span style={{ display: "grid", gap: "2px" }}>
                    {desc !== null && <span>{desc}</span>}
                    <TipRow
                      label={t("tooltip.code")}
                      value={`${m.code} · ${m.unit}`}
                      mono
                    />
                    <TipFormula
                      label={t("tooltip.formula")}
                      value={m.formula}
                    />
                  </span>
                }
              >
                <button
                  onClick={() => props.onMetricChange(m.code)}
                  aria-pressed={props.metric === m.code}
                  style={{
                    fontFamily: "var(--ui-font)",
                    fontSize: "var(--text-xs)",
                    padding: "2px 8px",
                    border: "none",
                    background:
                      props.metric === m.code
                        ? "var(--active-bg)"
                        : "transparent",
                    color:
                      props.metric === m.code
                        ? "var(--accent-strong)"
                        : "var(--fg-dim)",
                    cursor: "pointer",
                    transition:
                      "color var(--transition-fast), background var(--transition-fast)",
                  }}
                >
                  {label}
                </button>
              </Tooltip>
            );
          })}
        </div>
        {/* Verdict legend: the colors mean thresholds, not decoration. */}
        <span
          style={{
            marginInlineStart: "auto",
            display: "inline-flex",
            alignItems: "center",
            gap: "4px",
            fontFamily: "var(--ui-font)",
            fontSize: "var(--text-xs)",
            color: "var(--fg-dim)",
          }}
        >
          <span
            style={{
              width: 10,
              height: 10,
              background: "var(--heat-1)",
              borderRadius: 2,
            }}
          />
          {t("heatmap.legend.normal")}
          <span
            style={{
              width: 10,
              height: 10,
              background: "var(--heat-2)",
              borderRadius: 2,
              marginInlineStart: 6,
            }}
          />
          {t("heatmap.legend.warning")}
          <span
            style={{
              width: 10,
              height: 10,
              background: "var(--heat-3)",
              borderRadius: 2,
              marginInlineStart: 6,
            }}
          />
          {t("heatmap.legend.critical")}
        </span>
      </div>
      {heatmap.isSuccess && rows.length === 0 && (
        <div style={{ color: "var(--fg-dim)" }}>{t("heatmap.empty")}</div>
      )}
      {rows.length > 0 && (
        <div
          data-testid="heatmap-time-grid"
          style={{
            position: "relative",
            display: "grid",
            gridTemplateColumns: `${HEATMAP_LABEL_COL_PX}px repeat(${bucketCount}, 1fr)`,
            marginTop: "4px",
            gap: "1px 0",
          }}
        >
          {shownRows.map((r) => (
            <HeatmapRowView
              key={r.entity}
              row={r}
              view={props.view.code}
              metricText={metricText}
              gridFromUs={gridFromUs}
              bucketWidthUs={bucketWidthUs}
              max={max}
              presentation={props.presentation ?? "heatmap"}
              onSelectEntity={props.onSelectEntity}
            />
          ))}
          {!expanded && collapsedCount > 0 && (
            <button
              type="button"
              title={[...normalRows, ...emptyRows]
                .map((r) => displayLabel(props.view.code, r.label))
                .slice(0, 20)
                .join(", ")}
              onClick={() => setExpanded(true)}
              style={{
                gridColumn: "1 / -1",
                fontFamily: "var(--ui-font)",
                fontSize: "var(--text-xs)",
                lineHeight: "14px",
                textAlign: "start",
                color: "var(--fg-dim)",
                background: "none",
                border: "none",
                padding: 0,
                cursor: "pointer",
              }}
            >
              {[
                normalRows.length > 0
                  ? t("heatmap.collapsedNormal", { count: normalRows.length })
                  : null,
                emptyRows.length > 0
                  ? t("heatmap.collapsedEmpty", { count: emptyRows.length })
                  : null,
              ]
                .filter((part) => part !== null)
                .join(" · ")}
            </button>
          )}
          {expanded && collapsedCount > 0 && (
            <button
              type="button"
              onClick={() => setExpanded(false)}
              style={{
                gridColumn: "1 / -1",
                fontFamily: "var(--ui-font)",
                fontSize: "var(--text-xs)",
                lineHeight: "14px",
                textAlign: "start",
                color: "var(--fg-dim)",
                background: "none",
                border: "none",
                padding: 0,
                cursor: "pointer",
              }}
            >
              {t("heatmap.collapse")}
            </button>
          )}
          <div
            data-testid="heatmap-time-overlay"
            aria-hidden="true"
            style={{
              position: "absolute",
              insetBlock: 0,
              insetInlineStart: `${HEATMAP_LABEL_COL_PX}px`,
              insetInlineEnd: 0,
              pointerEvents: "none",
              overflow: "hidden",
            }}
          >
            {selectedStart !== null && selectedEnd !== null && (
              <span
                data-testid="heatmap-selected-range"
                style={{
                  position: "absolute",
                  insetBlock: 0,
                  left: `${Math.min(selectedStart, selectedEnd)}%`,
                  width: `${Math.abs(selectedEnd - selectedStart)}%`,
                  background: "var(--active-bg)",
                  opacity: 0.12,
                }}
              />
            )}
            {brushStart !== null && brushEnd !== null && (
              <span
                data-testid="heatmap-brush-draft"
                style={{
                  position: "absolute",
                  insetBlock: 0,
                  left: `${Math.min(brushStart, brushEnd)}%`,
                  width: `${Math.abs(brushEnd - brushStart)}%`,
                  background: "var(--accent)",
                  opacity: 0.18,
                }}
              />
            )}
            {baselinePercent !== null && (
              <span
                data-testid="heatmap-baseline"
                style={{
                  position: "absolute",
                  insetBlock: 0,
                  left: `${baselinePercent}%`,
                  borderInlineStart: "1px dashed var(--accent)",
                }}
              />
            )}
            {cursorPercent !== null && (
              <span
                data-testid="heatmap-cursor"
                style={{
                  position: "absolute",
                  insetBlock: 0,
                  left: `${cursorPercent}%`,
                  borderInlineStart: "1px solid var(--fg)",
                }}
              />
            )}
            {hoverPercent !== null && (
              <span
                data-testid="heatmap-hover-cursor"
                style={{
                  position: "absolute",
                  insetBlock: 0,
                  left: `${hoverPercent}%`,
                  borderInlineStart: "1px dashed var(--fg-dim)",
                }}
              />
            )}
          </div>
        </div>
      )}
    </section>
  );
}
