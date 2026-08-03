import { Fragment, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError, isWarmingUp } from "../api/client";
import { colDesc, colLabel } from "../api/codes";
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
import {
  formatDurationUs,
  isIdentityColumn,
  shortIdToken,
} from "../design/format";
import { formatCellValue } from "./cellFormat";
import { formatIntervalTime } from "./FocusBar";
import { SemanticBadge } from "./SemanticBadge";
import "./DockOverlay.css";

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

const dockStyle = (mobile: boolean, kind: DockKind) =>
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
      : kind === "row"
        ? {
            // The published forensic detail is a workspace, not a cramped
            // drawer. Keep global header, navigation, Health Line and footer.
            insetBlockStart: "136px",
            insetBlockEnd: "24px",
            insetInline: 0,
            borderBlockStart: "1px solid var(--border-strong)",
          }
        : {
            insetBlock: 0,
            insetInlineEnd: 0,
            width: "560px",
            maxWidth: "calc(100vw - 24px)",
            borderInlineStart: "1px solid var(--border)",
          }),
    background: "var(--bg-overlay)",
    boxShadow: !mobile && kind === "row" ? "none" : "var(--shadow-pop)",
    color: "var(--fg)",
    fontFamily: "var(--ui-font)",
    overflowY: "auto",
    overflowX: "hidden",
    zIndex: 10,
    padding: !mobile && kind === "row" ? "0" : "12px",
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
        className="entity-detail__actions"
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
  relatedActivity?: EntityPointResponse;
  onPatch: (patch: Partial<UiState>) => void;
}) {
  const { t } = useTranslation();
  const meaningful = props.data.fields.filter((field) => {
    const availability =
      props.columns.get(field.code)?.availability ?? "available";
    return (
      props.viewCode === "processes" ||
      field.value !== null ||
      availability !== "available"
    );
  });
  const identityCodes = new Set(
    props.viewCode === "activity"
      ? ["pid", "database", "user", "application", "process_link"]
      : props.viewCode === "processes"
        ? ["pid", "type", "state", "cgroup"]
        : props.viewCode === "statements"
          ? ["queryid", "database", "user"]
          : props.viewCode === "plans"
            ? ["planid", "queryid"]
            : ["schema", "table", "index", "pid", "database"],
  );
  const stateCodes = new Set(
    props.viewCode === "activity"
      ? ["state", "wait_event", "query_duration_us", "transaction_duration_us"]
      : [],
  );
  const identity = meaningful.filter((field) => identityCodes.has(field.code));
  const state = meaningful
    .filter((field) => stateCodes.has(field.code))
    .slice(0, 4);
  const body = meaningful.filter(
    (field) => !identityCodes.has(field.code) && !stateCodes.has(field.code),
  );
  const ordered = (codes: readonly string[]) =>
    codes.flatMap((code) => body.filter((field) => field.code === code));
  const groups =
    props.viewCode === "processes"
      ? [
          {
            code: "compute",
            fields: ordered([
              "cpu",
              "cpu_user",
              "cpu_system",
              "run_delay",
              "block_delay",
              "current_cpu",
              "rss",
              "virtual_memory",
              "swap",
              "threads",
              "minor_faults_per_second",
              "major_faults_per_second",
              "voluntary_context_switches_per_second",
              "involuntary_context_switches_per_second",
              "scheduler_policy",
              "nice",
              "priority",
              "realtime_priority",
            ]),
          },
          {
            code: "ioCache",
            fields: ordered([
              "logical_read_bytes_per_second",
              "cache_served_read_bytes_per_second",
              "read_bytes_per_second",
              "logical_write_bytes_per_second",
              "write_bytes_per_second",
              "read_syscalls_per_second",
              "write_syscalls_per_second",
            ]),
          },
        ]
      : [
          {
            code: "compute",
            fields: body.filter((field) =>
              /(cpu|rss|mem|thread|sched|delay|load|context)/i.test(field.code),
            ),
          },
          {
            code: "ioCache",
            fields: body.filter((field) =>
              /(read|write|(^|_)io|cache|hit|miss|block|wal|buffer|temp|disk)/i.test(
                field.code,
              ),
            ),
          },
        ];
  const groupedCodes = new Set(
    groups.flatMap((group) => group.fields.map((field) => field.code)),
  );
  groups.push({
    code: "context",
    fields:
      props.viewCode === "processes"
        ? [
            ...ordered([
              "parent_pid",
              "uid",
              "effective_uid",
              "started_at",
              "command",
            ]),
            ...body
              .filter((field) => !groupedCodes.has(field.code))
              .filter(
                (field) =>
                  ![
                    "parent_pid",
                    "uid",
                    "effective_uid",
                    "started_at",
                    "command",
                  ].includes(field.code),
              ),
          ]
        : body.filter((field) => !groupedCodes.has(field.code)),
  });

  const processSemantics: Record<string, "S" | "G" | "R" | "EST"> = {
    pid: "S",
    type: "S",
    state: "S",
    cgroup: "S",
    parent_pid: "S",
    uid: "S",
    effective_uid: "S",
    started_at: "S",
    command: "S",
    current_cpu: "G",
    rss: "G",
    virtual_memory: "G",
    swap: "G",
    threads: "G",
    scheduler_policy: "G",
    nice: "G",
    priority: "G",
    realtime_priority: "G",
    cpu: "R",
    cpu_user: "R",
    cpu_system: "R",
    run_delay: "R",
    block_delay: "R",
    minor_faults_per_second: "R",
    major_faults_per_second: "R",
    voluntary_context_switches_per_second: "R",
    involuntary_context_switches_per_second: "R",
    logical_read_bytes_per_second: "R",
    logical_write_bytes_per_second: "R",
    read_bytes_per_second: "R",
    write_bytes_per_second: "R",
    read_syscalls_per_second: "R",
    write_syscalls_per_second: "R",
    cache_served_read_bytes_per_second: "EST",
  };
  const processSubgroupBefore: Record<string, string> = {
    rss: "memory",
    logical_read_bytes_per_second: "readPath",
    logical_write_bytes_per_second: "ioRates",
    parent_pid: "execution",
  };
  const relatedActivityRelation = props.data.related.find(
    (relation) =>
      props.viewCode === "processes" &&
      relation.view === "activity" &&
      relation.relation === "activity_process",
  );
  const relatedActivityField = (code: string) =>
    props.relatedActivity?.fields.find((field) => field.code === code)?.value ??
    null;
  const relatedQuery = relatedActivityField("query");
  const relatedDuration = relatedActivityField("query_duration_us");
  const relatedMeta = [
    relatedActivityField("database"),
    relatedActivityField("user"),
    relatedActivityField("application"),
  ].filter(
    (value): value is string => typeof value === "string" && value !== "",
  );
  const relatedState = relatedActivityField("state");
  const relatedWait = relatedActivityField("wait_event");

  const renderField = (
    field: EntityPointResponse["fields"][number],
    compact = false,
  ) => {
    const spec = props.columns.get(field.code);
    const cellColumn = {
      code: field.code,
      type: spec?.type ?? "text",
      unit: spec?.unit ?? null,
    };
    const label = colLabel(t, props.viewCode, field.code);
    const desc = colDesc(t, props.viewCode, field.code);
    const availability = spec?.availability ?? "available";
    const semantic =
      props.viewCode === "processes" ? processSemantics[field.code] : undefined;
    const notCollected =
      field.value === null &&
      (props.viewCode === "processes" || availability !== "available");
    const isSql =
      typeof field.value === "string" &&
      (field.value.length > 60 || field.value.includes("\n"));
    const fullIdentity = field.value !== null && isIdentityColumn(field.code);
    const unavailableKind =
      props.viewCode === "processes" && availability === "available"
        ? "not_collected"
        : availability;
    const unavailableFallback =
      unavailableKind === "available" ? "—" : "not collected";
    const display = notCollected
      ? t(`availability.${unavailableKind}`, {
          defaultValue: unavailableFallback,
        })
      : fullIdentity
        ? String(field.value)
        : formatCellValue(field.value, cellColumn, t);
    return isSql && !compact && !notCollected ? (
      <div
        key={field.code}
        data-field={field.code}
        data-semantic={semantic === "EST" ? "estimate" : semantic}
        className="entity-detail__measurement entity-detail__measurement--block"
      >
        <div title={desc ?? undefined} className="entity-detail__label">
          <span>{label}</span>
          {semantic && <SemanticBadge kind={semantic} />}
        </div>
        <pre data-sql className="entity-detail__code">
          {field.value}
        </pre>
      </div>
    ) : (
      <div
        key={field.code}
        data-field={field.code}
        data-semantic={semantic === "EST" ? "estimate" : semantic}
        className={`entity-detail__measurement${compact ? " entity-detail__measurement--compact" : ""}`}
      >
        <span title={desc ?? undefined} className="entity-detail__label">
          <span>{label}</span>
          {semantic && <SemanticBadge kind={semantic} />}
        </span>
        <span
          className={`entity-detail__value${notCollected ? " entity-detail__value--missing" : ""}`}
          style={fullIdentity ? { userSelect: "all" } : undefined}
        >
          {display}
        </span>
      </div>
    );
  };

  return (
    <div
      data-kv
      data-forensic-summary
      data-view={props.viewCode}
      className="entity-detail__forensic"
    >
      {identity.length > 0 && (
        <div className="entity-detail__identity-strip">
          {identity.map((field) => renderField(field, true))}
        </div>
      )}
      {state.length > 0 && (
        <div className="entity-detail__state-strip">
          {state.map((field) => renderField(field, true))}
        </div>
      )}
      <div className="entity-detail__summary-grid">
        {groups
          .filter((group) => group.fields.length > 0)
          .map((group) => (
            <section
              key={group.code}
              data-forensic-group={group.code}
              className="entity-detail__group"
            >
              <h3>
                <span>
                  {t(`dock.detail.group.${group.code}.${props.viewCode}`, {
                    defaultValue: t(`dock.detail.group.${group.code}`),
                  })}
                </span>
                {props.viewCode === "processes" && (
                  <code>{t(`dock.detail.source.process.${group.code}`)}</code>
                )}
              </h3>
              {group.code === "context" &&
                relatedActivityRelation !== undefined &&
                props.relatedActivity !== undefined && (
                  <button
                    type="button"
                    className="entity-detail__inline-activity"
                    aria-label={t("dock.detail.relatedActivity.open")}
                    onClick={() =>
                      props.onPatch({
                        view: relatedActivityRelation.view,
                        entity: relatedActivityRelation.entity,
                        dock: "row",
                        preset: null,
                        q: null,
                        sort: null,
                        order: null,
                        at: relatedActivityRelation.snapshot_ts_us,
                      })
                    }
                  >
                    <span className="entity-detail__inline-activity-heading">
                      <strong>{t("dock.detail.relatedActivity.title")}</strong>
                      <span>{t("dock.detail.relatedActivity.open")}</span>
                    </span>
                    {relatedMeta.length > 0 && (
                      <code>{relatedMeta.join(" / ")}</code>
                    )}
                    {typeof relatedQuery === "string" &&
                      relatedQuery !== "" && <pre>{relatedQuery}</pre>}
                    <span className="entity-detail__inline-activity-state">
                      {typeof relatedState === "string" && (
                        <span>{relatedState}</span>
                      )}
                      {typeof relatedWait === "string" && (
                        <span>{relatedWait}</span>
                      )}
                      {typeof relatedDuration === "number" && (
                        <span>{formatDurationUs(relatedDuration)}</span>
                      )}
                    </span>
                  </button>
                )}
              <div className="entity-detail__measurements">
                {group.fields.map((field) => {
                  const subgroup =
                    props.viewCode === "processes"
                      ? processSubgroupBefore[field.code]
                      : undefined;
                  return (
                    <Fragment key={field.code}>
                      {subgroup && (
                        <h4 className="entity-detail__subgroup">
                          {t(`dock.detail.subgroup.${subgroup}`)}
                        </h4>
                      )}
                      {renderField(field)}
                    </Fragment>
                  );
                })}
              </div>
            </section>
          ))}
      </div>
    </div>
  );
}

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
    <div className="entity-detail__history-scroll">
      <table data-detail-history className="entity-detail__history-table">
        <thead>
          <tr>
            <th>{t("dock.detail.observedAt")}</th>
            {data.columns.map((column) => (
              <th
                key={column}
                title={colDesc(t, props.viewCode, column) ?? undefined}
              >
                {colLabel(t, props.viewCode, column)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {data.snapshots.map((snapshot) => (
            <tr key={snapshot.ts_us} data-testid="history-snapshot">
              <td className="entity-detail__history-time">
                {formatIntervalTime(Number(snapshot.ts_us))}
              </td>
              {data.columns.map((column, i) => {
                const value = snapshot.values[i] ?? null;
                const spec = props.columns.get(column);
                return (
                  <td
                    key={column}
                    className={
                      value === null
                        ? "entity-detail__history-missing"
                        : undefined
                    }
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
          data-testid="history-load-more"
          disabled={props.loadingMore === true}
          onClick={props.onLoadMore}
          className="entity-detail__load-more"
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

function relationLabelKey(relation: string): string {
  switch (relation) {
    case "activity_process":
      return "dock.relation.pid";
    case "statement_plan":
      return "dock.relation.query";
    case "table_index":
    case "table_vacuum":
      return "dock.relation.table";
    case "index_table":
      return "dock.relation.index";
    default:
      return relation.includes("time") || relation.includes("temporal")
        ? "dock.relation.nearTime"
        : "dock.relation.object";
  }
}

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

function mergeHistoryQuality(
  existing: EntityHistoryResponse["quality"] | null,
  incoming: EntityHistoryResponse["quality"],
): EntityHistoryResponse["quality"] {
  if (existing === null) return incoming;
  const gaps = new Map(
    existing.gaps.map((gap) => [`${gap.from_us}:${gap.to_us}`, gap]),
  );
  for (const gap of incoming.gaps) {
    gaps.set(`${gap.from_us}:${gap.to_us}`, gap);
  }
  return {
    status:
      existing.status === "complete" && incoming.status === "complete"
        ? "complete"
        : "partial",
    gaps: [...gaps.values()],
    gated: [...new Set([...existing.gated, ...incoming.gated])].sort(),
  };
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
  const [historyQuality, setHistoryQuality] = useState<
    EntityHistoryResponse["quality"] | null
  >(null);
  const [extraSnapshots, setExtraSnapshots] = useState<EntitySnapshotDto[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const entityKey = `${props.state.view}:${props.state.entity ?? ""}`;
  const entity = useEntityPoint({
    view: props.state.view,
    entity: props.state.entity ?? "",
    at: props.at,
    includeRelated: true,
  });
  const activityRelation = entity.data?.related.find(
    (relation) =>
      props.state.view === "processes" &&
      relation.view === "activity" &&
      relation.relation === "activity_process",
  );
  const relatedActivity = useEntityPoint({
    view: "activity",
    entity: activityRelation?.entity ?? "",
    at: activityRelation?.snapshot_ts_us ?? props.at,
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
    setHistoryQuality(null);
    setExtraSnapshots([]);
    setNextCursor(null);
  }, [historyKey]);
  useEffect(() => {
    const data = history.data;
    if (data === undefined) return;
    setHistoryQuality((previous) =>
      mergeHistoryQuality(previous, data.quality),
    );
    if (historyCursor !== null) {
      setExtraSnapshots((previous) =>
        uniqueSnapshots(previous, data.snapshots),
      );
      setNextCursor(data.page.next ?? null);
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
          quality: historyQuality ?? visibleHistory.quality,
        };

  const [tokenCopied, setTokenCopied] = useState(false);
  const copyToken = () => {
    if (props.state.entity === null) return;
    void navigator.clipboard.writeText(props.state.entity);
    setTokenCopied(true);
    setTimeout(() => setTokenCopied(false), 1700);
  };
  const rawEvidence =
    data === undefined
      ? null
      : {
          endpoint: `/v1/entity/${viewCode}/${encodeURIComponent(data.entity)}`,
          technical_entity_id: data.entity,
          snapshot_ts_us: data.snapshot_ts_us,
          quality: data.quality,
          response: data,
        };

  return (
    <div className="entity-detail">
      <div data-testid="dock-entity-heading" className="entity-detail__heading">
        <span className="entity-detail__view">{t(`tabs.${viewCode}`)}</span>
        {heading !== null && (
          <>
            {" · "}
            <span className="entity-detail__title">{heading}</span>
          </>
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
      {data !== undefined && (
        <>
          <div
            role="tablist"
            aria-label={t("dock.detail.tabs")}
            className="entity-detail__tabs"
          >
            {ENTITY_DETAIL_TABS.map((tab) => (
              <button
                key={tab}
                type="button"
                role="tab"
                data-detail-tab-trigger={tab}
                aria-selected={detailTab === tab}
                onClick={() => setDetailTab(tab)}
                className="entity-detail__tab"
              >
                {t(`dock.detail.${tab}`)}
              </button>
            ))}
          </div>
          <div
            role="tabpanel"
            data-detail-tab={detailTab}
            className="entity-detail__panel"
          >
            {detailTab === "summary" && (
              <div className="entity-detail__summary">
                <EntityPointView
                  data={data}
                  columns={columnSpecs}
                  viewCode={viewCode}
                  relatedActivity={relatedActivity.data}
                  onPatch={props.onPatch}
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
                  <>
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
                  </>
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
                  const labelKey = relationLabelKey(relation.relation);
                  return (
                    <button
                      key={`${relation.view}:${relation.entity}`}
                      type="button"
                      onClick={() =>
                        props.onPatch({
                          view: relation.view,
                          entity: relation.entity,
                          dock: "row",
                          at: relation.snapshot_ts_us,
                          ...(relation.view === props.state.view
                            ? {}
                            : {
                                preset: null,
                                q: null,
                                sort: null,
                                order: null,
                              }),
                        })
                      }
                      className="entity-detail__relation"
                    >
                      <span className="entity-detail__relation-target">
                        {t(`tabs.${relation.view}`, {
                          defaultValue: relation.view,
                        })}
                        <span aria-hidden="true"> →</span>
                      </span>
                      <strong className="entity-detail__relation-basis">
                        {t(labelKey)}
                      </strong>
                    </button>
                  );
                })}
              </div>
            )}
            {detailTab === "raw" && (
              <div className="entity-detail__raw">
                <div role="note" className="entity-detail__raw-note">
                  {t("dock.detail.rawProjectedOnly")}
                </div>
                <div className="entity-detail__raw-actions">
                  <button
                    type="button"
                    onClick={copyToken}
                    className="entity-detail__raw-copy"
                  >
                    {tokenCopied
                      ? t("dock.row.tokenCopied")
                      : t("dock.row.copyTechnicalId")}
                  </button>
                </div>
                <pre data-raw-evidence className="entity-detail__raw-data">
                  {JSON.stringify(rawEvidence, null, 2)}
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
  const rowWorkspace = active === "row" && !props.mobile;
  return (
    <aside
      data-dock={active}
      style={dockStyle(props.mobile, active)}
      className={`dock-overlay${props.mobile ? " dock-overlay--mobile" : ""}${rowWorkspace ? " dock-overlay--row-workspace" : ""}`}
      aria-label={t("dock.title")}
    >
      {!rowWorkspace && (
        <div
          role="tablist"
          className="dock-overlay__rail"
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
              className="dock-overlay__rail-tab"
            >
              {t(`dock.tabs.${kind}`)}
            </button>
          ))}
          <button
            type="button"
            aria-label={t("dock.close")}
            onClick={props.onClose}
            className="dock-overlay__close"
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
      )}
      {rowWorkspace && (
        <button
          type="button"
          aria-label={t("dock.close")}
          onClick={props.onClose}
          className="dock-overlay__close dock-overlay__workspace-close"
        >
          ×
        </button>
      )}
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
