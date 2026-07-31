import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useFrame } from "../api/frame";
import { useIncidents } from "../api/incidents";
import type {
  FrameValue,
  IncidentFindingResponse,
  IncidentResponse,
  ViewSpec,
} from "../api/types";
import type { DockKind, UiState } from "../state/url";
import { formatIntervalTime } from "./FocusBar";

export interface DockOverlayProps {
  state: UiState;
  view: ViewSpec | undefined;
  onClose: () => void;
  onSelectIncident: (key: string | null) => void;
  onPatch: (patch: Partial<UiState>) => void;
}

/** Finding confidence → accent color used for borders and labels. */
function confidenceColor(confidence: string): string {
  if (confidence === "high") return "var(--sev-crit)";
  if (confidence === "medium") return "var(--sev-warn)";
  return "var(--border)";
}

/** Threshold verdict level → color, mirroring the table cell coloring. */
function levelColor(level: string): string {
  if (level === "critical") return "var(--sev-crit)";
  if (level === "warning") return "var(--sev-warn)";
  if (level === "ok") return "var(--sev-ok)";
  return "var(--fg-dim)";
}

function maxConfidence(incident: IncidentResponse): string {
  if (incident.findings.some((f) => f.confidence === "high")) return "high";
  if (incident.findings.some((f) => f.confidence === "medium"))
    return "medium";
  return "low";
}

function formatCell(value: FrameValue): string {
  if (value === null) return "—";
  return String(value);
}

function formatEvidence(evidence: IncidentFindingResponse["evidence"]): string[] {
  return evidence.map((e) => (typeof e === "string" ? e : JSON.stringify(e)));
}

/** View codes a finding `logical_section` can legitimately navigate to. */
const KNOWN_VIEW_CODES = new Set([
  "activity",
  "statements",
  "plans",
  "tables",
  "indexes",
  "sessions",
  "locks",
  "databases",
]);

/**
 * A finding scope is navigable when its section names the current view or
 * one of the known view codes.
 */
function scopeViewCode(
  scope: IncidentFindingResponse["scope"],
  current: string | undefined,
): string | undefined {
  if (scope.logical_section === current) return scope.logical_section;
  return KNOWN_VIEW_CODES.has(scope.logical_section)
    ? scope.logical_section
    : undefined;
}

/** µs window derived from the cursor state: [at - span, at], at = now when LIVE. */
function stateWindow(state: UiState): { from: string; to: string } {
  const to = state.at ?? String(Date.now() * 1000);
  const from = String(Number(to) - state.span * 1e6);
  return { from, to };
}

const dockStyle = {
  position: "fixed",
  insetBlock: 0,
  insetInlineEnd: 0,
  width: "clamp(400px, 32vw, 560px)",
  background: "var(--bg-raised)",
  borderInlineStart: "1px solid var(--border)",
  color: "var(--fg)",
  fontFamily: "var(--ui-font)",
  overflowY: "auto",
  zIndex: 10,
  padding: "8px",
} as const;

const tabButtonStyle = (active: boolean) =>
  ({
    fontFamily: "var(--mono-font)",
    color: active ? "var(--accent)" : "var(--fg)",
    background: "none",
    border: "none",
    borderBottom: active
      ? "2px solid var(--accent)"
      : "2px solid transparent",
    cursor: "pointer",
  }) as const;

function FindingCard(props: {
  finding: IncidentFindingResponse;
  viewCode: string | undefined;
  onJump: (view: string) => void;
}) {
  const { t } = useTranslation();
  const { finding } = props;
  const target = scopeViewCode(finding.scope, props.viewCode);
  return (
    <div
      data-finding
      style={{
        border: "1px solid var(--border)",
        borderInlineStart: `3px solid ${confidenceColor(finding.confidence)}`,
        borderRadius: "4px",
        padding: "6px 8px",
        marginBlockEnd: "6px",
      }}
    >
      <div style={{ display: "flex", gap: "8px", alignItems: "baseline" }}>
        <span style={{ fontFamily: "var(--mono-font)" }}>
          {finding.lens_id}
        </span>
        <span style={{ color: "var(--fg-dim)" }}>{finding.role}</span>
        <span
          style={{
            fontFamily: "var(--mono-font)",
            color: confidenceColor(finding.confidence),
          }}
        >
          {finding.confidence}
        </span>
      </div>
      <div style={{ fontFamily: "var(--mono-font)", color: "var(--fg-dim)" }}>
        {finding.scope.logical_section}·{finding.scope.column}
      </div>
      {finding.evidence.length > 0 && (
        <div
          style={{
            display: "flex",
            flexWrap: "wrap",
            gap: "4px",
            marginBlockStart: "4px",
          }}
        >
          {formatEvidence(finding.evidence).map((e) => (
            <span
              key={e}
              style={{
                fontFamily: "var(--mono-font)",
                fontSize: "0.85em",
                border: "1px solid var(--border)",
                borderRadius: "4px",
                padding: "0 4px",
                color: "var(--fg-dim)",
              }}
            >
              {e}
            </span>
          ))}
        </div>
      )}
      {target !== undefined && (
        <button
          type="button"
          onClick={() => props.onJump(target)}
          style={{
            marginBlockStart: "4px",
            fontFamily: "var(--mono-font)",
            color: "var(--accent)",
            background: "none",
            border: "none",
            padding: 0,
            cursor: "pointer",
          }}
        >
          {t("dock.incidents.jump", { view: target })}
        </button>
      )}
    </div>
  );
}

