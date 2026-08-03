import { useTranslation } from "react-i18next";
import { useFrame } from "../api/frame";
import type {
  FrameColumnDto,
  FrameRowDto,
  FrameValue,
  ViewSpec,
} from "../api/types";
import { formatByUnit, shortIdToken } from "../design/format";

interface WorkloadEvidencePanelProps {
  view: ViewSpec;
  preset: string | null;
  at: string;
  span: number;
  onOpenEntity: (view: string, entity: string) => void;
}

function cell(
  columns: FrameColumnDto[],
  row: FrameRowDto,
  code: string,
): FrameValue | null {
  const index = columns.findIndex((column) => column.code === code);
  return index < 0 ? null : (row.cells[index] ?? null);
}

function evidenceText(value: FrameValue | null): string {
  if (value === null) return "—";
  if (typeof value === "boolean") return value ? "true" : "false";
  return String(value);
}

function panelStyle(): React.CSSProperties {
  return {
    minWidth: 0,
    height: "100%",
    overflow: "hidden",
    padding: "var(--space-2)",
    color: "var(--fg)",
    background: "var(--bg-raised)",
    border: "1px solid var(--border)",
    borderRadius: "var(--radius-md)",
    fontFamily: "var(--ui-font)",
  };
}

function PanelHeading(props: { title: string; provenance: string }) {
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
        {props.title}
      </strong>
      <span
        style={{
          marginInlineStart: "auto",
          color: "var(--fg-dim)",
          fontFamily: "var(--mono-font)",
          fontSize: "var(--text-xs)",
          whiteSpace: "nowrap",
        }}
      >
        {props.provenance}
      </span>
    </div>
  );
}

function LockLanes(props: {
  at: string;
  span: number;
  onOpenEntity: (view: string, entity: string) => void;
}) {
  const { t } = useTranslation();
  const frame = useFrame({
    view: "locks",
    at: props.at,
    span: props.span,
    preset: "tree",
    limit: 3,
  });
  if (frame.isPending) {
    return <span style={{ color: "var(--fg-dim)" }}>{t("table.loading")}</span>;
  }
  if (frame.isError) {
    return (
      <span style={{ color: "var(--sev-warn-fg)" }}>{t("table.error")}</span>
    );
  }
  if (frame.data.rows.length === 0) {
    return (
      <span style={{ color: "var(--fg-dim)" }}>
        {t("activity.locks.empty")}
      </span>
    );
  }
  return (
    <div
      data-testid="activity-lock-lanes"
      style={{ display: "grid", gap: "var(--space-1)" }}
    >
      {frame.data.rows.map((row) => {
        const pid = evidenceText(cell(frame.data.columns, row, "pid"));
        const blockedBy = evidenceText(
          cell(frame.data.columns, row, "blocked_by"),
        );
        const waitAge = cell(frame.data.columns, row, "wait_age_us");
        const target = evidenceText(cell(frame.data.columns, row, "target"));
        return (
          <button
            key={row.entity}
            type="button"
            onClick={() => props.onOpenEntity("locks", row.entity)}
            aria-label={`${pid} → ${blockedBy} · ${target}`}
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
              {pid} → {blockedBy} · {target}
            </span>
            <span style={{ color: "var(--fg-dim)" }}>
              {typeof waitAge === "number"
                ? formatByUnit(waitAge, "us")
                : evidenceText(waitAge)}
            </span>
          </button>
        );
      })}
    </div>
  );
}

function ActivityPanel(props: WorkloadEvidencePanelProps) {
  const { t } = useTranslation();
  const processLens = props.preset === "cpu" || props.preset === "disk_io";
  return (
    <aside
      data-testid="workload-evidence-panel"
      data-view="activity"
      style={panelStyle()}
    >
      <PanelHeading
        title={t(`activity.lens.${props.preset ?? "overview"}`)}
        provenance={t("activity.snapshotBadge")}
      />
      <div
        data-testid="activity-point-evidence"
        style={{
          color: "var(--fg-dim)",
          fontSize: "var(--text-xs)",
          marginBlockEnd: "var(--space-2)",
        }}
      >
        {t("activity.pointEvidence")}
      </div>
      {props.preset === "waits_locks" ? (
        <LockLanes
          at={props.at}
          span={props.span}
          onOpenEntity={props.onOpenEntity}
        />
      ) : processLens ? (
        <div
          data-testid="activity-process-evidence"
          style={{ fontSize: "var(--text-xs)" }}
        >
          {t("activity.processEvidence")}
        </div>
      ) : props.preset === "sampling" ? (
        <div style={{ fontSize: "var(--text-xs)" }}>
          {t("activity.samplingEvidence")}
        </div>
      ) : (
        <div style={{ fontSize: "var(--text-xs)" }}>
          {t(`activity.lensNote.${props.preset ?? "overview"}`)}
        </div>
      )}
    </aside>
  );
}

