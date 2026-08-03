import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { eventKindLabel } from "../api/codes";
import type { EventFact } from "../api/types";
import { useTimelineEvents } from "../api/timeline";
import { formatNumber, formatTimestampUs } from "../design/format";
import { button as uiButton } from "../design/ui";

interface EventsSignalPanelProps {
  from: string;
  to: string;
  preset: string | null;
  onInvestigate: (view: string, atUs: string, eventInstance: string) => void;
}

const MAX_SIGNAL_LANES = 6;
const EVENT_REQUEST_LIMIT = 50;

const panel: CSSProperties = {
  minWidth: 0,
  height: "100%",
  overflow: "hidden",
  padding: "var(--space-1)",
  color: "var(--fg)",
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-md)",
  fontFamily: "var(--ui-font)",
  fontSize: "var(--text-xs)",
};

function eventMatchesPreset(event: EventFact, preset: string | null): boolean {
  const code = event.event_kind;
  switch (preset) {
    case "errors":
      return code.includes("error");
    case "checkpoints":
      return code.startsWith("pg.checkpoint.");
    case "vacuum":
      return code.startsWith("pg.maintenance.");
    case "slow":
      return code.startsWith("pg.query.slow_");
    case "collector_health":
      return code.startsWith("collector.");
    default:
      return true;
  }
}

function investigationView(event: EventFact): string {
  switch (event.entity?.kind) {
    case "host":
    case "filesystem":
    case "cgroup":
      return "processes";
    case "postmaster":
    case "replication_sender":
    case "replication_slot":
      return "activity";
    case "database":
      return "tables";
    default:
      return "events";
  }
}

function eventAt(event: EventFact): string {
  return String(event.occurred_at_us ?? event.sort_ts_us);
}

function lossText(event: EventFact): string | null {
  if (event.loss === null) return null;
  const lowerBound = event.loss.lost_count_lower_bound;
  const reasons = event.loss.reasons.join(", ");
  return [
    lowerBound === null ? null : `lost≥${formatNumber(lowerBound)}`,
    reasons === "" ? null : reasons,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
}

export function EventsSignalPanel(props: EventsSignalPanelProps) {
  const { t } = useTranslation();
  const events = useTimelineEvents({
    from: props.from,
    to: props.to,
    limit: EVENT_REQUEST_LIMIT,
  });
  const matching = [...(events.data?.events ?? [])]
    .filter((event) => eventMatchesPreset(event, props.preset))
    .sort((left, right) => right.sort_ts_us - left.sort_ts_us);
  const visible = matching.slice(0, MAX_SIGNAL_LANES);
  const quality = events.data;

  return (
    <aside data-testid="events-signal-panel" data-view="events" style={panel}>
      <div
        style={{
          display: "flex",
          alignItems: "baseline",
          gap: "var(--space-2)",
          marginBlockEnd: "var(--space-1)",
        }}
      >
        <strong style={{ fontSize: "var(--text-sm)" }}>
          {t("eventsSignals.title")}
        </strong>
        <span
          data-testid="event-signals-quality"
          role="status"
          aria-live="polite"
          style={{
            minWidth: 0,
            marginInlineStart: "auto",
            overflow: "hidden",
            color: "var(--fg-dim)",
            fontFamily: "var(--mono-font)",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {quality === undefined
            ? t("table.loading")
            : `${quality.completeness} · ${quality.retained_exactness} · ${visible.length}/${matching.length}`}
        </span>
      </div>
      {events.isPending ? (
        <div style={{ color: "var(--fg-dim)" }}>{t("table.loading")}</div>
      ) : events.isError ? (
        <div
          role="alert"
          style={{
            display: "flex",
            alignItems: "center",
            gap: "var(--space-2)",
            color: "var(--sev-warn-fg)",
          }}
        >
          <span>{t("eventsSignals.error")}</span>
          <button
            type="button"
            disabled={events.isFetching}
            onClick={() => void events.refetch()}
            style={{ ...uiButton, marginInlineStart: "auto" }}
          >
            {t("eventsSignals.retry")}
          </button>
        </div>
      ) : visible.length === 0 ? (
        <div style={{ color: "var(--fg-dim)" }}>{t("eventsSignals.empty")}</div>
      ) : (
        <div
          data-testid="event-signal-lanes"
          style={{ display: "grid", gap: "calc(var(--space-1) / 2)" }}
        >
          {visible.map((event) => {
            const target = investigationView(event);
            const lost = lossText(event);
            const qualityText = `${event.identity_quality}/${event.evidence_quality}`;
            const label = eventKindLabel(t, event.event_kind);
            return (
              <button
                key={event.event_instance_id}
                type="button"
                data-testid="event-signal-lane"
                data-event-instance={event.event_instance_id}
                aria-label={t("eventsSignals.investigate", {
                  event: label,
                  target: t(
                    target === "processes"
                      ? "navigation.destination.os"
                      : `tabs.${target}`,
                  ),
                  quality: qualityText,
                })}
                title={t("eventsSignals.tooltip", {
                  eventKind: event.event_kind,
                  quality: qualityText,
                  count: event.supporting_evidence.length,
                })}
                onClick={() =>
                  props.onInvestigate(
                    target,
                    eventAt(event),
                    event.event_instance_id,
                  )
                }
                style={{
                  display: "grid",
                  gridTemplateColumns: "auto minmax(0, 1fr) auto",
                  alignItems: "center",
                  gap: "var(--space-2)",
                  minWidth: 0,
                  padding: "0 var(--space-2)",
                  color: "var(--fg)",
                  background: "var(--bg)",
                  border: "1px solid var(--border)",
                  borderRadius: "var(--radius-sm)",
                  cursor: "pointer",
                  fontFamily: "var(--mono-font)",
                  fontSize: "var(--text-xs)",
                  textAlign: "start",
                }}
              >
                <time
                  dateTime={new Date(
                    Number(eventAt(event)) / 1000,
                  ).toISOString()}
                >
                  {formatTimestampUs(eventAt(event)).split(", ").at(-1)}
                </time>
                <span
                  style={{
                    minWidth: 0,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {label} · {qualityText}
                  {event.occurrence_count > 1
                    ? ` · ×${formatNumber(event.occurrence_count)}`
                    : ""}
                  {lost === null ? "" : ` · ${lost}`}
                  {` · ${t("eventsSignals.supportingEvidence", {
                    count: event.supporting_evidence.length,
                  })}`}
                </span>
                <span style={{ color: "var(--accent-strong)" }}>
                  → {target === "processes" ? "OS" : target}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </aside>
  );
}
