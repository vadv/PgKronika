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
import { formatByUnit, formatTimestampUs } from "../design/format";
import "./DataMaintenanceDetail.css";

const MAX_HISTORY_SECONDS = 21_600;
const MAX_HISTORY_COLUMNS = 6;

const historyPriority: Record<string, readonly string[]> = {
  tables: [
    "seq_scan",
    "idx_scan",
    "dead_pct",
    "io_hit_pct",
    "modified_since_analyze",
    "inserted_since_vacuum",
  ],
  indexes: ["scans", "rows_per_scan", "io_hit_pct", "size", "last_idx_scan"],
  vacuum: ["progress", "dead_tuples", "dead_item_ids", "dead_tuple_bytes"],
};

const identityPriority: Record<string, readonly string[]> = {
  tables: ["size", "dead_pct"],
  indexes: ["table", "size", "scans"],
  vacuum: ["relation", "pid", "phase", "progress"],
};

const temporalLanes: Record<
  string,
  readonly { code: string; metrics: readonly string[] }[]
> = {
  tables: [
    { code: "access", metrics: ["idx_scan", "seq_scan"] },
    {
      code: "churn",
      metrics: ["modified_since_analyze", "inserted_since_vacuum"],
    },
    { code: "cache", metrics: ["io_hit_pct"] },
  ],
  indexes: [
    { code: "usage", metrics: ["scans", "rows_per_scan"] },
    { code: "cache", metrics: ["io_hit_pct"] },
    { code: "footprint", metrics: ["size"] },
  ],
  vacuum: [
    { code: "progress", metrics: ["progress"] },
    { code: "deadItems", metrics: ["dead_tuples", "dead_item_ids"] },
    { code: "deadBytes", metrics: ["dead_tuple_bytes"] },
  ],
};

const keyStats: Record<string, readonly string[]> = {
  tables: ["io_hit_pct", "xid_age"],
  indexes: ["io_hit_pct", "scans"],
  vacuum: ["progress", "dead_tuples"],
};

const analysisPriority: Record<
  string,
  { primary: readonly string[]; state: readonly string[] }
> = {
  tables: {
    primary: ["seq_scan", "idx_scan", "seq_scan_pct", "io_hit_pct", "size"],
    state: [
      "dead_pct",
      "dead_tuples",
      "modified_since_analyze",
      "inserted_since_vacuum",
      "last_autovacuum",
      "autovacuum_age_seconds",
      "autoanalyze_age_seconds",
      "xid_age",
      "mxid_age",
    ],
  },
  indexes: {
    primary: ["scans", "rows_per_scan", "io_hit_pct"],
    state: ["size", "last_idx_scan"],
  },
  vacuum: {
    primary: ["progress", "phase", "is_autovacuum"],
    state: [
      "dead_tuples",
      "dead_item_ids",
      "dead_tuple_bytes",
      "pid",
      "relation",
    ],
  },
};

export interface DataMaintenanceDetailProps {
  view: ViewSpec;
  entity: string;
  at: string;
  span: number;
  onClose: () => void;
  onOpenEntity: (view: string, entity: string, at: string) => void;
}

function boundedFrom(at: string, span: number): string {
  try {
    const width =
      BigInt(Math.min(Math.max(span, 1), MAX_HISTORY_SECONDS)) * 1_000_000n;
    return (BigInt(at) - width).toString();
  } catch {
    return at;
  }
}

export function detailHistoryColumns(view: ViewSpec): string[] {
  const candidates = new Map(
    view.columns
      .filter(
        (column) =>
          !column.lazy &&
          column.availability === "available" &&
          column.type !== "text" &&
          column.type !== "bool" &&
          column.type !== "timestamp",
      )
      .map((column) => [column.code, column]),
  );
  const preferred = (historyPriority[view.code] ?? []).filter((code) =>
    candidates.has(code),
  );
  const remaining = [...candidates.keys()].filter(
    (code) => !preferred.includes(code),
  );
  return [...preferred, ...remaining].slice(0, MAX_HISTORY_COLUMNS);
}

function pointFields(data: EntityPointResponse | undefined) {
  return new Map(
    (data?.fields ?? []).map((field) => [field.code, field.value]),
  );
}

