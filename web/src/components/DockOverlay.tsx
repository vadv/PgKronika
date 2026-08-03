import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError, isWarmingUp } from "../api/client";
import {
  colDesc,
  colLabel,
  relationKindDesc,
  relationKindLabel,
} from "../api/codes";
import { useEntityHistory, useEntityPoint } from "../api/entity";
import { useIncidents } from "../api/incidents";
import type {
  ColumnSpec,
  EntitySnapshotDto,
  EntityHistoryResponse,
  EntityPointResponse,
  IncidentFindingResponse,
  IncidentResponse,
  ViewSpec,
} from "../api/types";
import type { DockKind, UiState } from "../state/url";
import { isIdentityColumn, shortIdToken } from "../design/format";
import { formatCellValue } from "./cellFormat";
import { formatIntervalTime } from "./FocusBar";

export interface DockOverlayProps {
  state: UiState;
  view: ViewSpec | undefined;
  /** Shared cursor time (pinned LIVE tick from App) — never a local Date.now(). */
  at: string;
  /** Narrow viewport: the dock docks to the bottom edge as a sheet. */
  mobile: boolean;
  onClose: () => void;
  onSelectIncident: (key: string | null) => void;
  onPatch: (patch: Partial<UiState>) => void;
}

/** Localized incident title from the server's language-neutral summary code;
 * the binary provenance key never renders as a headline. */
function incidentTitle(
  incident: IncidentResponse,
  t: (key: string, opts?: Record<string, unknown>) => string,
): string {
  const specific = t(`incident.summary.${incident.summary_code}`, {
    defaultValue: "",
  });
  if (specific !== "") return specific;
  // Dynamic codes `anomaly.{section}.{column}` fall back to the column's
  // human label — a raw dotted code is never a headline.
  const dynamic = /^anomaly\.[^.]+\.(.+)$/.exec(incident.summary_code);
  if (dynamic !== null) {
    const label = t(`col.${dynamic[1]}.label`, { defaultValue: "" });
    if (label !== "") {
      return t("incident.summary.anomaly.generic", { what: label });
    }
  }
  return incident.summary_code;
}

/** Server incident level → color: the only severity source for incidents. */
function levelColor(level: string): string {
  if (level === "critical") return "var(--sev-crit)";
  if (level === "warning") return "var(--sev-warn)";
  return "var(--border)";
}

