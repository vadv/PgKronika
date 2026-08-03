import { useTranslation } from "react-i18next";
import { metricDesc, metricLabel } from "../api/codes";
import { useHeatmap } from "../api/heatmap";
import { useTimelineSpine } from "../api/spine";
import type { ContextResponse, SpineSeries, ViewSpec } from "../api/types";
import { formatByUnit } from "../design/format";
import { TableView } from "./TableView";
import { TipFormula, TipRow, Tooltip } from "./Tooltip";

export interface OsWorkspaceProps {
  view: ViewSpec;
  at: string;
  span: number;
  from: string;
  to: string;
  metric: string;
  preset: string | null;
  q: string | null;
  sort: string | null;
  order: "asc" | "desc" | null;
  entity: string | null;
  matched: number | null;
  mobile: boolean;
  context: ContextResponse | undefined;
  onMetricChange: (metric: string) => void;
  onSort: (sort: string | null, order: "asc" | "desc" | null) => void;
  onSelectRow: (entity: string) => void;
  onMatched?: (matched: number) => void;
}

export function osMetricForPreset(preset: string | null): "cpu" | "io" {
  return preset === "disk_io" ? "io" : "cpu";
}

function latest(values: (number | null)[]): number | null {
  for (let index = values.length - 1; index >= 0; index -= 1) {
    const value = values[index];
    if (value !== null && value !== undefined && Number.isFinite(value)) {
      return value;
    }
  }
  return null;
}

function maximum(values: (number | null)[]): number | null {
  const observed = values.filter(
    (value): value is number => value !== null && Number.isFinite(value),
  );
  return observed.length === 0 ? null : Math.max(...observed);
}

function HostSignalLane(props: {
  series: SpineSeries | undefined;
  code: "loadPerCpu" | "psiIo";
  headline: number | null;
}) {
  const { t } = useTranslation();
  const values = props.series?.values ?? [];
  const observed = values.filter(
    (value): value is number => value !== null && Number.isFinite(value),
  );
  const scaleMax =
    props.series?.unit === "percent"
      ? 100
      : Math.max(1, ...observed.map((value) => Math.abs(value)));
  return (
    <section
      className="os-host-signal"
      aria-label={t(`hostEvidence.${props.code}`)}
    >
      <span className="os-host-signal__label">
        {t(`hostEvidence.${props.code}`)}
      </span>
      <strong className="os-host-signal__value">
        {props.headline === null
          ? "—"
          : formatByUnit(props.headline, props.series?.unit)}
      </strong>
      <ol
        className="os-host-signal__buckets"
        aria-label={t("host.matrix.hostBuckets", { count: values.length })}
      >
        {values.map((value, index) => (
          <li key={index}>
            <meter
              min={0}
              max={scaleMax}
              value={value === null ? 0 : Math.max(0, value)}
              data-missing={value === null ? "true" : undefined}
              title={
                value === null
                  ? t("spine.missing")
                  : formatByUnit(value, props.series?.unit)
              }
            />
          </li>
        ))}
      </ol>
    </section>
  );
}

