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
import { formatByUnit, shortIdToken } from "../design/format";
import { TableView } from "./TableView";

export interface PlansWorkspaceProps {
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

function shortTimestamp(value: FrameValue | null): string {
  if (typeof value !== "number" && typeof value !== "string") return "—";
  const micros = Number(value);
  if (!Number.isFinite(micros)) return String(value);
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(micros / 1000));
}

function compactMetric(
  value: FrameValue | null,
  unit: string | null | undefined,
): string {
  if (typeof value !== "number") return evidenceText(value);
  return formatByUnit(value, unit);
}

function PlanChangeEvidence(props: {
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
  const data = frame.data;

  return (
    <section
      className="plan-change-evidence"
      data-testid="plan-change-evidence"
      aria-label={t("plans.changes.title")}
    >
      <div className="plan-change-evidence__context">
        <strong>{t("plans.changes.title")}</strong>
        <span>{t("plans.changes.note")}</span>
      </div>
      <div className="plan-change-evidence__records">
        {frame.isPending && (
          <span className="plan-change-evidence__state">
            {t("table.loading")}
          </span>
        )}
        {frame.isError && (
          <button
            type="button"
            className="plan-change-evidence__state plan-change-evidence__retry"
            onClick={() => void frame.refetch()}
          >
            {t("table.error")} · {t("table.retry")}
          </button>
        )}
        {data !== undefined && data.rows.length === 0 && (
          <span className="plan-change-evidence__state">
            {t("plans.timeline.empty")}
          </span>
        )}
        {data?.rows.slice(0, 3).map((row) => {
          const planId = evidenceText(cell(data.columns, row, "planid"));
          const queryId = evidenceText(cell(data.columns, row, "queryid"));
          const first = shortTimestamp(cell(data.columns, row, "first_call"));
          const last = shortTimestamp(cell(data.columns, row, "last_call"));
          const callsColumn = data.columns.find(
            (column) => column.code === "calls",
          );
          const meanColumn = data.columns.find(
            (column) => column.code === "mean",
          );
          const calls = compactMetric(
            cell(data.columns, row, "calls"),
            callsColumn?.unit,
          );
          const mean = compactMetric(
            cell(data.columns, row, "mean"),
            meanColumn?.unit ?? "ms",
          );
          return (
            <button
              key={row.entity}
              type="button"
              className="plan-change-evidence__record"
              data-testid="plan-observation-envelope"
              onClick={() => props.onOpenEntity("plans", row.entity)}
              aria-label={t("plans.changes.open", {
                plan: planId,
                query: queryId,
                first,
                last,
              })}
            >
              <span className="plan-change-evidence__identity">
                <strong>p {shortIdToken(planId)}</strong>
                <span>q {shortIdToken(queryId)}</span>
              </span>
              <span className="plan-change-evidence__window">
                <span>{t("plans.changes.first", { time: first })}</span>
                <span>{t("plans.changes.last", { time: last })}</span>
              </span>
              <span className="plan-change-evidence__metrics">
                {t("plans.changes.metrics", { calls, mean })}
              </span>
            </button>
          );
        })}
      </div>
      <span className="plan-change-evidence__badge">
        {t("plans.changes.provenance")}
      </span>
    </section>
  );
}

export function PlansWorkspace(props: PlansWorkspaceProps) {
  const { t } = useTranslation();
  const buckets = props.mobile ? 48 : 96;
  const heatmap = useHeatmap({
    view: "plans",
    metric: props.metric,
    from: props.from,
    to: props.to,
    buckets,
    top: 64,
  });
  const metrics = props.view.metrics.filter(
    (metric) => metric.availability === "available",
  );
  const joins = props.view.joins.filter(
    (join) =>
      join.left === "plans" &&
      join.right === "statements" &&
      join.kind === "best_effort" &&
      join.provenance.includes("attribution"),
  );
  const quality = heatmap.data?.quality;
  const retained = heatmap.data?.rows.length ?? 0;
  const metricText = metricLabel(t, props.view.code, props.metric);
  const lens = props.preset ?? "regression";
  const filteredFrame = (props.q?.trim().length ?? 0) > 0;
  const showChangeEvidence =
    lens === "regression" || lens === "change_timeline";

  return (
    <section
      className="plans-workspace"
      data-testid="plans-workspace"
      data-lens={lens}
    >
      <div className="plans-workspace__evidence">
        <div className="plans-workspace__context">
          <strong>{t(`plans.lens.${lens}`)}</strong>
          <span
            data-testid={
              lens === "regression" ? "plans-regression-boundary" : undefined
            }
            title={t(`plans.lensNote.${lens}`)}
          >
            {t(`plans.lensNote.${lens}`)}
          </span>
        </div>
        <div
          className="plans-workspace__metrics"
          role="group"
          aria-label={t("plans.matrix.metricGroup")}
        >
          {metrics.map((metric) => (
            <button
              key={metric.code}
              type="button"
              className="plans-workspace__metric"
              aria-pressed={props.metric === metric.code}
              title={metricDesc(t, props.view.code, metric.code) ?? undefined}
              onClick={() => props.onMetricChange(metric.code)}
            >
              {metricLabel(t, props.view.code, metric.code)}
            </button>
          ))}
        </div>
        <div
          className="plans-workspace__provenance"
          data-testid="plans-attribution-provenance"
          aria-label={t("plans.matrix.attribution")}
        >
          {joins.length === 0 ? (
            <span>{t("plans.attributionUnavailable")}</span>
          ) : (
            joins.map((join) => {
              const ossc = join.provenance.startsWith("ossc_");
              return (
                <span
                  key={join.provenance}
                  className="plans-workspace__fork"
                  title={t("plans.matrix.attributionHint")}
                >
                  {ossc
                    ? t("plans.attribution.ossc")
                    : t("plans.attribution.vadv")}
                </span>
              );
            })
          )}
        </div>
        <span className="plans-workspace__coverage">
          {t(
            filteredFrame
              ? "plans.matrix.coverageFiltered"
              : "plans.matrix.coverage",
            {
              retained,
              matched: props.matched ?? "—",
              snapshots: quality?.snapshots ?? "—",
            },
          )}
        </span>
      </div>
      {showChangeEvidence && (
        <PlanChangeEvidence
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
          kind: "plans",
          evidenceMode: "plan_intervals",
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