function formatEvidence(
  evidence: IncidentFindingResponse["evidence"],
): string[] {
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

/** µs window [at - span, at] over the shared cursor time (exact BigInt math). */
function stateWindow(at: string, span: number): { from: string; to: string } {
  return {
    from: (BigInt(at) - BigInt(span) * 1_000_000n).toString(),
    to: at,
  };
}

const dockStyle = (mobile: boolean) =>
  ({
    position: "fixed",
    // min() keeps the dock inside narrow viewports: on mobile triage
    // (<760px) the dock is the only path to incidents/findings.
    ...(mobile
      ? {
          // Bottom sheet: the side panel would cover half the narrow
          // viewport; a capped sheet keeps the content readable above it.
          insetInline: 0,
          insetBlockEnd: 0,
          maxHeight: "60vh",
          borderBlockStart: "1px solid var(--border)",
        }
      : {
          insetBlock: 0,
          insetInlineEnd: 0,
          width: "520px",
          maxWidth: "calc(100vw - 24px)",
          borderInlineStart: "1px solid var(--border)",
        }),
    background: "var(--bg-overlay)",
    boxShadow: "var(--shadow-pop)",
    color: "var(--fg)",
    fontFamily: "var(--ui-font)",
    overflowY: "auto",
    overflowX: "hidden",
    zIndex: 10,
    padding: "12px",
  }) as const;

const tabButtonStyle = (active: boolean) =>
  ({
    fontFamily: "var(--ui-font)",
    fontSize: "var(--text-sm)",
    color: active ? "var(--accent-strong)" : "var(--fg)",
    background: "none",
    border: "none",
    borderBottom: active ? "2px solid var(--accent)" : "2px solid transparent",
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
        {/* Confidence is shown as a neutral localized label — it is an
            evidence attribute, not a severity verdict. */}
        <span
          style={{ fontFamily: "var(--mono-font)", color: "var(--fg-dim)" }}
        >
          {t(`dock.finding.confidence.${finding.confidence}`, {
            defaultValue: finding.confidence,
          })}
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
                overflowWrap: "anywhere",
                maxWidth: "100%",
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
      <div
        style={{
          fontFamily: "var(--ui-font)",
          fontWeight: 600,
          marginBlockEnd: "4px",
          overflowWrap: "anywhere",
        }}
      >
        {incidentTitle(incident, t)}
      </div>
      <div
        title={incident.incident_key}
        style={{
          fontFamily: "var(--mono-font)",
          fontSize: "var(--text-xs)",
          color: "var(--fg-dim)",
          marginBlockEnd: "6px",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
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
  at: string;
  viewCode: string | undefined;
  onSelectIncident: (key: string | null) => void;
  onPatch: (patch: Partial<UiState>) => void;
}) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<string | null>(null);
  const { from, to } = stateWindow(props.at, props.state.span);
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
      {incidents.isError && (
        <div style={{ color: "var(--sev-warn)" }} role="alert">
          {isWarmingUp(incidents.error)
            ? t("error.warming")
            : t("dock.incidents.error")}
        </div>
      )}
      {incidents.isPending && (
        <div style={{ color: "var(--fg-dim)" }}>
          {incidents.failureCount > 0 && isWarmingUp(incidents.failureReason)
            ? t("loading.warming")
            : t("dock.incidents.loading")}
        </div>
      )}
      {/* Analysis status comes first: findings are hypotheses until the
          server completes evaluation. */}
      {incidents.data !== undefined &&
        incidents.data.analysis_status !== "complete" && (
          <div
            style={{
              color: "var(--fg-dim)",
              fontFamily: "var(--mono-font)",
              marginBlockEnd: "6px",
            }}
          >
            {t("dock.incidents.analysis", {
              status: t(`incident.analysis.${incidents.data.analysis_status}`, {
                defaultValue: incidents.data.analysis_status,
              }),
            })}
          </div>
        )}
      {incidents.isSuccess && list.length === 0 && (
        <div style={{ color: "var(--fg-dim)" }}>
          {t("dock.incidents.empty")}
        </div>
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
            borderInlineStart: `4px solid ${levelColor(incident.level)}`,
            borderRadius: "4px",
            padding: "6px 8px",
            marginBlockEnd: "6px",
            color: "var(--fg)",
            cursor: "pointer",
          }}
        >
          <div
            title={incident.incident_key}
            style={{ fontFamily: "var(--ui-font)", overflowWrap: "anywhere" }}
          >
            {incidentTitle(incident, t)}
          </div>
          <div
            style={{ fontFamily: "var(--mono-font)", color: "var(--fg-dim)" }}
          >
            {formatIntervalTime(incident.interval.from)}→
            {formatIntervalTime(incident.interval.to)}
          </div>
          <div style={{ color: "var(--fg-dim)", fontSize: "0.85em" }}>
            {t(`verdict.level.${incident.level}`, {
              defaultValue: incident.level,
            })}
            {" · "}
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

function EntityPointView(props: {
  data: EntityPointResponse;
  columns: Map<string, ColumnSpec>;
  viewCode: string;
}) {
  const { t } = useTranslation();
  return (
    <div
      data-kv
      style={{
        display: "grid",
        gridTemplateColumns: "130px 1fr",
        gap: "2px 8px",
        alignItems: "baseline",
      }}
    >
      {props.data.fields.map((field) => {
        const spec = props.columns.get(field.code);
        const cellColumn = {
          code: field.code,
          type: spec?.type ?? "text",
          unit: spec?.unit ?? null,
        };
        const label = colLabel(t, props.viewCode, field.code);
        const desc = colDesc(t, props.viewCode, field.code);
        const availability = spec?.availability ?? "available";
        const isSql =
          typeof field.value === "string" &&
          (field.value.length > 60 || field.value.includes("\n"));
        // Honest absence: a column the source never fills (or the store
        // gates) renders its availability status, not a blank em-dash.
        const notCollected =
          field.value === null && availability !== "available";
        // Identifier values are never cut in the dock: full mono text,
        // wrap anywhere, one click selects the whole value for copying.
        const fullIdentity =
          isIdentityColumn(field.code) && field.value !== null;
        const display = notCollected
          ? t(`availability.${availability}`, { defaultValue: availability })
          : fullIdentity
            ? String(field.value)
            : formatCellValue(field.value, cellColumn, t);
        return isSql && !notCollected ? (
          <div
            key={field.code}
            data-field={field.code}
            style={{ gridColumn: "1 / -1", marginBlock: "4px" }}
          >
            <div
              title={desc ?? undefined}
              style={{
                color: "var(--fg-dim)",
                fontFamily: "var(--ui-font)",
              }}
            >
              {label}
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
              {field.value}
            </pre>
          </div>
        ) : (
          <div
            key={field.code}
            data-field={field.code}
            style={{ display: "contents" }}
          >
            <span
              title={desc ?? undefined}
              style={{
                color: "var(--fg-dim)",
                fontFamily: "var(--ui-font)",
              }}
            >
              {label}
            </span>
            <span
              style={{
                fontFamily: "var(--mono-font)",
                color:
                  field.value !== null && !notCollected
                    ? "var(--fg)"
                    : "var(--fg-dim)",
                overflowWrap: "break-word",
                ...(fullIdentity ? { userSelect: "all" } : {}),
              }}
            >
              {display}
            </span>
          </div>
        );
      })}
    </div>
  );
}

const historyHeadCellStyle = {
  textAlign: "start",
  color: "var(--fg-dim)",
  fontWeight: "normal",
  borderBottom: "1px solid var(--border)",
  padding: "2px 6px 2px 0",
} as const;

const historyCellStyle = {
  borderBottom: "1px solid var(--border)",
  padding: "2px 6px 2px 0",
  overflowWrap: "break-word",
} as const;

function EntityHistoryView(props: {
  data: EntityHistoryResponse;
  columns: Map<string, ColumnSpec>;
  viewCode: string;
  loadingMore?: boolean;
  onLoadMore?: () => void;
}) {
  const { t } = useTranslation();
  const { data } = props;
  return (
    <div>
      <table
        data-detail-history
        style={{
          borderCollapse: "collapse",
          fontFamily: "var(--mono-font)",
          width: "100%",
        }}
      >
        <thead>
          <tr>
            <th style={historyHeadCellStyle}>ts</th>
            {data.columns.map((column) => (
              <th
                key={column}
                title={colDesc(t, props.viewCode, column) ?? undefined}
                style={historyHeadCellStyle}
              >
                {colLabel(t, props.viewCode, column)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {data.snapshots.map((snapshot) => (
            <tr key={snapshot.ts_us}>
              <td style={{ ...historyCellStyle, color: "var(--fg-dim)" }}>
                {formatIntervalTime(Number(snapshot.ts_us))}
              </td>
              {data.columns.map((column, i) => {
                const value = snapshot.values[i] ?? null;
                const spec = props.columns.get(column);
                return (
                  <td
                    key={column}
                    style={{
                      ...historyCellStyle,
                      color: value !== null ? "var(--fg)" : "var(--fg-dim)",
                    }}
                  >
                    {formatCellValue(
                      value,
                      {
                        code: column,
                        type: spec?.type ?? "text",
                        unit: spec?.unit ?? null,
                      },
                      t,
                    )}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
      {data.page.next !== null && (
        <button
          type="button"
          disabled={props.loadingMore === true}
          onClick={props.onLoadMore}
          style={{
            color: "var(--accent)",
            fontFamily: "var(--mono-font)",
            background: "none",
            border: "none",
            padding: "4px 0",
            cursor: "pointer",
            marginBlockStart: "4px",
          }}
        >
          {props.loadingMore === true
            ? t("table.loading")
            : t("dock.row.loadMore")}
        </button>
      )}
    </div>
  );
}

type EntityDetailTab = "summary" | "history" | "relationships" | "raw";

const ENTITY_DETAIL_TABS: EntityDetailTab[] = [
  "summary",
  "history",
  "relationships",
  "raw",
];
const MAX_DETAIL_HISTORY_SECONDS = 21_600;

function detailHistoryColumns(view: ViewSpec | undefined): string[] {
  if (view === undefined || !view.capabilities.history) return [];
  const useful = view.columns.filter(
    (column) =>
      !column.lazy &&
      column.availability === "available" &&
      !isIdentityColumn(column.code),
  );
  const metricLike = useful.filter((column) => column.type !== "text");
  return (metricLike.length > 0 ? metricLike : useful)
    .slice(0, 6)
    .map((column) => column.code);
}

function uniqueSnapshots(
  existing: EntitySnapshotDto[],
  incoming: EntitySnapshotDto[],
): EntitySnapshotDto[] {
  const snapshots = new Map(existing.map((item) => [item.ts_us, item]));
  for (const item of incoming) snapshots.set(item.ts_us, item);
  return [...snapshots.values()].sort((left, right) => {
    const leftTs = BigInt(left.ts_us);
    const rightTs = BigInt(right.ts_us);
    return leftTs < rightTs ? -1 : leftTs > rightTs ? 1 : 0;
  });
}

function RowDock(props: {
  state: UiState;
  view: ViewSpec | undefined;
  /** Resolved cursor time (LIVE tick when the URL pins none) — the entity
   * point query needs it: the API admits only point (`at`) or history
   * (`from`+`to`+`columns`) shapes, a bare token is a 400. */
  at: string;
  onPatch: (patch: Partial<UiState>) => void;
}) {
  const { t } = useTranslation();
  const [detailTab, setDetailTab] = useState<EntityDetailTab>("summary");
  const [historyCursor, setHistoryCursor] = useState<string | null>(null);
  const [historyBase, setHistoryBase] = useState<EntityHistoryResponse | null>(
    null,
  );
  const [extraSnapshots, setExtraSnapshots] = useState<EntitySnapshotDto[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const entityKey = `${props.state.view}:${props.state.entity ?? ""}`;
  const entity = useEntityPoint({
    view: props.state.view,
    entity: props.state.entity ?? "",
    at: props.at,
    includeRelated: true,
  });
  const historyColumns = detailHistoryColumns(props.view);
  const historySpan = Math.min(props.state.span, MAX_DETAIL_HISTORY_SECONDS);
  const historyFrom = (
    BigInt(props.at) -
    BigInt(historySpan) * 1_000_000n
  ).toString();
  const historyKey = `${entityKey}:${historyFrom}:${props.at}:${historyColumns.join(",")}`;
  const history = useEntityHistory({
    view: props.state.view,
    entity: props.state.entity ?? "",
    from: historyFrom,
    to: props.at,
    columns: historyColumns,
    limit: 200,
    cursor: historyCursor,
    enabled: detailTab === "history" && historyColumns.length > 0,
  });

  useEffect(() => {
    setDetailTab("summary");
  }, [entityKey]);
  useEffect(() => {
    setHistoryCursor(null);
    setHistoryBase(null);
    setExtraSnapshots([]);
    setNextCursor(null);
  }, [historyKey]);
  useEffect(() => {
    const data = history.data;
    if (data === undefined) return;
    if (historyCursor !== null) {
      setExtraSnapshots((previous) =>
        uniqueSnapshots(previous, data.snapshots),
      );
      setNextCursor(data.page.next ?? null);
      setHistoryCursor(null);
    } else {
      setHistoryBase((previous) => previous ?? data);
      setNextCursor(data.page.next ?? null);
    }
  }, [history.data, historyCursor]);
  const viewCode = props.view?.code ?? props.state.view;
  const data = props.state.entity === null ? undefined : entity.data;
  const apiError = entity.error instanceof ApiError ? entity.error : null;
  // Only a typed not-found means "no such entity"; any other failure is an
  // error state, not absence.
  const notFound =
    apiError !== null &&
    (apiError.code === "entity_not_found" ||
      apiError.code === "view_gone" ||
      apiError.status === 404 ||
      apiError.status === 410);
  const failed = entity.isError && !notFound;
  const missing = props.state.entity === null || notFound;
  const columnSpecs = new Map(
    (props.view?.columns ?? []).map((c) => [c.code, c]),
  );
  // The API label is the human row name (index/relation/pid); the typed
  // entity token is routing material — short form, full value in the title.
  const label = data !== undefined && data.label !== "" ? data.label : null;
  // Statement/plan labels deliberately remain stable numeric identities even
  // when bounded SQL text is available in the detail fields. The heading is
  // the tab name plus a short id; the full id stays in the field list below.
  const heading =
    label !== null &&
    (viewCode === "statements" || viewCode === "plans") &&
    /^-?\d+$/.test(label)
      ? t(`dock.row.heading.${viewCode}`, { id: shortIdToken(label) })
      : label;
  const visibleHistory =
    historyBase ?? (historyCursor === null ? history.data : undefined);
  const combinedHistory =
    visibleHistory === undefined
      ? undefined
      : {
          ...visibleHistory,
          snapshots: uniqueSnapshots(visibleHistory.snapshots, extraSnapshots),
          page: { next: nextCursor },
        };

  const [tokenCopied, setTokenCopied] = useState(false);
  const copyToken = () => {
    if (props.state.entity === null) return;
    void navigator.clipboard.writeText(props.state.entity);
    setTokenCopied(true);
    setTimeout(() => setTokenCopied(false), 1700);
  };

  return (
    <div>
      {/* The heading is always human text (tab + server label); the entity
          token is routing material — it lives in the heading tooltip and
          rides the clipboard, never a visible line of its own. */}
      <div
        title={props.state.entity ?? undefined}
        style={{
          fontFamily: "var(--ui-font)",
          marginBlockEnd: "6px",
          overflowWrap: "anywhere",
        }}
      >
        <span style={{ color: "var(--fg-dim)" }}>{t(`tabs.${viewCode}`)}</span>
        {heading !== null && (
          <>
            {" · "}
            <span style={{ fontWeight: 600 }}>{heading}</span>
          </>
        )}
        {props.state.entity !== null && (
          <button
            type="button"
            data-testid="dock-copy-token"
            title={t("dock.row.copyToken")}
            onClick={copyToken}
            style={{
              marginInlineStart: "6px",
              fontFamily: "var(--mono-font)",
              fontSize: "var(--text-xs)",
              color: "var(--fg-dim)",
              background: "none",
              border: "none",
              padding: 0,
              cursor: "pointer",
            }}
          >
            {tokenCopied ? t("dock.row.tokenCopied") : "⧉"}
          </button>
        )}
      </div>
      {missing && (
        <div style={{ color: "var(--fg-dim)" }}>{t("dock.row.missing")}</div>
      )}
      {failed && (
        <div style={{ color: "var(--sev-warn)" }} role="alert">
          {isWarmingUp(entity.error) ? t("error.warming") : t("dock.row.error")}
        </div>
      )}
      {entity.isPending && (
        <div role="status" style={{ color: "var(--fg-dim)" }}>
          {entity.failureCount > 0 && isWarmingUp(entity.failureReason)
            ? t("loading.warming")
            : t("table.loading")}
        </div>
      )}
      {data && data.quality.status !== "complete" && (
        <div
          style={{
            color: "var(--fg-dim)",
            fontFamily: "var(--mono-font)",
            marginBlockEnd: "6px",
          }}
        >
          {t("dock.row.partial")}
        </div>
      )}
      {data !== undefined && (
        <>
          <div
            role="tablist"
            aria-label={t("dock.detail.tabs")}
            style={{
              display: "flex",
              gap: "var(--space-1)",
              borderBlockEnd: "1px solid var(--border)",
              marginBlockEnd: "var(--space-2)",
            }}
          >
            {ENTITY_DETAIL_TABS.map((tab) => (
              <button
                key={tab}
                type="button"
                role="tab"
                data-detail-tab-trigger={tab}
                aria-selected={detailTab === tab}
                onClick={() => setDetailTab(tab)}
                style={tabButtonStyle(detailTab === tab)}
              >
                {t(`dock.detail.${tab}`)}
              </button>
            ))}
          </div>
          <div role="tabpanel" data-detail-tab={detailTab}>
            {detailTab === "summary" && (
              <div>
                <div
                  data-detail-provenance
                  style={{
                    display: "grid",
                    gridTemplateColumns: "repeat(2, minmax(0, 1fr))",
                    gap: "var(--space-1)",
                    marginBlockEnd: "var(--space-2)",
                    padding: "var(--space-2)",
                    color: "var(--fg-dim)",
                    background: "var(--bg-raised)",
                    border: "1px solid var(--border)",
                    borderRadius: "var(--radius-sm)",
                    fontFamily: "var(--mono-font)",
                    fontSize: "var(--text-xs)",
                  }}
                >
                  <span>{formatIntervalTime(Number(data.snapshot_ts_us))}</span>
                  <span>{data.quality.status}</span>
                  <span>gaps {data.quality.gaps.length}</span>
                  <span>gated {data.quality.gated.length}</span>
                  <span style={{ gridColumn: "1 / -1" }}>
                    /v1/entity/{viewCode}/… · point projection
                  </span>
                </div>
                <EntityPointView
                  data={data}
                  columns={columnSpecs}
                  viewCode={viewCode}
                />
              </div>
            )}
            {detailTab === "history" && (
              <div>
                {props.state.span > MAX_DETAIL_HISTORY_SECONDS && (
                  <div
                    role="note"
                    style={{
                      color: "var(--fg-dim)",
                      fontFamily: "var(--mono-font)",
                      marginBlockEnd: "var(--space-2)",
                    }}
                  >
                    {t("dock.detail.historyCapped", {
                      hours: MAX_DETAIL_HISTORY_SECONDS / 3600,
                    })}
                  </div>
                )}
                {historyColumns.length === 0 ? (
                  <div style={{ color: "var(--fg-dim)" }}>
                    {t("dock.detail.historyUnavailable")}
                  </div>
                ) : history.isError ? (
                  <div role="alert" style={{ color: "var(--sev-warn-fg)" }}>
                    {t("dock.detail.historyError")}
                  </div>
                ) : combinedHistory !== undefined ? (
                  <EntityHistoryView
                    data={combinedHistory}
                    columns={columnSpecs}
                    viewCode={viewCode}
                    loadingMore={historyCursor !== null && history.isLoading}
                    onLoadMore={
                      nextCursor !== null
                        ? () => setHistoryCursor(nextCursor)
                        : undefined
                    }
                  />
                ) : (
                  <div role="status" style={{ color: "var(--fg-dim)" }}>
                    {t("table.loading")}
                  </div>
                )}
              </div>
            )}
            {detailTab === "relationships" && (
              <div>
                {data.related.length === 0 && (
                  <div style={{ color: "var(--fg-dim)" }}>
                    {t("dock.detail.noRelationships")}
                  </div>
                )}
                {data.related.map((relation) => {
                  const kind = relationKindLabel(t, relation.provenance.kind);
                  const kindDesc = relationKindDesc(
                    t,
                    relation.provenance.kind,
                  );
                  return (
                    <button
                      key={`${relation.view}:${relation.entity}`}
                      type="button"
                      onClick={() =>
                        props.onPatch({
                          view: relation.view,
                          entity: relation.entity,
                          dock: "row",
                        })
                      }
                      style={{
                        display: "block",
                        width: "100%",
                        marginBlockEnd: "var(--space-2)",
                        padding: "var(--space-2)",
                        color: "var(--fg)",
                        background: "var(--bg-raised)",
                        border: "1px solid var(--border)",
                        borderRadius: "var(--radius-sm)",
                        textAlign: "start",
                        cursor: "pointer",
                      }}
                    >
                      <strong>{relation.relation}</strong>
                      <span
                        style={{
                          display: "block",
                          color: "var(--fg-dim)",
                          fontFamily: "var(--mono-font)",
                          fontSize: "var(--text-xs)",
                        }}
                      >
                        {kind} · {relation.provenance.method} ·{" "}
                        {relation.provenance.fields.join(", ")}
                      </span>
                      {kindDesc !== null && (
                        <span style={{ color: "var(--fg-dim)" }}>
                          {kindDesc}
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
            )}
            {detailTab === "raw" && (
              <div>
                <div
                  role="note"
                  style={{
                    color: "var(--fg-dim)",
                    marginBlockEnd: "var(--space-2)",
                  }}
                >
                  {t("dock.detail.rawProjectedOnly")}
                </div>
                <pre
                  data-raw-evidence
                  style={{
                    margin: 0,
                    padding: "var(--space-2)",
                    color: "var(--fg)",
                    background: "var(--bg)",
                    border: "1px solid var(--border)",
                    borderRadius: "var(--radius-sm)",
                    fontFamily: "var(--mono-font)",
                    fontSize: "var(--text-xs)",
                    whiteSpace: "pre-wrap",
                    overflowWrap: "anywhere",
                  }}
                >
                  {JSON.stringify(data, null, 2)}
                </pre>
              </div>
            )}
          </div>
        </>
      )}
      <div
        style={{
          display: "flex",
          gap: "8px",
          marginBlockStart: "8px",
          flexWrap: "wrap",
        }}
      >
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
    <aside
      data-dock={active}
      style={dockStyle(props.mobile)}
      aria-label={t("dock.title")}
    >
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
          at={props.at}
          viewCode={props.view?.code ?? props.state.view}
          onSelectIncident={props.onSelectIncident}
          onPatch={props.onPatch}
        />
      ) : (
        <RowDock
          state={props.state}
          view={props.view}
          at={props.at}
          onPatch={props.onPatch}
        />
      )}
    </aside>
  );
}
