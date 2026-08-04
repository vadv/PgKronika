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

export const PRESSURE_CHART_WIDTH = 240;
export const PRESSURE_CHART_HEIGHT = 32;

/** Each contiguous run of samples becomes its own filled polygon — a missing
 * bucket stays a visible gap in the shape, never bridged into a false
 * continuous trend. */
export function pressureAreaSegments(
  values: (number | null)[],
  max: number,
): string[] {
  const step =
    values.length > 1 ? PRESSURE_CHART_WIDTH / (values.length - 1) : 0;
  const segments: string[] = [];
  let points: string[] = [];
  let firstX = 0;
  let lastX = 0;
  const flush = () => {
    if (points.length >= 2) {
      segments.push(
        `${points.join(" ")} L${lastX},${PRESSURE_CHART_HEIGHT} L${firstX},${PRESSURE_CHART_HEIGHT} Z`,
      );
    }
    points = [];
  };
  values.forEach((value, index) => {
    if (value === null || !Number.isFinite(value)) {
      flush();
      return;
    }
    const x = index * step;
    const ratio = Math.max(0, Math.min(value, max)) / max;
    const y = PRESSURE_CHART_HEIGHT - ratio * PRESSURE_CHART_HEIGHT;
    if (points.length === 0) firstX = x;
    lastX = x;
    points.push(`${points.length === 0 ? "M" : "L"}${x},${y}`);
  });
  flush();
  return segments;
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
  const segments = pressureAreaSegments(values, scaleMax);
  const tone = props.code === "psiIo" ? "var(--sev-warn)" : "var(--accent)";
  const step =
    values.length > 1 ? PRESSURE_CHART_WIDTH / (values.length - 1) : 0;
  const hitWidth = step > 0 ? step : PRESSURE_CHART_WIDTH;
  const peak = maximum(values);
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
      <svg
        className="os-host-signal__chart"
        viewBox={`0 0 ${PRESSURE_CHART_WIDTH} ${PRESSURE_CHART_HEIGHT}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={t("host.matrix.hostTrend", {
          latest:
            props.headline === null
              ? "—"
              : formatByUnit(props.headline, props.series?.unit),
          peak: peak === null ? "—" : formatByUnit(peak, props.series?.unit),
          count: values.length,
        })}
      >
        {segments.map((d, index) => (
          <path
            key={index}
            d={d}
            fill={tone}
            fillOpacity={0.32}
            stroke={tone}
            strokeWidth={1}
            vectorEffect="non-scaling-stroke"
          />
        ))}
        {values.map((value, index) => (
          <rect
            key={index}
            x={step > 0 ? index * step - hitWidth / 2 : 0}
            y={0}
            width={hitWidth}
            height={PRESSURE_CHART_HEIGHT}
            fill="transparent"
            data-missing={value === null ? "true" : undefined}
          >
            <title>
              {value === null
                ? t("data.noSnapshotInterval")
                : formatByUnit(value, props.series?.unit)}
            </title>
          </rect>
        ))}
      </svg>
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
  const metrics = props.view.metrics.filter(
    (metric) =>
      metric.availability === "available" &&
      (metric.code === "cpu" || metric.code === "io"),
  );
  const metricText = metricLabel(t, props.view.code, props.metric);
  const host = props.context?.host;
  const scopeFiltered = props.q !== null && props.q.trim() !== "";

  return (
    <section className="os-workspace" data-testid="os-workspace">
      <aside
        className="os-host-rail"
        data-testid="host-pressure-evidence"
        data-view="processes"
      >
        <div className="os-host-rail__heading">
          <strong>
            {t(`hostEvidence.lens.${props.preset ?? "pressure"}`)}
          </strong>
          <span
            className="os-host-rail__facts"
            data-testid="host-facts"
            data-cpus={host?.logical_cpu_count ?? "unknown"}
          >
            {t("hostEvidence.facts", {
              cpus: host?.logical_cpu_count ?? "—",
              kernel: host?.kernel_version ?? "—",
            })}
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
