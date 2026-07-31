import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ContextResponse, IncidentsResponse } from "../api/types";
import { button, chip, chipInteractive, text, verdictTint } from "../design/ui";
import type { UiState } from "../state/url";
import { DataHealthPopover } from "./DataHealthPopover";
import { TipRow, Tooltip } from "./Tooltip";

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

/** 170003 → "17.3". */
function formatPgVersion(versionNum: number): string {
  const major = Math.floor(versionNum / 10000);
  const minor = versionNum % 100;
  return major >= 10
    ? `${major}.${minor}`
    : `${major}.${Math.floor((versionNum % 10000) / 100)}.${minor}`;
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
    <span
      data-testid="clock"
      style={{
        fontFamily: "var(--mono-font)",
        fontSize: "var(--text-sm)",
        color: "var(--fg-dim)",
      }}
    >
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
      <button type="button" onClick={copy} style={button}>
        ⧉ {t("header.copyLink")}
      </button>
      {copied && (
        <span
          data-testid="toast"
          role="status"
          style={{
            position: "absolute",
            top: "100%",
            right: 0,
            marginTop: "6px",
            padding: "3px 8px",
            background: "var(--bg-overlay)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-sm)",
            boxShadow: "var(--shadow-pop)",
            color: "var(--sev-ok-fg)",
            fontSize: text.sm,
            whiteSpace: "nowrap",
            zIndex: 20,
          }}
        >
          {t("header.linkCopied")}
        </span>
      )}
    </span>
  );
}

function Dot(props: { color: string; square?: boolean }) {
  return (
    <span
      aria-hidden="true"
      style={{
        display: "inline-block",
        width: "8px",
        height: "8px",
        borderRadius: props.square ? "2px" : "50%",
        background: props.color,
        flexShrink: 0,
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
      style={chip}
      data-testid="role-chip"
      title={
        instance?.role == null
          ? (instance?.role_reason ?? undefined)
          : undefined
      }
    >
      <span style={{ color: "var(--fg-strong)", fontWeight: 600 }}>{role}</span>
      {repl && (
        <span
          style={{ color: "var(--fg-dim)" }}
          title={
            repl.replay_lag_us == null
              ? (repl.replay_lag_reason ?? undefined)
              : undefined
          }
        >
          {`${repl.streaming_replicas} ${t("header.replicas")} · ${t("header.lag")} ${lag}`}
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
        gap: "6px",
        flexWrap: "wrap",
        padding: "6px 12px",
        background: "var(--bg)",
        borderBottom: "1px solid var(--border)",
        color: "var(--fg)",
        fontFamily: "var(--ui-font)",
      }}
    >
      <Tooltip
        content={
          <span style={{ display: "grid", gap: "2px" }}>
            <TipRow
              label="host"
              value={props.context?.instance.hostname ?? "local"}
              mono
            />
            {props.context?.instance.pg_version_num != null && (
              <TipRow
                label="pg"
                value={formatPgVersion(props.context.instance.pg_version_num)}
                mono
              />
            )}
            {props.context?.host.logical_cpu_count != null && (
              <TipRow
                label="cpu"
                value={props.context.host.logical_cpu_count}
                mono
              />
            )}
          </span>
        }
      >
        <span style={chip} data-testid="instance-chip">
          <Dot color="var(--sev-ok)" />
          <span style={{ color: "var(--fg-strong)", fontWeight: 600 }}>
            {props.context?.instance.hostname ?? "local"}
          </span>
        </span>
      </Tooltip>

      <RoleChip context={props.context} />

      <span style={{ position: "relative" }}>
        <button
          type="button"
          data-testid="data-health-chip"
          aria-expanded={props.dataHealthOpen}
          onClick={props.onToggleDataHealth}
          style={chipInteractive}
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
          style={{ ...chipInteractive, ...verdictTint("critical") }}
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
          style={{ ...chipInteractive, ...verdictTint("warning") }}
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