function columnsByCode(view: ViewSpec) {
  return new Map(view.columns.map((column) => [column.code, column]));
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
  const columnIndex = history.columns.findIndex((column) => column === code);
  if (columnIndex < 0) return [];
  return history.snapshots.map((snapshot) => {
    const value = snapshot.values[columnIndex] ?? null;
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (
      typeof value === "string" &&
      value.trim() !== "" &&
      Number.isFinite(Number(value))
    ) {
      return Number(value);
    }
    return null;
  });
}

function polylinePoints(values: (number | null)[]): string {
  const finite = values.filter((value): value is number => value !== null);
  if (finite.length === 0) return "";
  const min = Math.min(...finite);
  const max = Math.max(...finite);
  const width = Math.max(1, values.length - 1);
  return values
    .map((value, index) => {
      if (value === null) return null;
      const x = (index / width) * 100;
      const ratio = max === min ? 0.5 : (value - min) / (max - min);
      const y = 56 - ratio * 48;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .filter((point): point is string => point !== null)
    .join(" ");
}

function firstFinite(values: (number | null)[]): number | null {
  return values.find((value): value is number => value !== null) ?? null;
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

function formatWindowDelta(
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

function formatSnapshotTime(timestampUs: string): string {
  const milliseconds = Number(timestampUs) / 1_000;
  if (!Number.isFinite(milliseconds)) return timestampUs;
  return new Date(milliseconds).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function TemporalLane(props: {
  view: ViewSpec;
  lane: { code: string; metrics: readonly string[] };
  history: EntityHistoryResponse | undefined;
  columns: Map<string, ColumnSpec>;
  fields: Map<string, FrameValue>;
}) {
  const { t } = useTranslation();
  const missing = t("maintenanceDetail.notCollected");
  const metrics = props.lane.metrics.filter(
    (code) => props.columns.has(code) || props.fields.has(code),
  );
  const warning = /churn|dead/i.test(props.lane.code);
  const laneLabel = t(
    `maintenanceDetail.temporal.${props.view.code}.${props.lane.code}`,
  );
  return (
    <div
      data-testid="maintenance-temporal-lane"
      data-lane={props.lane.code}
      className={`maintenance-detail__temporal-lane${
        warning ? " maintenance-detail__temporal-lane--warning" : ""
      }`}
    >
      <div className="maintenance-detail__lane-label">
        <span className="maintenance-detail__lane-title">{laneLabel}</span>
        <div className="maintenance-detail__lane-values">
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
        className="maintenance-detail__lane-chart"
        viewBox="0 0 100 64"
        preserveAspectRatio="none"
        role="img"
        aria-label={t("maintenanceDetail.timelineAria", {
          metric: laneLabel,
        })}
      >
        <line
          className="maintenance-detail__lane-baseline"
          x1="0"
          y1="56"
          x2="100"
          y2="56"
        />
        {metrics.map((code, index) => {
          const points = polylinePoints(numericSeries(props.history, code));
          return points === "" ? null : (
            <polyline
              key={code}
              data-testid="maintenance-lane-trace"
              data-series={index + 1}
              className="maintenance-detail__lane-trace"
              points={points}
            />
          );
        })}
        <line
          className="maintenance-detail__lane-cursor"
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
  codes: readonly string[];
  history: EntityHistoryResponse | undefined;
  columns: Map<string, ColumnSpec>;
  fields: Map<string, FrameValue>;
}) {
  const { t } = useTranslation();
  const missing = t("maintenanceDetail.notCollected");
  return (
    <table
      data-testid="maintenance-history-matrix"
      className="maintenance-detail__history-matrix"
    >
      <thead>
        <tr>
          <th>{t("maintenanceDetail.matrix.metric")}</th>
          <th>{t("maintenanceDetail.matrix.current")}</th>
          <th>{t("maintenanceDetail.matrix.delta")}</th>
          <th>{t("maintenanceDetail.matrix.baseline")}</th>
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
              <td>
                {formatWindowDelta(props.fields.get(code), baseline, missing)}
              </td>
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

function KeyStat(props: {
  view: ViewSpec;
  code: string;
  value: FrameValue | undefined;
  column: ColumnSpec | undefined;
}) {
  const { t } = useTranslation();
  return (
    <div
      data-testid="maintenance-key-stat"
      className="maintenance-detail__key-stat"
    >
      <span>{colLabel(t, props.view.code, props.code)}</span>
      <strong>
        {displayValue(
          props.value,
          props.column,
          t("maintenanceDetail.notCollected"),
        )}
      </strong>
    </div>
  );
}

function AnalysisField(props: {
  view: ViewSpec;
  code: string;
  value: FrameValue | undefined;
  column: ColumnSpec | undefined;
}) {
  const { t } = useTranslation();
  return (
    <div className="maintenance-detail__measurement" data-field={props.code}>
      <span>{colLabel(t, props.view.code, props.code)}</span>
      <strong>
        {displayValue(
          props.value,
          props.column,
          t("maintenanceDetail.notCollected"),
        )}
      </strong>
    </div>
  );
}

export function DataMaintenanceDetail(props: DataMaintenanceDetailProps) {
  const { t } = useTranslation();
  const historyColumns = detailHistoryColumns(props.view);
  const historyEnabled =
    props.view.capabilities.history && historyColumns.length > 0;
  const from = boundedFrom(props.at, props.span);
  const point = useEntityPoint({
    view: props.view.code,
    entity: props.entity,
    at: props.at,
    includeRelated: true,
  });
  const history = useEntityHistory({
    view: props.view.code,
    entity: props.entity,
    from,
    to: props.at,
    columns: historyColumns,
    limit: 96,
    enabled: historyEnabled,
  });
  const data = point.data;
  const fields = pointFields(data);
  const columns = columnsByCode(props.view);
  const groups = analysisPriority[props.view.code] ?? {
    primary: historyColumns,
    state: [],
  };
  const lanes = temporalLanes[props.view.code] ?? [];
  const keyStatCodes = (keyStats[props.view.code] ?? []).filter((code) =>
    fields.has(code),
  );
  const identity = (identityPriority[props.view.code] ?? []).filter((code) =>
    fields.has(code),
  );
  const quality =
    history.data?.quality.status ?? data?.quality.status ?? "complete";
  const risk =
    typeof fields.get("dead_pct") === "number" &&
    Number(fields.get("dead_pct")) >= 10;

  return (
    <section
      data-testid="data-maintenance-detail"
      data-view={props.view.code}
      className="maintenance-detail"
      aria-label={t("maintenanceDetail.title", {
        entity: data?.label ?? t(`tabs.${props.view.code}`),
      })}
    >
      <header
        data-testid="maintenance-entity-strip"
        className="maintenance-detail__entity-strip"
      >
        <div className="maintenance-detail__breadcrumb">
          <span>{t(`tabs.${props.view.code}`)}</span>
          <span aria-hidden="true">›</span>
          <strong>{data?.label ?? t("table.loading")}</strong>
        </div>
        {identity.length > 0 && (
          <div className="maintenance-detail__identity">
            {identity.map((code) => (
              <span key={code}>
                <small>{colLabel(t, props.view.code, code)}</small>
                <strong>
                  {displayValue(
                    fields.get(code),
                    columns.get(code),
                    t("maintenanceDetail.notCollected"),
                  )}
                </strong>
              </span>
            ))}
          </div>
        )}
        {data !== undefined && (
          <span className="maintenance-detail__snapshot">
            <small>{t("maintenanceDetail.snapshot")}</small>
            <strong>{formatSnapshotTime(data.snapshot_ts_us)}</strong>
          </span>
        )}
        <span
          data-testid="maintenance-collection-state"
          data-status={quality}
          className="maintenance-detail__collection-state"
        >
          {t(`maintenanceDetail.collection.${quality}`, {
            defaultValue: quality,
          })}
        </span>
        {risk && (
          <span className="maintenance-detail__risk" data-severity="critical">
            {t("maintenanceDetail.risk.deadTuples", {
              value: displayValue(
                fields.get("dead_pct"),
                columns.get("dead_pct"),
                t("maintenanceDetail.notCollected"),
              ),
            })}
          </span>
        )}
        <button
          type="button"
          className="maintenance-detail__close"
          aria-label={t("maintenanceDetail.close")}
          onClick={props.onClose}
        >
          {t("maintenanceDetail.closeShort")}
        </button>
      </header>

      {point.isPending && (
        <div className="maintenance-detail__pending" role="status">
          {t("table.loading")}
        </div>
      )}
      {point.isError && (
        <div className="maintenance-detail__error" role="alert">
          {t("dock.row.error")}
        </div>
      )}
      {data !== undefined && (
        <>
          <div
            data-testid="maintenance-temporal-field"
            className="maintenance-detail__temporal-field"
          >
            {lanes.map((lane) => (
              <TemporalLane
                key={lane.code}
                view={props.view}
                lane={lane}
                history={history.data}
                columns={columns}
                fields={fields}
              />
            ))}
            {historyEnabled && history.isPending && (
              <div className="maintenance-detail__history-state" role="status">
                {t("maintenanceDetail.loadingHistory")}
              </div>
            )}
            {historyEnabled && history.isError && (
              <div className="maintenance-detail__history-state" role="alert">
                {t("maintenanceDetail.historyUnavailable")}
              </div>
            )}
            {!historyEnabled && (
              <div
                data-testid="maintenance-history-not-collected"
                className="maintenance-detail__history-state"
              >
                {t("maintenanceDetail.historyNotCollected")}
              </div>
            )}
            {historyEnabled &&
              history.data !== undefined &&
              history.data.snapshots.length === 0 && (
                <div
                  data-testid="maintenance-history-empty"
                  className="maintenance-detail__history-state"
                >
                  {t("maintenanceDetail.noSamples")}
                </div>
              )}
            <div className="maintenance-detail__event-lane">
              <span>{t("maintenanceDetail.events")}</span>
              <div>
                {data.related.map((relation) => (
                  <button
                    key={`${relation.view}:${relation.entity}`}
                    type="button"
                    aria-label={t("maintenanceDetail.openRelated", {
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
                  <em>{t("maintenanceDetail.noRelated")}</em>
                )}
              </div>
            </div>
          </div>

          <div className="maintenance-detail__analysis-grid">
            <section
              data-testid="maintenance-primary-analysis"
              className="maintenance-detail__analysis"
            >
              <h2>{t(`maintenanceDetail.primary.${props.view.code}`)}</h2>
              <HistoryMatrix
                view={props.view}
                codes={historyColumns}
                history={history.data}
                columns={columns}
                fields={fields}
              />
            </section>
            <section
              data-testid="maintenance-state-analysis"
              className="maintenance-detail__analysis"
            >
              <h2>{t(`maintenanceDetail.state.${props.view.code}`)}</h2>
              <div className="maintenance-detail__key-stats">
                {keyStatCodes.map((code) => (
                  <KeyStat
                    key={code}
                    view={props.view}
                    code={code}
                    value={fields.get(code)}
                    column={columns.get(code)}
                  />
                ))}
              </div>
              <div className="maintenance-detail__measurements">
                {groups.state
                  .filter((code) => !keyStatCodes.includes(code))
                  .map((code) => (
                    <AnalysisField
                      key={code}
                      view={props.view}
                      code={code}
                      value={fields.get(code)}
                      column={columns.get(code)}
                    />
                  ))}
              </div>
            </section>
            <section
              data-testid="maintenance-related-evidence"
              className="maintenance-detail__analysis maintenance-detail__related"
            >
              <h2>{t("maintenanceDetail.relatedEvidence")}</h2>
              {data.related.map((relation) => (
                <button
                  key={`${relation.view}:${relation.entity}:card`}
                  type="button"
                  onClick={() =>
                    props.onOpenEntity(
                      relation.view,
                      relation.entity,
                      relation.snapshot_ts_us,
                    )
                  }
                >
                  <span className="maintenance-detail__related-label">
                    <strong>
                      {t(`tabs.${relation.view}`, {
                        defaultValue: relation.view,
                      })}
                    </strong>
                    <small>
                      {t("maintenanceDetail.relatedAt", {
                        time: formatSnapshotTime(relation.snapshot_ts_us),
                      })}
                    </small>
                  </span>
                  <strong>{t("maintenanceDetail.openEvidence")}</strong>
                </button>
              ))}
              {data.related.length === 0 && (
                <p>{t("maintenanceDetail.relatedBoundary")}</p>
              )}
            </section>
          </div>
        </>
      )}
    </section>
  );
}
