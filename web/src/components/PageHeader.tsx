import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { sectionTitle, verdictTint } from "../design/ui";
import { TipRow, Tooltip } from "./Tooltip";

function KpiCard(props: {
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
    <div
      title={props.hint}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "2px",
        minWidth: "110px",
        padding: "6px 10px",
        background: "var(--bg-raised)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-md)",
        ...(tint ?? {}),
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
        {props.label}
      </span>
      <span
        style={{
          fontFamily: "var(--mono-font)",
          fontSize: "var(--text-lg)",
          fontWeight: 600,
          color: tint !== undefined ? tint.color : "var(--fg-strong)",
        }}
      >
        {props.value}
      </span>
    </div>
  );
}

export function PageHeader(props: {
  view: ViewSpec;
  summary: ViewSummaryItem | undefined;
  matched: number | null;
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
      <div style={{ display: "flex", gap: "var(--space-2)", flexWrap: "wrap" }}>
        <KpiCard
          label={t("pageheader.population")}
          value={s?.population != null ? String(s.population) : "—"}
          hint={t("pageheader.populationHint")}
        />
        {s?.notable === true && (
          <KpiCard
            label={t("pageheader.notable")}
            value={`${s.notable_level} ×${s.notable_count}`}
            tone={s.notable_level === "critical" ? "critical" : "warning"}
            hint={t("pageheader.notableHint")}
          />
        )}
        {collection != null && (
          <KpiCard
            label={t("pageheader.collection")}
            value={`${collection.collected}/${collection.source_total ?? "?"}`}
            hint={t("pageheader.collectionHint")}
          />
        )}
        {props.matched !== null && (
          <KpiCard
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