function IncidentDetail(props: {
  incident: IncidentResponse;
  viewCode: string | undefined;
  onBack: () => void;
  onPatch: (patch: Partial<UiState>) => void;
}) {
  const { t } = useTranslation();
  const { incident } = props;
  return (
    <div>
      <button
        type="button"
        onClick={props.onBack}
        style={{
          fontFamily: "var(--mono-font)",
          color: "var(--accent)",
          background: "none",
          border: "none",
          padding: 0,
          marginBlockEnd: "6px",
          cursor: "pointer",
        }}
      >
        {t("dock.incidents.back")}
      </button>
      <div style={{ fontFamily: "var(--mono-font)", marginBlockEnd: "4px" }}>
        {incident.incident_key}
      </div>
      <div
        style={{
          fontFamily: "var(--mono-font)",
          color: "var(--fg-dim)",
          marginBlockEnd: "8px",
        }}
      >
        {formatIntervalTime(incident.interval.from)}→
        {formatIntervalTime(incident.interval.to)}
      </div>
      {incident.findings.map((f) => (
        <FindingCard
          key={`${f.lens_id}:${f.role}`}
          finding={f}
          viewCode={props.viewCode}
          onJump={(view) =>
            props.onPatch({ view, focus: incident.incident_key })
          }
        />
      ))}
    </div>
  );
}

function IncidentsDock(props: {
  state: UiState;
  viewCode: string | undefined;
  onSelectIncident: (key: string | null) => void;
  onPatch: (patch: Partial<UiState>) => void;
}) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<string | null>(null);
  const { from, to } = stateWindow(props.state);
  const incidents = useIncidents({ from, to });
  const list = incidents.data?.incidents ?? [];
  const detail = selected
    ? list.find((i) => i.incident_key === selected)
    : undefined;

  const select = (key: string | null) => {
    setSelected(key);
    props.onSelectIncident(key);
  };

  if (detail) {
    return (
      <IncidentDetail
        incident={detail}
        viewCode={props.viewCode}
        onBack={() => select(null)}
        onPatch={props.onPatch}
      />
    );
  }

  return (
    <div>
      {incidents.isSuccess && list.length === 0 && (
        <div style={{ color: "var(--fg-dim)" }}>{t("dock.incidents.empty")}</div>
      )}
      {list.map((incident) => (
        <button
          key={incident.incident_key}
          type="button"
          data-incident={incident.incident_key}
          onClick={() => select(incident.incident_key)}
          style={{
            display: "block",
            width: "100%",
            textAlign: "start",
            background: "none",
            border: "1px solid var(--border)",
            borderInlineStart: `3px solid ${confidenceColor(maxConfidence(incident))}`,
            borderRadius: "4px",
            padding: "6px 8px",
            marginBlockEnd: "6px",
            color: "var(--fg)",
            cursor: "pointer",
          }}
        >
          <div style={{ fontFamily: "var(--mono-font)" }}>
            {incident.incident_key}
          </div>
          <div
            style={{ fontFamily: "var(--mono-font)", color: "var(--fg-dim)" }}
          >
            {formatIntervalTime(incident.interval.from)}→
            {formatIntervalTime(incident.interval.to)}
          </div>
          <div style={{ color: "var(--fg-dim)", fontSize: "0.85em" }}>
            {t("dock.incidents.counts", {
              members: incident.members.length,
              findings: incident.findings.length,
            })}
          </div>
        </button>
      ))}
    </div>
  );
}

