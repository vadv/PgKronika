import { useTranslation } from "react-i18next";
import type { IncidentResponse } from "../api/types";

export interface FocusBarProps {
  incident: IncidentResponse;
  onExit: () => void;
}

/** int64 µs → local wall time (HH:MM:SS). */
export function formatIntervalTime(us: number): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(us / 1000));
}

export function FocusBar(props: FocusBarProps) {
  const { t } = useTranslation();
  const scope = props.incident.findings[0]?.scope;
  return (
    <div
      role="status"
      style={{
        display: "flex",
        alignItems: "baseline",
        gap: "8px",
        padding: "4px 8px",
        border: "1px solid var(--accent)",
        borderRadius: "4px",
        background: "var(--bg-raised)",
        color: "var(--fg)",
        fontFamily: "var(--ui-font)",
      }}
    >
      <span
        style={{
          color: "var(--accent)",
          fontSize: "0.85em",
          textTransform: "uppercase",
        }}
      >
        {t("focus.tag")}
      </span>
      <span
        style={{
          fontFamily: "var(--mono-font)",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {props.incident.incident_key}
      </span>
      <span style={{ fontFamily: "var(--mono-font)", color: "var(--fg-dim)" }}>
        {formatIntervalTime(props.incident.interval.from)}→
        {formatIntervalTime(props.incident.interval.to)}
      </span>
      {scope && (
        <span
          style={{
            fontFamily: "var(--mono-font)",
            color: "var(--fg-dim)",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {scope.logical_section}·{scope.column}
        </span>
      )}
      <button
        type="button"
        aria-label={t("focus.exit")}
        onClick={props.onExit}
        style={{
          marginInlineStart: "auto",
          color: "var(--fg-dim)",
          background: "none",
          border: "none",
          cursor: "pointer",
          fontFamily: "var(--mono-font)",
        }}
      >
        ✕
      </button>
    </div>
  );
}
