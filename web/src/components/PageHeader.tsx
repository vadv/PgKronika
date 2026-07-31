import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { sectionTitle, verdictTint } from "../design/ui";
import { TipRow, Tooltip } from "./Tooltip";

function KpiStat(props: {
  label: string;
  value: string;
  hint?: string;
  tone?: "default" | "warning" | "critical";
}) {
  const tint =
    props.tone === "critical"
      ? verdictTint("critical")
      : props.tone === "warning"
        ? verdictTint("warning")
        : undefined;
  return (
    <span
      title={props.hint}
      style={{
        display: "inline-flex",
        alignItems: "baseline",
        gap: "5px",
        padding: "1px 8px",
        background: tint !== undefined ? tint.background : "var(--bg-raised)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-sm)",
        whiteSpace: "nowrap",
      }}
    >
      <span
        style={{
          fontFamily: "var(--ui-font)",
          fontSize: "var(--text-xs)",
          color: "var(--fg-dim)",
        }}
      >
        {props.label}
      </span>
      <span
        style={{
          fontFamily: "var(--mono-font)",
          fontSize: "var(--text-md)",
          fontWeight: 600,
          color: tint !== undefined ? tint.color : "var(--fg-strong)",
        }}
      >
        {props.value}
      </span>
    </span>
  );
}

export function PageHeader(props: {
  view: ViewSpec;
  summary: ViewSummaryItem | undefined;
  matched: number | null;
  onOpenIncidents?: () => void;
}) {
  const { t } = useTranslation();
  const s = props.summary;
  const snapshotLabel =
    s?.snapshot_ts_us != null
      ? new Intl.DateTimeFormat(undefined, {
          dateStyle: "short",
          timeStyle: "medium",
        }).format(new Date(Number(s.snapshot_ts_us) / 1000))
      : null;
  const collection = s?.collection;
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: "var(--space-3)",
        flexWrap: "wrap",
        padding: "2px 2px 0",
      }}
    >
      <div style={{ display: "flex", flexDirection: "column", gap: "1px" }}>
        <span
          style={{
            fontFamily: "var(--ui-font)",
            fontSize: "var(--text-lg)",
            fontWeight: 700,
            color: "var(--fg-strong)",
          }}
        >
          {t(`tabs.${props.view.code}`)}
        </span>
        <span
          style={{
            fontFamily: "var(--ui-font)",
            fontSize: "var(--text-xs)",
            color: "var(--fg-dim)",
          }}
        >
          {snapshotLabel !== null
            ? t("pageheader.snapshot", { at: snapshotLabel })
            : t("pageheader.noSnapshot")}
        </span>
      </div>
      <div
        style={{
          display: "flex",
          gap: "6px",
          flexWrap: "wrap",
          alignItems: "center",
        }}
      >
        <KpiStat
          label={t("pageheader.population")}
          value={s?.population != null ? String(s.population) : "—"}
          hint={t("pageheader.populationHint")}
        />
        {s?.notable === true && (
          <button
            type="button"
            onClick={props.onOpenIncidents}
            title={t("pageheader.notableHint")}
            style={{
              background: "none",
              border: "none",
              padding: 0,
              cursor: "pointer",
            }}
          >
            <KpiStat
              label={t("pageheader.notable")}
              value={`${s.notable_level} ×${s.notable_count}`}
              tone={s.notable_level === "critical" ? "critical" : "warning"}
            />
          </button>
        )}
        {collection != null && (
          <KpiStat
            label={t("pageheader.collection")}
            value={`${collection.collected}/${collection.source_total ?? "?"}`}
            hint={t("pageheader.collectionHint")}
          />
        )}
        {props.matched !== null && (
          <KpiStat
            label={t("pageheader.matched")}
            value={String(props.matched)}
            hint={t("pageheader.matchedHint")}
          />
        )}
      </div>
      <span style={{ flex: 1 }} />
      <Tooltip
        content={
          <span style={{ display: "grid", gap: "2px" }}>
            <TipRow label="view" value={props.view.code} mono />
            <TipRow
              label="canonical metric"
              value={props.view.canonical_metric}
              mono
            />
            <TipRow label="availability" value={props.view.availability} />
          </span>
        }
      >
        <span style={{ ...sectionTitle, cursor: "default" }}>
          {props.view.code}
        </span>
      </Tooltip>
    </div>
  );
}