function RowDock(props: {
  state: UiState;
  view: ViewSpec | undefined;
  onPatch: (patch: Partial<UiState>) => void;
}) {
  const { t } = useTranslation();
  const frame = useFrame({
    view: props.state.view,
    at: props.state.at ?? String(Date.now() * 1000),
    span: props.state.span,
    preset: props.state.preset,
    q: props.state.q,
    sort: props.state.sort,
    order: props.state.order,
  });
  const row = frame.data?.rows.find((r) => r.entity === props.state.entity);
  const viewCode = props.view?.code ?? props.state.view;

  return (
    <div>
      <div
        style={{
          fontFamily: "var(--mono-font)",
          marginBlockEnd: "8px",
          overflow: "hidden",
          textOverflow: "ellipsis",
        }}
      >
        {viewCode} · {props.state.entity ?? "—"}
      </div>
      {frame.isSuccess && !row && (
        <div style={{ color: "var(--fg-dim)" }}>{t("dock.row.missing")}</div>
      )}
      {frame.data && row && (
        <div
          data-kv
          style={{
            display: "grid",
            gridTemplateColumns: "130px 1fr",
            gap: "2px 8px",
            alignItems: "baseline",
          }}
        >
          {frame.data.columns.map((column, i) => {
            const value = row.cells[i] ?? null;
            const classification = row.classifications.find(
              (c) => c.column === column.code,
            );
            const level =
              classification && "level" in classification.result
                ? classification.result.level
                : undefined;
            const isSql =
              column.type === "text" &&
              typeof value === "string" &&
              (value.length > 60 || value.includes("\n"));
            return isSql ? (
              <div
                key={column.code}
                style={{ gridColumn: "1 / -1", marginBlock: "4px" }}
              >
                <div
                  style={{
                    color: "var(--fg-dim)",
                    fontFamily: "var(--mono-font)",
                  }}
                >
                  {column.code}
                </div>
                <pre
                  data-sql
                  style={{
                    margin: 0,
                    fontFamily: "var(--mono-font)",
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-word",
                    color: "var(--fg)",
                    border: "1px solid var(--border)",
                    borderRadius: "4px",
                    padding: "4px 6px",
                  }}
                >
                  {value}
                </pre>
              </div>
            ) : (
              <div key={column.code} style={{ display: "contents" }}>
                <span
                  style={{
                    color: "var(--fg-dim)",
                    fontFamily: "var(--mono-font)",
                  }}
                >
                  {column.code}
                </span>
                <span
                  data-verdict={level}
                  title={level}
                  style={{
                    fontFamily: "var(--mono-font)",
                    color:
                      level !== undefined ? levelColor(level) : "var(--fg)",
                    overflowWrap: "break-word",
                  }}
                >
                  {formatCell(value)}
                </span>
              </div>
            );
          })}
        </div>
      )}
      <div style={{ display: "flex", gap: "8px", marginBlockStart: "8px" }}>
        {viewCode === "statements" && (
          <button
            type="button"
            onClick={() =>
              props.onPatch({
                view: "plans",
                q: props.state.entity,
                dock: "row",
                entity: null,
              })
            }
            style={drillButtonStyle}
          >
            {t("dock.row.drill", { view: "plans" })}
          </button>
        )}
        {viewCode === "tables" && (
          <button
            type="button"
            onClick={() =>
              props.onPatch({
                view: "indexes",
                q: props.state.entity,
                dock: "row",
                entity: null,
              })
            }
            style={drillButtonStyle}
          >
            {t("dock.row.drill", { view: "indexes" })}
          </button>
        )}
        <button
          type="button"
          onClick={() => props.onPatch({ entity: null, dock: null })}
          style={drillButtonStyle}
        >
          {t("dock.row.clear")}
        </button>
      </div>
    </div>
  );
}

const drillButtonStyle = {
  fontFamily: "var(--mono-font)",
  color: "var(--accent)",
  background: "none",
  border: "1px solid var(--border)",
  borderRadius: "4px",
  padding: "2px 6px",
  cursor: "pointer",
} as const;

const DOCK_KINDS: DockKind[] = ["incidents", "row"];

export function DockOverlay(props: DockOverlayProps) {
  const { t } = useTranslation();
  if (props.state.dock === null) return null;
  const active = props.state.dock;
  return (
    <aside data-dock={active} style={dockStyle} aria-label={t("dock.title")}>
      <div
        role="tablist"
        style={{
          display: "flex",
          gap: "8px",
          alignItems: "baseline",
          marginBlockEnd: "8px",
        }}
      >
        {DOCK_KINDS.map((kind) => (
          <button
            key={kind}
            type="button"
            role="tab"
            aria-selected={active === kind}
            onClick={() => props.onPatch({ dock: kind })}
            style={tabButtonStyle(active === kind)}
          >
            {t(`dock.tabs.${kind}`)}
          </button>
        ))}
        <button
          type="button"
          aria-label={t("dock.close")}
          onClick={props.onClose}
          style={{
            marginInlineStart: "auto",
            color: "var(--fg-dim)",
            background: "none",
            border: "none",
            cursor: "pointer",
            fontFamily: "var(--mono-font)",
          }}
        >
          ×
        </button>
      </div>
      {active === "incidents" ? (
        <IncidentsDock
          state={props.state}
          viewCode={props.view?.code ?? props.state.view}
          onSelectIncident={props.onSelectIncident}
          onPatch={props.onPatch}
        />
      ) : (
        <RowDock state={props.state} view={props.view} onPatch={props.onPatch} />
      )}
    </aside>
  );
}
