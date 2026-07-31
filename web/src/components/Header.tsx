import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ContextResponse, IncidentsResponse } from "../api/types";
import type { UiState } from "../state/url";
import { DataHealthPopover } from "./DataHealthPopover";

export interface HeaderProps {
  state: UiState;
  context: ContextResponse | undefined;
  incidents: IncidentsResponse | undefined;
  /** Canonical share URL with the absolute cursor time fixed (LIVE-safe). */
  shareUrl: string;
  dataHealthOpen: boolean;
  onToggleDataHealth: () => void;
  onOpenIncidents: () => void;
}

type ContextQuality = ContextResponse["quality"];

type DataHealth = "ok" | "partial" | "unknown";

function dataHealth(quality: ContextQuality | undefined): DataHealth {
  if (!quality) return "unknown";
  // Show the API status as it is — no client-side re-classification.
  if (quality.status === "complete") return "ok";
  if (quality.status === "partial") return "partial";
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

function CopyLinkButton(props: { url: string }) {
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
    void navigator.clipboard.writeText(props.url);
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

function RoleChip(props: { context: ContextResponse | undefined }) {
  const { t } = useTranslation();
  const instance = props.context?.instance;
  const role = instance?.role ?? "—";
  const repl = props.context?.replication.instance;
  const lag =
    repl?.replay_lag_us == null
      ? "—"
      : `${Math.round(repl.replay_lag_us / 1_000_000)}s`;
  return (
    <span
      style={chipStyle}
      data-testid="role-chip"
      title={
        instance?.role == null
          ? (instance?.role_reason ?? undefined)
          : undefined
      }
    >
      {role}
      {repl && (
        <span
          title={
            repl.replay_lag_us == null
              ? (repl.replay_lag_reason ?? undefined)
              : undefined
          }
        >
          {` · ${repl.streaming_replicas} ${t("header.replicas")} · ${t("header.lag")} ${lag}`}
        </span>
      )}
    </span>
  );
}

export function Header(props: HeaderProps) {
  const { t } = useTranslation();
  const health = dataHealth(props.context?.quality);

  // Window for the data-health popover queries (int64 µs decimal strings).
  const to = props.state.at ?? String(Date.now() * 1000);
  const from = String(Number(to) - props.state.span * 1_000_000);

  // Incident severity is the server's typed verdict (`level` with
  // `level_policy_revision`), never a client-side approximation from finding
  // confidence — confidence and severity are different axes.
  let critical = 0;
  let warning = 0;
  for (const inc of props.incidents?.incidents ?? []) {
    if (inc.level === "critical") critical += 1;
    else if (inc.level === "warning") warning += 1;
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
        {props.context?.instance.hostname ?? "local"}
      </span>

      <RoleChip context={props.context} />

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
        {props.dataHealthOpen && <DataHealthPopover from={from} to={to} />}
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
      <CopyLinkButton url={props.shareUrl} />
    </header>
  );
}
