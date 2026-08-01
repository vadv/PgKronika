import { Fragment } from "react";
import { useTranslation } from "react-i18next";
import { useHeatmap } from "../api/heatmap";
import type { HeatmapQuality, ViewSpec } from "../api/types";
import { heatColor } from "./heatmapColor";
import { TipRow, Tooltip } from "./Tooltip";

function formatValue(v: number): string {
  return String(Number(v.toFixed(v < 10 ? 2 : 0)));
}

/** Localized breakdown of heatmap quality reasons for the partial chip:
 * a bare "partial data" gives the operator nothing to act on. */
function qualityReasonRows(
  quality: HeatmapQuality,
  t: (key: string, opts?: Record<string, unknown>) => string,
): Array<{ code: string; label: string }> {
  const rows: Array<{ code: string; label: string }> = [];
  const push = (code: string, count: number) => {
    if (count > 0)
      rows.push({ code, label: t(`heatmap.quality.${code}`, { count }) });
  };
  push("gaps", quality.gaps.length);
  push("gated", quality.gated.length);
  push("unavailable_revision", quality.unavailable_revision.length);
  push("resource_limited", quality.resource_limited.length);
  push("unbounded_segments", quality.unbounded_segments.length);
  if (quality.active_tail)
    rows.push({ code: "active_tail", label: t("heatmap.quality.active_tail") });
  return rows;
}

export function HeatmapStrip(props: {
  view: ViewSpec;
  metric: string;
  from: string;
  to: string;
  onMetricChange: (metric: string) => void;
  onSelectEntity: (entity: string) => void;
}) {
  const { t } = useTranslation();
  const heatmap = useHeatmap({
    view: props.view.code,
    metric: props.metric,
    from: props.from,
    to: props.to,
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

  return (
    <section
      style={{
        fontFamily: "var(--mono-font)",
        background: "var(--bg-raised)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-md)",
        padding: "8px 12px 10px",
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
          {metrics.map((m) => (
            <button
              key={m.code}
              onClick={() => props.onMetricChange(m.code)}
              aria-pressed={props.metric === m.code}
              style={{
                fontFamily: "var(--ui-font)",
                fontSize: "var(--text-xs)",
                padding: "2px 8px",
                border: "none",
                background:
                  props.metric === m.code ? "var(--active-bg)" : "transparent",
                color:
                  props.metric === m.code
                    ? "var(--accent-strong)"
                    : "var(--fg-dim)",
                cursor: "pointer",
                transition:
                  "color var(--transition-fast), background var(--transition-fast)",
              }}
            >
              {m.code}
            </button>
          ))}
        </div>
        {heatmap.data &&
          (heatmap.data.quality.gaps.length > 0 ||
            (heatmap.data.quality.status !== "complete" &&
              !heatmap.data.quality.active_tail)) && (
            <Tooltip
              content={
                <span style={{ display: "grid", gap: "2px" }}>
                  {qualityReasonRows(heatmap.data.quality, t).map((row) => (
                    <TipRow key={row.code} label={row.code} value={row.label} />
                  ))}
                </span>
              }
            >
              <span
                style={{
                  fontFamily: "var(--ui-font)",
                  fontSize: "var(--text-xs)",
                  fontWeight: 600,
                  color: "var(--sev-warn-fg)",
                  background: "var(--sev-warn-bg)",
                  borderRadius: "var(--radius-sm)",
                  padding: "1px 8px",
                }}
              >
                {t("heatmap.partial")}
              </span>
            </Tooltip>
          )}
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
          style={{
            display: "grid",
            gridTemplateColumns: `180px repeat(${bucketCount}, 1fr)`,
            marginTop: "8px",
            gap: "2px 0",
          }}
        >
          {rows.map((r) => (
            <Fragment key={r.entity}>
              <Tooltip
                content={
                  <span style={{ display: "grid", gap: "2px" }}>
                    <span style={{ overflowWrap: "anywhere" }}>{r.label}</span>
                    <TipRow
                      label="entity"
                      value={
                        r.entity.length <= 14
                          ? r.entity
                          : `${r.entity.slice(0, 12)}…`
                      }
                      mono
                    />
                  </span>
                }
              >
                <button
                  onClick={() => props.onSelectEntity(r.entity)}
                  style={{
                    fontFamily: "var(--mono-font)",
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
                  {r.label}
                </button>
              </Tooltip>
              {r.values.map((v, i) => {
                const bucketStart =
                  gridFromUs !== null ? gridFromUs + i * bucketWidthUs : null;
                const when =
                  bucketStart !== null
                    ? new Intl.DateTimeFormat(undefined, {
                        hour: "2-digit",
                        minute: "2-digit",
                      }).format(new Date(bucketStart / 1000))
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
                          {r.label}
                        </span>
                        <TipRow
                          label={props.metric}
                          value={v === null ? "—" : formatValue(v)}
                          mono
                        />
                        {when !== null && (
                          <TipRow label="bucket" value={when} mono />
                        )}
                      </span>
                    }
                  >
                    <div
                      data-cell
                      data-empty={v === null ? "true" : undefined}
                      style={{
                        width: "11px",
                        height: "13px",
                        margin: "0.5px",
                        borderRadius: "2px",
                        background: heatColor(
                          v === null ? null : max > 0 ? v / max : 0,
                        ),
                      }}
                    />
                  </Tooltip>
                );
              })}
            </Fragment>
          ))}
        </div>
      )}
    </section>
  );
}
