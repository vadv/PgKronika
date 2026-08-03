import { useTranslation } from "react-i18next";
import { metricDesc, metricLabel } from "../api/codes";
import { useHeatmap } from "../api/heatmap";
import type { ViewSpec } from "../api/types";
import { TableView } from "./TableView";
import { TipFormula, TipRow, Tooltip } from "./Tooltip";

export interface StatementsWorkspaceProps {
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
  onMatched?: (matched: number) => void;
}

export function StatementsWorkspace(props: StatementsWorkspaceProps) {
  const { t } = useTranslation();
  const buckets = props.mobile ? 48 : 96;
  const heatmap = useHeatmap({
    view: "statements",
    metric: props.metric,
    from: props.from,
    to: props.to,
    buckets,
    top: 64,
  });
  const metrics = props.view.metrics.filter(
    (metric) => metric.availability === "available",
  );
  const retained = heatmap.data?.rows.length ?? 0;
  const missing =
    props.matched === null ? null : Math.max(0, props.matched - retained);
  const quality = heatmap.data?.quality;
  const metricText = metricLabel(t, props.view.code, props.metric);

  return (
    <section
      className="statements-workspace"
      data-testid="statements-workspace"
    >
      <div
        className="statements-workspace__controls"
        role="group"
        aria-label={t("statements.matrix.controls")}
      >
        <span className="statements-workspace__title">
          {t("statements.matrix.title")}
        </span>
        <span className="statements-workspace__lens">
          {t("statements.matrix.lens", { lens: props.preset ?? "—" })}
        </span>
        <div
          className="statements-workspace__metrics"
          role="group"
          aria-label={t("statements.matrix.metricGroup")}
        >
          {metrics.map((metric) => {
            const label = metricLabel(t, props.view.code, metric.code);
            const desc = metricDesc(t, props.view.code, metric.code);
            return (
              <Tooltip
                key={metric.code}
                content={
                  <span className="statements-workspace__metric-tip">
                    {desc !== null && <span>{desc}</span>}
                    <TipRow
                      label={t("tooltip.code")}
                      value={`${metric.code} · ${metric.unit}`}
                      mono
                    />
                    <TipFormula
                      label={t("tooltip.formula")}
                      value={metric.formula}
                    />
                  </span>
                }
              >
                <button
                  type="button"
                  className="statements-workspace__metric"
                  aria-pressed={props.metric === metric.code}
                  onClick={() => props.onMetricChange(metric.code)}
                >
                  {label}
                </button>
              </Tooltip>
            );
          })}
        </div>
        <span
          className="statements-workspace__count"
          data-retained={retained}
          data-matched={props.matched ?? undefined}
        >
          {t("statements.matrix.retained", {
            retained,
            matched: props.matched ?? "—",
          })}
        </span>
        {missing !== null && missing > 0 && (
          <span className="statements-workspace__missing">
            {t("statements.matrix.missing", { count: missing })}
          </span>
        )}
        {quality !== undefined && (
          <Tooltip
            content={
              <span className="statements-workspace__quality-tip">
                <TipRow
                  label={t("statements.matrix.quality")}
                  value={`${quality.status} · ${quality.snapshots}`}
                />
                <TipRow
                  label={t("statements.matrix.gaps")}
                  value={String(quality.gaps.length)}
                />
                <TipRow
                  label={t("statements.matrix.provenanceLabel")}
                  value={t("statements.matrix.provenance")}
                />
              </span>
            }
          >
            <span
              className="statements-workspace__quality"
              data-quality={quality.status}
            >
              {t("statements.matrix.qualitySummary", {
                status: quality.status,
                snapshots: quality.snapshots,
              })}
            </span>
          </Tooltip>
        )}
      </div>
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
