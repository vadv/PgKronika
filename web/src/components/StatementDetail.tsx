import { useTranslation } from "react-i18next";
import { colLabel } from "../api/codes";
import { useEntityHistory, useEntityPoint } from "../api/entity";
import type {
  ColumnSpec,
  EntityHistoryResponse,
  EntityPointResponse,
  FrameValue,
  ViewSpec,
} from "../api/types";
import {
  formatByUnit,
  formatCompactNumber,
  formatTimestampUs,
  shortIdToken,
} from "../design/format";
import "./StatementDetail.css";

const MAX_HISTORY_SECONDS = 21_600;
const HISTORY_COLUMNS = [
  "total",
  "calls",
  "mean",
  "blks_read",
  "wal_bytes",
  "temp_written",
] as const;

const TEMPORAL_LANES = [
  { code: "impact", metrics: ["total", "calls"] },
  { code: "latency", metrics: ["mean", "ms_per_row"] },
  { code: "buffers", metrics: ["blks_read", "hit_pct"] },
  { code: "writes", metrics: ["wal_bytes", "temp_written"] },
] as const;

export interface StatementDetailProps {
  view: ViewSpec;
  entity: string;
  at: string;
  span: number;
  onClose: () => void;
  onOpenEntity: (view: string, entity: string, at: string) => void;
}

function boundedFrom(at: string, span: number): string {
  try {
    const seconds = Math.min(Math.max(span, 1), MAX_HISTORY_SECONDS);
    return (BigInt(at) - BigInt(seconds) * 1_000_000n).toString();
  } catch {
    return at;
  }
}

function pointFields(data: EntityPointResponse | undefined) {
  return new Map(
    (data?.fields ?? []).map((field) => [field.code, field.value]),
  );
}

function columnsByCode(view: ViewSpec) {
  return new Map(view.columns.map((column) => [column.code, column]));
}

function numericValue(value: FrameValue | undefined): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (
    typeof value === "string" &&
    value.trim() !== "" &&
    Number.isFinite(Number(value))
  ) {
    return Number(value);
  }
  return null;
}

function displayValue(
  value: FrameValue | undefined,
  column: ColumnSpec | undefined,
  missing: string,
): string {
  if (value === null || value === undefined) return missing;
  if (column?.type === "timestamp") return formatTimestampUs(String(value));
  if (typeof value === "number") return formatByUnit(value, column?.unit);
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value);
}

function numericSeries(
  history: EntityHistoryResponse | undefined,
  code: string,
): (number | null)[] {
  if (history === undefined) return [];
  const index = history.columns.indexOf(code);
  if (index < 0) return [];
  return history.snapshots.map((snapshot) =>
    numericValue(snapshot.values[index] ?? null),
  );
}

function lineSegments(values: (number | null)[]): string[] {
  const finite = values.filter((value): value is number => value !== null);
  if (finite.length === 0) return [];
  const min = Math.min(...finite);
  const max = Math.max(...finite);
  const width = Math.max(1, values.length - 1);
  const segments: string[] = [];
  let current: string[] = [];
  const flush = () => {
    if (current.length > 0) segments.push(current.join(" "));
    current = [];
  };
  values.forEach((value, index) => {
    if (value === null) {
      flush();
      return;
    }
    const x = (index / width) * 100;
    const ratio = max === min ? 0.5 : (value - min) / (max - min);
    current.push(`${x.toFixed(2)},${(54 - ratio * 44).toFixed(2)}`);
  });
  flush();
  return segments;
}

function firstFinite(values: (number | null)[]): number | null {
  return values.find((value): value is number => value !== null) ?? null;
}

function formatDelta(
  current: FrameValue | undefined,
  baseline: number | null,
  missing: string,
): string {
  const latest = numericValue(current);
  if (latest === null || baseline === null) return missing;
  if (baseline === 0) return latest === 0 ? "0%" : ">100%";
  const delta = ((latest - baseline) / Math.abs(baseline)) * 100;
  const precision = Math.abs(delta) >= 100 ? 0 : 1;
  return `${delta > 0 ? "+" : ""}${delta.toFixed(precision)}%`;
}

