import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { IncidentsResponse, ViewSummaryResponse } from "../api/types";
import type { UiState } from "../state/url";
import { DataHealthPopover } from "./DataHealthPopover";

export interface HeaderProps {
  state: UiState;
  summary: ViewSummaryResponse | undefined;
  incidents: IncidentsResponse | undefined;
  dataHealthOpen: boolean;
  onToggleDataHealth: () => void;
  onOpenIncidents: () => void;
}

type DataHealth = "ok" | "partial" | "unknown";

function dataHealth(summary: ViewSummaryResponse | undefined): DataHealth {
  if (!summary) return "unknown";
  const q = summary.quality;
  if (q.status === "complete" && q.gaps.length === 0) return "ok";
  if (
    q.gaps.length > 0 ||
    q.gated.length > 0 ||
    q.resource_limited.length > 0
  ) {
    return "partial";
  }
  return "unknown";
}

const healthColor: Record<DataHealth, string> = {
  ok: "var(--sev-ok)",
  partial: "var(--sev-warn)",
  unknown: "var(--fg-dim)",
};

const healthLabelKey: Record<DataHealth, string> = {
  ok: "header.dataOk",
  partial: "header.dataPartial",
  unknown: "header.dataUnknown",
};

function Clock() {
  const { i18n } = useTranslation();
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(id);
  }, []);
  const locale = i18n.language || undefined;
  const text = new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(now);
  return (
    <span data-testid="clock" style={{ fontFamily: "var(--mono-font)" }}>
      {text}
    </span>
  );
}

function CopyLinkButton() {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );
  const copy = () => {
    void navigator.clipboard.writeText(window.location.href);
    setCopied(true);
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied(false), 1700);
  };
  return (
    <span style={{ position: "relative" }}>
      <button
        type="button"
        onClick={copy}
        style={{
          fontFamily: "var(--mono-font)",
          color: "var(--fg)",
          background: "none",
          border: "1px solid var(--border)",
          padding: "2px 8px",
          cursor: "pointer",
        }}
      >
        {t("header.copyLink")}
      </button>
      {copied && (
        <span
          data-testid="toast"
          role="status"
          style={{
            position: "absolute",
            top: "100%",
            right: 0,
            marginTop: "4px",
            padding: "2px 8px",
            background: "var(--bg-raised)",
            border: "1px solid var(--border)",
            color: "var(--sev-ok)",
            whiteSpace: "nowrap",
          }}
        >
          {t("header.linkCopied")}
        </span>
      )}
    </span>
  );
}

const chipStyle = {
  display: "inline-flex",
  alignItems: "center",
  gap: "6px",
  fontFamily: "var(--mono-font)",
  fontSize: "12px",
  color: "var(--fg)",
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  padding: "2px 8px",
} as const;

function Dot(props: { color: string; square?: boolean }) {
  return (
    <span
      aria-hidden="true"
      style={{
        display: "inline-block",
        width: "8px",
        height: "8px",
        borderRadius: props.square ? 0 : "50%",
        background: props.color,
      }}
    />
  );
}

export function Header(props: HeaderProps) {
  const { t } = useTranslation();
  const health = dataHealth(props.summary);

  // IncidentFindingResponse has no severity field — only role/confidence
  // (see web/src/api/schema.d.ts). Approximation: an incident counts as
  // critical when any of its findings has confidence "high", otherwise it
  // counts as a warning. Incidents without findings are not counted.
  let critical = 0;
  let warning = 0;
  for (const inc of props.incidents?.incidents ?? []) {
    if (inc.findings.length === 0) continue;
    if (inc.findings.some((f) => f.confidence === "high")) critical += 1;
    else warning += 1;
  }

  return (
    <header
      style={{
        display: "flex",
        alignItems: "center",
        gap: "8px",
        padding: "4px 8px",
        background: "var(--bg)",
        borderBottom: "1px solid var(--border)",
        color: "var(--fg)",
        fontFamily: "var(--ui-font)",
      }}
    >
      <span style={chipStyle} data-testid="instance-chip">
        <Dot color="var(--sev-ok)" />
        {props.state.source}
      </span>

      <span style={{ position: "relative" }}>
        <button
          type="button"
          data-testid="data-health-chip"
          aria-expanded={props.dataHealthOpen}
          onClick={props.onToggleDataHealth}
          style={{ ...chipStyle, cursor: "pointer" }}
        >
          <Dot square color={healthColor[health]} />
          {t("header.data")}: {t(healthLabelKey[health])}
        </button>
        {props.dataHealthOpen && props.summary && (
          <DataHealthPopover
            quality={props.summary.quality}
            views={props.summary.views}
          />
        )}
      </span>

      {critical > 0 && (
        <button
          type="button"
          data-testid="incidents-critical"
          onClick={props.onOpenIncidents}
          style={{ ...chipStyle, cursor: "pointer" }}
        >
          <Dot square color="var(--sev-crit)" />
          {t("header.critical")}: {critical}
        </button>
      )}
      {warning > 0 && (
        <button
          type="button"
          data-testid="incidents-warning"
          onClick={props.onOpenIncidents}
          style={{ ...chipStyle, cursor: "pointer" }}
        >
          <Dot square color="var(--sev-warn)" />
          {t("header.warning")}: {warning}
        </button>
      )}

      <span style={{ flex: 1 }} />
      <Clock />
      <CopyLinkButton />
    </header>
  );
}
