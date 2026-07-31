import { Fragment } from "react";
import { useTranslation } from "react-i18next";
import { useHeatmap } from "../api/heatmap";
import type { HeatmapQuality, ViewSpec } from "../api/types";
import { heatColor } from "./heatmapColor";

function formatValue(v: number): string {
  return String(Number(v.toFixed(v < 10 ? 2 : 0)));
}

/** Localized breakdown of heatmap quality reasons for the partial chip:
 * a bare "partial data" gives the operator nothing to act on. */
function qualityReasons(
  quality: HeatmapQuality,
  t: (key: string, opts?: Record<string, unknown>) => string,
): string {
  const parts: string[] = [];
  const push = (code: string, count: number) => {
    if (count > 0) parts.push(t(`heatmap.quality.${code}`, { count }));
  };
  push("gaps", quality.gaps.length);
  push("gated", quality.gated.length);
  push("unavailable_revision", quality.unavailable_revision.length);
  push("resource_limited", quality.resource_limited.length);
  push("unbounded_segments", quality.unbounded_segments.length);
  if (quality.active_tail) parts.push(t("heatmap.quality.active_tail"));
  return parts.join("\n");
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

  return (
    <section style={{ fontFamily: "var(--mono-font)", padding: "4px 8px" }}>
      <div
        style={{
          display: "flex",
          gap: "var(--gap, 4px)",
          alignItems: "baseline",
        }}
      >
        <span style={{ color: "var(--fg-dim)" }}>{t("heatmap.metric")}</span>
        {metrics.map((m) => (
          <button
            key={m.code}
            onClick={() => props.onMetricChange(m.code)}
            style={{
              fontFamily: "var(--mono-font)",
              color: props.metric === m.code ? "var(--accent)" : "var(--fg)",
              background: "none",
              border: "none",
              borderBottom:
                props.metric === m.code
                  ? "2px solid var(--accent)"
                  : "2px solid transparent",
              cursor: "pointer",
            }}
          >
            {m.code}
          </button>
        ))}
        {heatmap.data &&
          (heatmap.data.quality.gaps.length > 0 ||
            (heatmap.data.quality.status !== "complete" &&
              !heatmap.data.quality.active_tail)) && (
            <span
              title={qualityReasons(heatmap.data.quality, t)}
              style={{ color: "var(--sev-warn)", whiteSpace: "pre-line" }}
            >
              {t("heatmap.partial")}
            </span>
          )}
      </div>
      {heatmap.isSuccess && rows.length === 0 && (
        <div style={{ color: "var(--fg-dim)" }}>{t("heatmap.empty")}</div>
      )}
      {rows.length > 0 && (
        <div
          style={{
            display: "grid",
            gridTemplateColumns: `160px repeat(${bucketCount}, 1fr)`,
            marginTop: "4px",
          }}
        >
          {rows.map((r) => (
            <Fragment key={r.entity}>
              <button
                onClick={() => props.onSelectEntity(r.entity)}
                title={r.entity}
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
              {r.values.map((v, i) => (
                <div
                  key={i}
                  data-cell
                  data-empty={v === null ? "true" : undefined}
                  title={`${r.label}: ${v === null ? "—" : formatValue(v)}`}
                  style={{
                    width: "12px",
                    height: "14px",
                    background: heatColor(
                      v === null ? null : max > 0 ? v / max : 0,
                    ),
                  }}
                />
              ))}
            </Fragment>
          ))}
        </div>
      )}
    </section>
  );
}