export function OsWorkspace(props: OsWorkspaceProps) {
  const { t } = useTranslation();
  const buckets = props.mobile ? 48 : 96;
  const spine = useTimelineSpine({
    from: props.from,
    to: props.to,
    buckets: 24,
  });
  const heatmap = useHeatmap({
    view: "processes",
    metric: props.metric,
    from: props.from,
    to: props.to,
    buckets,
    top: 64,
  });
  const load = spine.data?.series.find(
    (series) => series.code === "load_per_cpu",
  );
  const psi = spine.data?.series.find(
    (series) => series.code === "psi_io_some",
  );
  const quality = spine.data?.quality;
  const metrics = props.view.metrics.filter(
    (metric) =>
      metric.availability === "available" &&
      (metric.code === "cpu" || metric.code === "io"),
  );
  const retained = heatmap.data?.rows.length ?? 0;
  const metricText = metricLabel(t, props.view.code, props.metric);
  const host = props.context?.host;
  const scopeFiltered = props.q !== null && props.q.trim() !== "";

  return (
    <section className="os-workspace" data-testid="os-workspace">
      <aside className="os-host-rail" data-testid="host-pressure-evidence">
        <div className="os-host-rail__heading">
          <strong>
            {t(`hostEvidence.lens.${props.preset ?? "pressure"}`)}
          </strong>
          <span className="os-host-rail__scope">
            {t("hostEvidence.scope.host")}
          </span>
        </div>

        {spine.isPending ? (
          <span className="os-host-rail__state" aria-busy="true">
            {t("table.loading")}
          </span>
        ) : spine.isError ? (
          <div className="os-host-rail__state os-host-rail__state--error">
            <span role="alert">{t("host.matrix.hostLoadError")}</span>
            <button type="button" onClick={() => void spine.refetch()}>
              {t("table.retry")}
            </button>
          </div>
        ) : spine.data.series.length === 0 ? (
          <span className="os-host-rail__state">
            {t("host.matrix.hostUnavailable")}
          </span>
        ) : (
          <div className="os-host-rail__signals">
            <HostSignalLane
              series={load}
              code="loadPerCpu"
              headline={load === undefined ? null : latest(load.values)}
            />
            <HostSignalLane
              series={psi}
              code="psiIo"
              headline={psi === undefined ? null : maximum(psi.values)}
            />
          </div>
        )}

        <div
          className="os-host-rail__guard"
          data-testid="host-scope-guard"
          data-cpus={host?.logical_cpu_count ?? "unknown"}
        >
          {t("hostEvidence.scopeGuard", {
            cpus: host?.logical_cpu_count ?? "—",
            kernel: host?.kernel_version ?? "—",
          })}
        </div>
        <div className="os-host-rail__note">
          {t(`hostEvidence.note.${props.preset ?? "pressure"}`)}
        </div>
        <div
          className="os-host-rail__quality"
          data-testid="host-quality"
          data-limited={quality?.resource_limited.length ?? 0}
          data-status={quality?.status ?? "pending"}
        >
          {quality === undefined
            ? t("host.matrix.qualityPending")
            : t("host.matrix.quality", {
                status: quality.status,
                snapshots: quality.snapshots,
                gaps: quality.gaps.length,
                gated: quality.gated.length,
                limited: quality.resource_limited.length,
              })}
        </div>
      </aside>

      <div className="os-workspace__controls">
        <div
          className="os-workspace__metrics"
          role="group"
          aria-label={t("host.matrix.metricGroup")}
        >
          {metrics.map((metric) => {
            const label = metricLabel(t, props.view.code, metric.code);
            const description = metricDesc(t, props.view.code, metric.code);
            return (
              <Tooltip
                key={metric.code}
                content={
                  <span className="os-workspace__metric-tip">
                    {description !== null && <span>{description}</span>}
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
                  className="os-workspace__metric"
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
          className="os-workspace__population"
          data-testid="os-frame-population"
          data-matched={props.matched ?? undefined}
        >
          {scopeFiltered
            ? t("host.matrix.filteredFrame", {
                matched: props.matched ?? "—",
              })
            : t("host.matrix.frame", { matched: props.matched ?? "—" })}
        </span>
        <span
          className="os-workspace__population"
          data-testid="os-heatmap-population"
          data-retained={retained}
        >
          {t("host.matrix.globalHeatmap", { retained })}
        </span>
        <span className="os-workspace__scope-note">
          {t(
            scopeFiltered
              ? "host.matrix.independentFilteredScopes"
              : "host.matrix.independentScopes",
          )}
        </span>
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
          kind: "processes",
          evidenceMode: "process_intervals",
          data: heatmap.data,
          pending: heatmap.isPending,
          error: heatmap.isError,
          metricLabel: metricText,
          cursorUs: props.at,
          baselineUs: null,
          onRetry: () => void heatmap.refetch(),
        }}
      />
    </section>
  );
}
