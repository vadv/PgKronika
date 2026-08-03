import { useTranslation } from "react-i18next";
import { metricDesc, metricLabel } from "../api/codes";
import { useFrame } from "../api/frame";
import { useHeatmap } from "../api/heatmap";
import type {
  FrameColumnDto,
  FrameRowDto,
  FrameValue,
  ViewSpec,
} from "../api/types";
import { formatByUnit } from "../design/format";
import { TableView } from "./TableView";

export interface ActivityWorkspaceProps {
  view: ViewSpec;
  at: string;
  span: number;
  from: string;
  to: string;
  metric: string;
  baselineUs: string | null;
  preset: string | null;
  q: string | null;
  sort: string | null;
  order: "asc" | "desc" | null;
  entity: string | null;
  matched: number | null;
  mobile: boolean;
  onMetricChange: (metric: string) => void;
  onSort: (sort: string | null, order: "asc" | "desc" | null) => void;
  onSelectRow: (entity: string) => void;
  onOpenEntity: (view: string, entity: string) => void;
  onMatched?: (matched: number) => void;
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

function ActivityLockEvidence(props: {
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

  return (
    <section
      className="activity-lock-evidence"
      data-testid="activity-lock-evidence"
      data-provenance="edge_only"
      aria-label={t("activity.matrix.locks.title")}
    >
      <div className="activity-lock-evidence__context">
        <strong>{t("activity.matrix.locks.title")}</strong>
        <span>{t("activity.matrix.locks.note")}</span>
      </div>
      <div className="activity-lock-evidence__edges">
        {frame.isPending && (
          <span className="activity-lock-evidence__state">
            {t("table.loading")}
          </span>
        )}
        {frame.isError && (
          <span className="activity-lock-evidence__state activity-lock-evidence__state--error">
            {t("table.error")}
          </span>
        )}
        {frame.data?.rows.length === 0 && (
          <span className="activity-lock-evidence__state">
            {t("activity.locks.empty")}
          </span>
        )}
        {frame.data?.rows.map((row) => {
          const pid = evidenceText(cell(frame.data.columns, row, "pid"));
          const blockedBy = evidenceText(
            cell(frame.data.columns, row, "blocked_by"),
          );
          const target = evidenceText(cell(frame.data.columns, row, "target"));
          const waitAge = cell(frame.data.columns, row, "wait_age_us");
          return (
            <button
              key={row.entity}
              type="button"
              className="activity-lock-evidence__edge"
              onClick={() => props.onOpenEntity("locks", row.entity)}
              aria-label={`${pid} → ${blockedBy} · ${target}`}
            >
              <span className="activity-lock-evidence__pids">
                {pid} → {blockedBy}
              </span>
              <span className="activity-lock-evidence__target">{target}</span>
              <span className="activity-lock-evidence__age">
                {typeof waitAge === "number"
                  ? formatByUnit(waitAge, "us")
                  : evidenceText(waitAge)}
              </span>
            </button>
          );
        })}
      </div>
      <span className="activity-lock-evidence__badge">
        {t("activity.matrix.locks.provenance")}
      </span>
    </section>
  );
}

export function ActivityWorkspace(props: ActivityWorkspaceProps) {
  const { t } = useTranslation();
  const buckets = props.mobile ? 48 : 96;
  const heatmap = useHeatmap({
    view: "activity",
    metric: props.metric,
    from: props.from,
    to: props.to,
    buckets,
    top: 64,
  });
  const metrics = props.view.metrics.filter(
    (metric) => metric.availability === "available",
  );
  const quality = heatmap.data?.quality;
  const retained = heatmap.data?.rows.length ?? 0;
  const metricText = metricLabel(t, props.view.code, props.metric);
  const processJoin = props.view.joins.find(
    (join) =>
      join.left === "activity" &&
      join.right === "process" &&
      join.kind === "best_effort",
  );

  return (
    <section
      className="activity-workspace"
      data-testid="activity-workspace"
      data-lens={props.preset ?? "overview"}
    >
      <div className="activity-workspace__evidence">
        <div
          className="activity-workspace__snapshot"
          data-testid="activity-point-evidence"
        >
          <strong>{t("activity.snapshotBadge")}</strong>
          <span>{t("activity.pointEvidence")}</span>
        </div>
        <div
          className="activity-workspace__metrics"
          role="group"
          aria-label={t("activity.matrix.metricGroup")}
        >
          {metrics.map((metric) => (
            <button
              key={metric.code}
              type="button"
              className="activity-workspace__metric"
              aria-pressed={props.metric === metric.code}
              title={metricDesc(t, props.view.code, metric.code) ?? undefined}
              onClick={() => props.onMetricChange(metric.code)}
            >
              {metricLabel(t, props.view.code, metric.code)}
            </button>
          ))}
        </div>
        <span
          className="activity-workspace__provenance"
          data-testid="activity-process-provenance"
          title={t("activity.processEvidence")}
        >
          {t("activity.matrix.processLink")} ·{" "}
          {processJoin?.kind ?? "unavailable"}
          {processJoin === undefined ? "" : ` · ${processJoin.provenance}`}
        </span>
        <span
          className="activity-workspace__coverage"
          data-quality={quality?.status ?? "loading"}
        >
          {t("activity.matrix.coverage", {
            retained,
            matched: props.matched ?? "—",
            snapshots: quality?.snapshots ?? "—",
          })}
        </span>
      </div>
      {props.preset === "waits_locks" && (
        <ActivityLockEvidence
          at={props.at}
          span={props.span}
          onOpenEntity={props.onOpenEntity}
        />
      )}
      <TableView
        view={props.view}
        at={props.at}
        span={props.span}
        preset={props.preset}
        q={props.q}
        sort={props.sort}
        order={props.order}
        entity={props.entity}
        onSort={props.onSort}
        onSelectRow={props.onSelectRow}
        onMatched={props.onMatched}
        timeMatrix={{
          kind: "activity",
          data: heatmap.data,
          pending: heatmap.isPending,
          error: heatmap.isError,
          metricLabel: metricText,
          cursorUs: props.at,
          baselineUs: props.baselineUs,
          onRetry: () => void heatmap.refetch(),
        }}
      />
    </section>
  );
}
