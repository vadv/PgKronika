import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { formatTimestampUs } from "../design/format";
import { DATA_STATES, verdictTint, type DataState } from "../design/ui";
import { ProvenancePopover } from "./ProvenancePopover";

function knownDataState(value: string): DataState | undefined {
  return DATA_STATES.find((state) => state === value);
}

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
  const collection = s?.collection;
  const collectionState =
    s !== undefined && s.status !== "complete"
      ? knownDataState(s.status)
      : collection !== undefined && collection !== null
        ? knownDataState(collection.read_state)
        : undefined;

  // One context line instead of boxed KPI chips: population, collection
  // coverage and the live filter count, each with its meaning in the title.
  const contextParts: string[] = [];
  if (s?.population != null) {
    contextParts.push(`${t("pageheader.population")}: ${s.population}`);
  }
  if (collection != null) {
    contextParts.push(
      `${t("pageheader.collection")}: ${collection.collected}/${collection.source_total ?? "?"}`,
    );
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
      {s !== undefined && collection !== null && collection !== undefined && (
        <ProvenancePopover
          triggerLabel={t("pageheader.provenance.trigger")}
          renderTrigger={() => t("pageheader.provenance.short")}
          record={{
            definition: t("pageheader.provenance.definition"),
            value: `${collection.collected}/${collection.source_total ?? "?"}`,
            snapshot: s.snapshot_ts_us ?? undefined,
            source:
              props.view.inputs.length > 0
                ? props.view.inputs.map((input) => input.code).join(", ")
                : undefined,
            coverage: t("pageheader.provenance.coverage", {
              readState: collection.read_state,
              visibility: collection.visibility,
            }),
            state: collectionState,
            reason:
              collectionState !== undefined
                ? `${s.status} · ${collection.read_state} · ${collection.visibility}`
                : undefined,
          }}
        />
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