function snapshotTime(timestampUs: string): string {
  const milliseconds = Number(timestampUs) / 1_000;
  if (!Number.isFinite(milliseconds)) return timestampUs;
  return new Date(milliseconds).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function TimelineLane(props: {
  view: ViewSpec;
  code: string;
  metrics: readonly string[];
  history: EntityHistoryResponse | undefined;
  columns: Map<string, ColumnSpec>;
  fields: Map<string, FrameValue>;
}) {
  const { t } = useTranslation();
  const missing = t("statementDetail.notCollected");
  const title = t(`statementDetail.temporal.${props.code}`);
  const metrics = props.metrics.filter(
    (code) => props.fields.has(code) || props.columns.has(code),
  );
  return (
    <div
      data-testid="statement-temporal-lane"
      data-lane={props.code}
      className="statement-detail__temporal-lane"
    >
      <div className="statement-detail__lane-label">
        <span className="statement-detail__lane-title">{title}</span>
        <div className="statement-detail__lane-values">
          {metrics.map((code, index) => (
            <span key={code} data-series={index + 1}>
              <small>{colLabel(t, props.view.code, code)}</small>
              <strong>
                {displayValue(
                  props.fields.get(code),
                  props.columns.get(code),
                  missing,
                )}
              </strong>
            </span>
          ))}
        </div>
      </div>
      <svg
        className="statement-detail__lane-chart"
        viewBox="0 0 100 64"
        preserveAspectRatio="none"
        role="img"
        aria-label={t("statementDetail.timelineAria", { metric: title })}
      >
        <line
          className="statement-detail__lane-baseline"
          x1="0"
          y1="54"
          x2="100"
          y2="54"
        />
        {metrics.flatMap((code, metricIndex) =>
          lineSegments(numericSeries(props.history, code)).map(
            (points, segmentIndex) => (
              <polyline
                key={`${code}:${segmentIndex}`}
                data-series={metricIndex + 1}
                className="statement-detail__lane-trace"
                points={points}
              />
            ),
          ),
        )}
        <line
          className="statement-detail__lane-cursor"
          x1="99"
          y1="0"
          x2="99"
          y2="64"
        />
      </svg>
    </div>
  );
}

function HistoryMatrix(props: {
  view: ViewSpec;
  history: EntityHistoryResponse | undefined;
  columns: Map<string, ColumnSpec>;
  fields: Map<string, FrameValue>;
  codes: readonly string[];
}) {
  const { t } = useTranslation();
  const missing = t("statementDetail.notCollected");
  return (
    <table
      data-testid="statement-history-matrix"
      className="statement-detail__history-matrix"
    >
      <thead>
        <tr>
          <th>{t("statementDetail.matrix.metric")}</th>
          <th>{t("statementDetail.matrix.current")}</th>
          <th>{t("statementDetail.matrix.delta")}</th>
          <th>{t("statementDetail.matrix.baseline")}</th>
        </tr>
      </thead>
      <tbody>
        {props.codes.map((code) => {
          const baseline = firstFinite(numericSeries(props.history, code));
          return (
            <tr key={code}>
              <td>{colLabel(t, props.view.code, code)}</td>
              <td>
                {displayValue(
                  props.fields.get(code),
                  props.columns.get(code),
                  missing,
                )}
              </td>
              <td>{formatDelta(props.fields.get(code), baseline, missing)}</td>
              <td>
                {displayValue(baseline, props.columns.get(code), missing)}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function Fact(props: {
  view: ViewSpec;
  code: string;
  fields: Map<string, FrameValue>;
  columns: Map<string, ColumnSpec>;
}) {
  const { t } = useTranslation();
  return (
    <div className="statement-detail__fact" data-field={props.code}>
      <span>{colLabel(t, props.view.code, props.code)}</span>
      <strong>
        {displayValue(
          props.fields.get(props.code),
          props.columns.get(props.code),
          t("statementDetail.notCollected"),
        )}
      </strong>
    </div>
  );
}

export function StatementDetail(props: StatementDetailProps) {
  const { t } = useTranslation();
  const columns = columnsByCode(props.view);
  const historyColumns = HISTORY_COLUMNS.filter((code) => columns.has(code));
  const historyEnabled =
    props.view.capabilities.history && historyColumns.length > 0;
  const point = useEntityPoint({
    view: props.view.code,
    entity: props.entity,
    at: props.at,
    includeRelated: true,
  });
  const history = useEntityHistory({
    view: props.view.code,
    entity: props.entity,
    from: boundedFrom(props.at, props.span),
    to: props.at,
    columns: historyColumns,
    limit: 96,
    enabled: historyEnabled,
  });
  const data = point.data;
  const fields = pointFields(data);
  const queryId = fields.get("queryid");
  const query = fields.get("query");
  const database = fields.get("database");
  const role = fields.get("user");
  const calls = numericValue(fields.get("calls"));
  const mean = numericValue(fields.get("mean"));
  const total = numericValue(fields.get("total"));
  const timeShare = numericValue(fields.get("time_pct"));
  const quality =
    history.data?.quality.status ?? data?.quality.status ?? "complete";
  const relatedPlans =
    data?.related.filter((item) => item.view === "plans") ?? [];
  const relatedSamples =
    data?.related.filter((item) =>
      ["activity", "processes"].includes(item.view),
    ) ?? [];
  const idLabel =
    typeof queryId === "string" || typeof queryId === "number"
      ? shortIdToken(String(queryId))
      : t("statementDetail.unknownQuery");
  const impactEquation = `${
    calls === null
      ? t("statementDetail.notCollected")
      : formatCompactNumber(calls)
  } × ${
    mean === null
      ? t("statementDetail.notCollected")
      : formatByUnit(mean, columns.get("mean")?.unit ?? "ms")
  }`;
  const impactTotal =
    total === null
      ? t("statementDetail.notCollected")
      : formatByUnit(total, columns.get("total")?.unit ?? "duration_ms");

  return (
    <section
      data-testid="statement-detail"
      className="statement-detail"
      aria-label={t("statementDetail.title", { id: idLabel })}
    >
      <header
        data-testid="statement-entity-strip"
        className="statement-detail__entity-strip"
      >
        <div className="statement-detail__breadcrumb">
          <span>{t("tabs.statements")}</span>
          <span aria-hidden="true">›</span>
          <strong>{idLabel}</strong>
        </div>
        <div className="statement-detail__identity">
          <span>
            <small>{t("statementDetail.database")}</small>
            <strong>
              {database === null || database === undefined
                ? "—"
                : String(database)}
            </strong>
          </span>
          <span>
            <small>{t("statementDetail.role")}</small>
            <strong>
              {role === null || role === undefined ? "—" : String(role)}
            </strong>
          </span>
          <span>
            <small>{t("statementDetail.queryId")}</small>
            <strong title={queryId === undefined ? undefined : String(queryId)}>
              {idLabel}
            </strong>
          </span>
        </div>
        {data !== undefined && (
          <span className="statement-detail__snapshot">
            <small>{t("statementDetail.snapshot")}</small>
            <strong>{snapshotTime(data.snapshot_ts_us)}</strong>
          </span>
        )}
        <span
          className="statement-detail__collection-state"
          data-status={quality}
        >
          {t(`statementDetail.collection.${quality}`, {
            defaultValue: quality,
          })}
        </span>
        {timeShare !== null && timeShare >= 10 && (
          <span
            className="statement-detail__impact-signal"
            data-severity={timeShare >= 20 ? "critical" : "warning"}
          >
            {t("statementDetail.impactSignal", {
              value: formatByUnit(timeShare, "percent"),
            })}
          </span>
        )}
        <button
          type="button"
          className="statement-detail__close"
          aria-label={t("statementDetail.close")}
          onClick={props.onClose}
        >
          {t("statementDetail.closeShort")}
        </button>
      </header>

      {point.isPending && (
        <div className="statement-detail__pending" role="status">
          {t("table.loading")}
        </div>
      )}
      {point.isError && (
        <div className="statement-detail__error" role="alert">
          {t("dock.row.error")}
        </div>
      )}
      {data !== undefined && (
        <>
          <div
            data-testid="statement-temporal-field"
            className="statement-detail__temporal-field"
          >
            {TEMPORAL_LANES.map((lane) => (
              <TimelineLane
                key={lane.code}
                view={props.view}
                code={lane.code}
                metrics={lane.metrics}
                history={history.data}
                columns={columns}
                fields={fields}
              />
            ))}
            {historyEnabled && history.isPending && (
              <div className="statement-detail__history-state" role="status">
                {t("statementDetail.loadingHistory")}
              </div>
            )}
            {historyEnabled && history.isError && (
              <div className="statement-detail__history-state" role="alert">
                {t("statementDetail.historyUnavailable")}
              </div>
            )}
            <div className="statement-detail__related-lane">
              <span>{t("statementDetail.relatedLane")}</span>
              <div>
                {data.related.map((relation) => (
                  <button
                    key={`${relation.view}:${relation.entity}`}
                    type="button"
                    aria-label={t("statementDetail.openRelatedLane", {
                      view: t(`tabs.${relation.view}`, {
                        defaultValue: relation.view,
                      }),
                    })}
                    onClick={() =>
                      props.onOpenEntity(
                        relation.view,
                        relation.entity,
                        relation.snapshot_ts_us,
                      )
                    }
                  >
                    {t(`tabs.${relation.view}`, {
                      defaultValue: relation.view,
                    })}
                  </button>
                ))}
                {data.related.length === 0 && (
                  <em>{t("statementDetail.noRelated")}</em>
                )}
              </div>
            </div>
          </div>

          <div className="statement-detail__analysis-grid">
            <section
              data-testid="statement-impact-center"
              className="statement-detail__analysis statement-detail__impact-center"
            >
              <h2>{t("statementDetail.impactCenter")}</h2>
              <div className="statement-detail__equation">
                <span>{impactEquation}</span>
                <strong>
                  {t("statementDetail.totalImpact", { value: impactTotal })}
                </strong>
              </div>
              <div className="statement-detail__facts">
                {[
                  "time_pct",
                  "plan_time_pct",
                  "rows",
                  "hit_pct",
                  "blks_read",
                  "wal_bytes",
                  "temp_written",
                ].map((code) => (
                  <Fact
                    key={code}
                    view={props.view}
                    code={code}
                    fields={fields}
                    columns={columns}
                  />
                ))}
              </div>
              <div className="statement-detail__sql">
                <span>{t("statementDetail.sql")}</span>
                <code>
                  {typeof query === "string" && query.trim() !== ""
                    ? query
                    : t("statementDetail.sqlUnavailable")}
                </code>
              </div>
            </section>

            <section className="statement-detail__analysis">
              <h2>{t("statementDetail.metricHistory")}</h2>
              <HistoryMatrix
                view={props.view}
                history={history.data}
                columns={columns}
                fields={fields}
                codes={historyColumns}
              />
            </section>

            <section
              data-testid="statement-related-evidence"
              className="statement-detail__analysis statement-detail__related"
            >
              <h2>{t("statementDetail.relatedEvidence")}</h2>
              <div className="statement-detail__related-group">
                <h3>{t("statementDetail.recordedPlans")}</h3>
                {relatedPlans.map((relation) => (
                  <button
                    key={`${relation.view}:${relation.entity}:card`}
                    type="button"
                    aria-label={t("statementDetail.openRelated", {
                      view: t(`tabs.${relation.view}`),
                    })}
                    onClick={() =>
                      props.onOpenEntity(
                        relation.view,
                        relation.entity,
                        relation.snapshot_ts_us,
                      )
                    }
                  >
                    <span>
                      <strong>{t("statementDetail.planObserved")}</strong>
                      <small>
                        {t("statementDetail.relatedAt", {
                          time: snapshotTime(relation.snapshot_ts_us),
                        })}
                      </small>
                    </span>
                    <strong>{t("statementDetail.openEvidence")}</strong>
                  </button>
                ))}
                {relatedPlans.length === 0 && (
                  <p>{t("statementDetail.noPlans")}</p>
                )}
              </div>
              <div className="statement-detail__related-group">
                <h3>{t("statementDetail.observedSamples")}</h3>
                {relatedSamples.map((relation) => (
                  <button
                    key={`${relation.view}:${relation.entity}:sample`}
                    type="button"
                    aria-label={t("statementDetail.openRelated", {
                      view: t(`tabs.${relation.view}`),
                    })}
                    onClick={() =>
                      props.onOpenEntity(
                        relation.view,
                        relation.entity,
                        relation.snapshot_ts_us,
                      )
                    }
                  >
                    <span>
                      <strong>
                        {t(`tabs.${relation.view}`, {
                          defaultValue: relation.view,
                        })}
                      </strong>
                      <small>
                        {t("statementDetail.relatedAt", {
                          time: snapshotTime(relation.snapshot_ts_us),
                        })}
                      </small>
                    </span>
                    <strong>{t("statementDetail.openEvidence")}</strong>
                  </button>
                ))}
                {relatedSamples.length === 0 && (
                  <p>{t("statementDetail.noSamples")}</p>
                )}
              </div>
            </section>
          </div>
        </>
      )}
    </section>
  );
}
