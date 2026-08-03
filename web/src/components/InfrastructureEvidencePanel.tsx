import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { useFrame } from "../api/frame";
import type {
  ContextResponse,
  FrameColumnDto,
  FrameRowDto,
  FrameValue,
  ViewSpec,
} from "../api/types";
import {
  formatByUnit,
  formatDurationUs,
  formatTimestampUs,
} from "../design/format";

interface InfrastructureEvidencePanelProps {
  view: ViewSpec;
  preset: string | null;
  at: string;
  span: number;
  from: string;
  to: string;
  context: ContextResponse | undefined;
  onOpenEntity: (view: string, entity: string) => void;
}

const panel: CSSProperties = {
  minWidth: 0,
  height: "100%",
  overflow: "hidden",
  padding: "var(--space-2)",
  color: "var(--fg)",
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-md)",
  fontFamily: "var(--ui-font)",
  fontSize: "var(--text-xs)",
};

const dim: CSSProperties = { color: "var(--fg-dim)" };

function heading(title: string, provenance: string) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "baseline",
        gap: "var(--space-2)",
        marginBlockEnd: "var(--space-1)",
      }}
    >
      <strong
        style={{
          minWidth: 0,
          overflow: "hidden",
          fontSize: "var(--text-sm)",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {title}
      </strong>
      <span
        title={provenance}
        style={{
          ...dim,
          marginInlineStart: "auto",
          overflow: "hidden",
          fontFamily: "var(--mono-font)",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {provenance}
      </span>
    </div>
  );
}

function cell(
  columns: FrameColumnDto[],
  row: FrameRowDto,
  code: string,
): FrameValue | null {
  const index = columns.findIndex((column) => column.code === code);
  return index < 0 ? null : (row.cells[index] ?? null);
}

function TablesEvidence(props: InfrastructureEvidencePanelProps) {
  const { t } = useTranslation();
  const frame = useFrame({
    view: "vacuum",
    at: props.at,
    span: props.span,
    preset: "progress",
    limit: 3,
  });
  const snapshot = frame.data?.snapshot_ts_us;
  const snapshotMatchesCursor = snapshot === props.at;
  const snapshotDeltaUs =
    snapshot === undefined
      ? null
      : Math.abs(Number(props.at) - Number(snapshot));
  return (
    <aside
      data-testid="infrastructure-evidence-panel"
      data-view="tables"
      data-snapshot-ts={snapshot}
      data-snapshot-match={
        snapshot === undefined ? undefined : snapshotMatchesCursor
      }
      data-snapshot-delta-us={snapshotDeltaUs ?? undefined}
      data-snapshot-provenance="independent_nearest_vacuum_snapshot"
      style={panel}
    >
      {heading(
        t("tableEvidence.activeVacuum"),
        t("tableEvidence.independentSnapshot"),
      )}
      <div style={{ ...dim, marginBlockEnd: "var(--space-1)" }}>
        {snapshot === undefined
          ? t("tableEvidence.snapshotPending")
          : t("tableEvidence.snapshotDisclosure", {
              snapshot: formatTimestampUs(snapshot),
              delta: formatDurationUs(snapshotDeltaUs ?? 0),
            })}
      </div>
      {frame.isPending ? (
        <span style={dim}>{t("table.loading")}</span>
      ) : frame.isError ? (
        <span style={{ color: "var(--sev-warn-fg)" }}>{t("table.error")}</span>
      ) : frame.data.rows.length === 0 ? (
        <span style={dim}>{t("tableEvidence.noActiveVacuum")}</span>
      ) : (
        <div
          data-testid="table-vacuum-lanes"
          style={{ display: "grid", gap: "var(--space-1)" }}
        >
          {frame.data.rows.map((row) => {
            const relation = cell(frame.data.columns, row, "relation");
            const phase = cell(frame.data.columns, row, "phase");
            const progress = cell(frame.data.columns, row, "progress");
            const label = String(relation ?? row.label);
            return (
              <button
                key={row.entity}
                type="button"
                aria-label={`${label} · ${String(phase ?? "—")}`}
                onClick={() => props.onOpenEntity("vacuum", row.entity)}
                style={{
                  display: "grid",
                  gridTemplateColumns: "minmax(0, 1fr) auto",
                  gap: "var(--space-2)",
                  minWidth: 0,
                  padding: "var(--space-1) var(--space-2)",
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
                <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                  {label} · {String(phase ?? "—")}
                </span>
                <span style={dim}>
                  {typeof progress === "number"
                    ? formatByUnit(progress, "ratio")
                    : "—"}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </aside>
  );
}

function IndexEvidence(props: InfrastructureEvidencePanelProps) {
  const { t } = useTranslation();
  const provenance =
    props.view.joins.find(
      (join) => join.left === "indexes" && join.right === "tables",
    )?.provenance ?? "unavailable";
  return (
    <aside
      data-testid="infrastructure-evidence-panel"
      data-view="indexes"
      style={panel}
    >
      {heading(
        t("indexEvidence.tableContext"),
        t("relation.kind.temporal.label"),
      )}
      <div
        data-testid="index-table-provenance"
        style={{ fontFamily: "var(--mono-font)" }}
      >
        {provenance}
      </div>
      <div style={{ ...dim, marginBlockStart: "var(--space-1)" }}>
        {t("indexEvidence.sameSnapshotOnly")}
      </div>
      <div style={{ marginBlockStart: "var(--space-2)" }}>
        {t(`indexEvidence.note.${props.preset ?? "usage"}`)}
      </div>
    </aside>
  );
}

function VacuumEvidence(props: InfrastructureEvidencePanelProps) {
  const { t } = useTranslation();
  const temporal = props.view.joins.find(
    (join) => join.left === "vacuum" && join.right === "tables",
  );
  const pre17 = props.view.columns.find(
    (column) => column.code === "dead_tuples",
  );
  const pg17 = props.view.columns.find(
    (column) => column.code === "dead_item_ids",
  );
  const pgVersionNum = props.context?.instance.pg_version_num;
  const pgMajor =
    pgVersionNum == null ? null : Math.floor(pgVersionNum / 10_000);
  const status = (column: typeof pre17, applicable: boolean): string => {
    if (pgMajor === null) return "unknown";
    if (!applicable) return "not_applicable";
    return column?.availability ?? "gated";
  };
  const pre17Status = status(pre17, pgMajor !== null && pgMajor <= 16);
  const pg17Status = status(pg17, pgMajor !== null && pgMajor >= 17);
  return (
    <aside
      data-testid="infrastructure-evidence-panel"
      data-view="vacuum"
      style={panel}
    >
      {heading(
        t(`vacuumEvidence.lens.${props.preset ?? "progress"}`),
        temporal?.provenance ?? "unavailable",
      )}
      <div data-testid="vacuum-lifetime-warning" style={dim}>
        {t("vacuumEvidence.lifetimeWarning")}
      </div>
      <div
        style={{
          display: "flex",
          gap: "var(--space-1)",
          marginBlockStart: "var(--space-2)",
        }}
      >
        <span
          data-testid="vacuum-pre17-generation"
          data-status={pre17Status}
          style={{ padding: "var(--space-1)", background: "var(--bg)" }}
        >
          PG10–16 · {t(`status.${pre17Status}`, { defaultValue: pre17Status })}
        </span>
        <span
          data-testid="vacuum-pg17-generation"
          data-status={pg17Status}
          style={{ padding: "var(--space-1)", background: "var(--bg)" }}
        >
          PG17+ · {t(`status.${pg17Status}`, { defaultValue: pg17Status })}
        </span>
      </div>
      <div style={{ marginBlockStart: "var(--space-2)" }}>
        {t("vacuumEvidence.tableContext")}
      </div>
    </aside>
  );
}

export function InfrastructureEvidencePanel(
  props: InfrastructureEvidencePanelProps,
) {
  if (props.view.code === "tables") return <TablesEvidence {...props} />;
  if (props.view.code === "indexes") return <IndexEvidence {...props} />;
  if (props.view.code === "vacuum") return <VacuumEvidence {...props} />;
  return null;
}
