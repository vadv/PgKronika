import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { formatTimestampUs } from "../design/format";
import { verdictTint } from "../design/ui";

export function PageHeader(props: {
  view: ViewSpec;
  summary: ViewSummaryItem | undefined;
  matched: number | null;
  /** LIVE mode (no pinned cursor): a missing snapshot reads as "not yet",
   * not as an error. */
  live: boolean;
  onOpenIncidents?: () => void;
}) {
  const { t } = useTranslation();
  const s = props.summary;
  const snapshotLabel =
    s?.snapshot_ts_us != null ? formatTimestampUs(s.snapshot_ts_us) : null;

  // One context line instead of boxed KPI chips: population, collection
  // coverage and the live filter count, each with its meaning in the title.
  const contextParts: string[] = [];
  if (s?.population != null) {
    contextParts.push(`${t("pageheader.population")}: ${s.population}`);
  }
  if (props.matched !== null) {
    contextParts.push(`${t("pageheader.matched")}: ${props.matched}`);
  }

  return (
    <div
      style={{
        display: "flex",
        alignItems: "baseline",
        gap: "var(--space-3)",
        flexWrap: "wrap",
        padding: "0 2px",
      }}
    >
      <span
        style={{
          fontFamily: "var(--ui-font)",
          fontSize: "var(--text-md)",
          fontWeight: 600,
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
          : props.matched !== null
            ? t(
                props.live
                  ? "pageheader.liveRetained"
                  : "pageheader.retainedSnapshot",
              )
            : props.live
              ? t("pageheader.livePending")
              : t("pageheader.noSnapshot")}
      </span>
      {contextParts.length > 0 && (
        <span
          title={t("pageheader.matchedHint")}
          style={{
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-xs)",
            color: "var(--fg-dim)",
          }}
        >
          {contextParts.join(" · ")}
        </span>
      )}
      {s?.notable === true && (
        <button
          type="button"
          onClick={props.onOpenIncidents}
          title={t("pageheader.notableHint")}
          style={{
            display: "inline-flex",
            alignItems: "baseline",
            padding: "1px 8px",
            ...verdictTint(
              s.notable_level === "critical" ? "critical" : "warning",
            ),
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-sm)",
            whiteSpace: "nowrap",
            cursor: "pointer",
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-md)",
            fontWeight: 600,
          }}
        >
          {t(`verdict.level.${s.notable_level}`, {
            defaultValue: s.notable_level,
          })}
          {` ×${s.notable_count}`}
        </button>
      )}
    </div>
  );
}