function timestampLabel(value: FrameValue | null): string {
  if (typeof value !== "number" && typeof value !== "string") return "—";
  const micros = Number(value);
  if (!Number.isFinite(micros)) return String(value);
  return new Date(micros / 1000).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

function PlanTimeline(props: {
  at: string;
  span: number;
  onOpenEntity: (view: string, entity: string) => void;
}) {
  const { t } = useTranslation();
  const frame = useFrame({
    view: "plans",
    at: props.at,
    span: props.span,
    preset: "change_timeline",
    limit: 3,
  });
  if (frame.isPending) {
    return <span style={{ color: "var(--fg-dim)" }}>{t("table.loading")}</span>;
  }
  if (frame.isError) {
    return (
      <span style={{ color: "var(--sev-warn-fg)" }}>{t("table.error")}</span>
    );
  }
  if (frame.data.rows.length === 0) {
    return (
      <span style={{ color: "var(--fg-dim)" }}>
        {t("plans.timeline.empty")}
      </span>
    );
  }
  return (
    <div
      data-testid="plan-version-lanes"
      style={{ display: "grid", gap: "var(--space-1)" }}
    >
      {frame.data.rows.map((row) => {
        const planId = evidenceText(cell(frame.data.columns, row, "planid"));
        const queryId = evidenceText(cell(frame.data.columns, row, "queryid"));
        const first = timestampLabel(
          cell(frame.data.columns, row, "first_call"),
        );
        const last = timestampLabel(cell(frame.data.columns, row, "last_call"));
        return (
          <button
            key={row.entity}
            type="button"
            onClick={() => props.onOpenEntity("plans", row.entity)}
            aria-label={`${planId} · ${queryId} · ${first}–${last}`}
            style={{
              display: "grid",
              gridTemplateColumns: "auto minmax(0, 1fr)",
              alignItems: "center",
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
            <span>{shortIdToken(planId)}</span>
            <span
              style={{
                overflow: "hidden",
                color: "var(--fg-dim)",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              q {shortIdToken(queryId)} · {first}–{last}
            </span>
          </button>
        );
      })}
    </div>
  );
}

function PlansPanel(props: WorkloadEvidencePanelProps) {
  const { t } = useTranslation();
  const joins = props.view.joins.filter(
    (join) =>
      join.kind === "best_effort" &&
      join.provenance.includes("queryid") &&
      join.provenance.includes("attribution"),
  );
  return (
    <aside
      data-testid="workload-evidence-panel"
      data-view="plans"
      style={panelStyle()}
    >
      <PanelHeading
        title={t(`plans.lens.${props.preset ?? "time"}`)}
        provenance={t("plans.attributionBadge")}
      />
      <div
        data-testid="plan-attribution-provenance"
        style={{
          display: "flex",
          gap: "var(--space-1)",
          marginBlockEnd: "var(--space-1)",
          overflow: "hidden",
          color: "var(--fg-dim)",
          fontFamily: "var(--mono-font)",
          fontSize: "var(--text-xs)",
          whiteSpace: "nowrap",
        }}
      >
        {joins.length === 0 ? (
          <span>{t("plans.attributionUnavailable")}</span>
        ) : (
          joins.map((join) => (
            <span
              key={join.provenance}
              title={`${join.provenance} · ${join.fields.join(", ")}`}
              style={{
                padding: "0 var(--space-1)",
                background: "var(--bg)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius-sm)",
              }}
            >
              {join.provenance.startsWith("ossc_")
                ? "OSSC · queryid/dbid/userid"
                : "vadv · ss-queryid/dbid/userid"}
              <span
                style={{
                  position: "absolute",
                  width: "1px",
                  height: "1px",
                  padding: 0,
                  margin: "-1px",
                  overflow: "hidden",
                  clip: "rect(0, 0, 0, 0)",
                  whiteSpace: "nowrap",
                  border: 0,
                }}
              >
                {join.provenance}
              </span>
            </span>
          ))
        )}
      </div>
      {props.preset === "change_timeline" ? (
        <PlanTimeline
          at={props.at}
          span={props.span}
          onOpenEntity={props.onOpenEntity}
        />
      ) : (
        <div style={{ fontSize: "var(--text-xs)" }}>
          {t(`plans.lensNote.${props.preset ?? "time"}`)}
        </div>
      )}
    </aside>
  );
}

export function WorkloadEvidencePanel(props: WorkloadEvidencePanelProps) {
  if (props.view.code === "activity") return <ActivityPanel {...props} />;
  if (props.view.code === "plans") return <PlansPanel {...props} />;
  return null;
}
