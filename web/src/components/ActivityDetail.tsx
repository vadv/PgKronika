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
import { formatByUnit } from "../design/format";
import "./ActivityDetail.css";

const MAX_HISTORY_SECONDS = 21_600;
const HISTORY_COLUMNS = [
  "state",
  "wait_event",
  "query_duration_us",
  "transaction_duration_us",
  "cpu",
  "rss",
  "read_bytes_per_second",
  "write_bytes_per_second",
] as const;

const NUMERIC_LANES = [
  {
    code: "durations",
    metrics: ["query_duration_us", "transaction_duration_us"],
  },
  { code: "resources", metrics: ["cpu", "rss"] },
  {
    code: "disk",
    metrics: ["read_bytes_per_second", "write_bytes_per_second"],
  },
] as const;

const MATRIX_COLUMNS = [
  "query_duration_us",
  "transaction_duration_us",
  "cpu",
  "rss",
  "threads",
  "read_bytes_per_second",
  "write_bytes_per_second",
] as const;

export interface ActivityDetailProps {
  view: ViewSpec;
  entity: string;
  at: string;
  span: number;
  onClose: () => void;
  onOpenEntity: (view: string, entity: string, at: string) => void;
  onFindStatements: (queryId: string) => void;
  onOpenWaits: (pid: number) => void;
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

function textValue(value: FrameValue | undefined): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

function displayValue(
  value: FrameValue | undefined,
  column: ColumnSpec | undefined,
  missing: string,
): string {
  if (value === null || value === undefined) return missing;
  if (typeof value === "number") return formatByUnit(value, column?.unit);
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value);
}

function series(
  history: EntityHistoryResponse | undefined,
  code: string,
): (FrameValue | null)[] {
  if (history === undefined) return [];
  const index = history.columns.indexOf(code);
  if (index < 0) return history.snapshots.map(() => null);
  return history.snapshots.map((snapshot) => snapshot.values[index] ?? null);
}

function numericSeries(
  history: EntityHistoryResponse | undefined,
  code: string,
): (number | null)[] {
  return series(history, code).map((value) => numericValue(value));
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

function firstObserved(values: (FrameValue | null)[]): FrameValue | null {
  return values.find((value) => value !== null) ?? null;
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

function NumericLane(props: {
  view: ViewSpec;
  code: string;
  metrics: readonly string[];
  history: EntityHistoryResponse | undefined;
  fields: Map<string, FrameValue>;
  columns: Map<string, ColumnSpec>;
}) {
  const { t } = useTranslation();
  const missing = t("activityDetail.notObserved");
  const title = t(`activityDetail.temporal.${props.code}`);
  return (
    <div
      data-testid="activity-temporal-lane"
      data-lane={props.code}
      className="activity-detail__temporal-lane"
    >
      <div className="activity-detail__lane-label">
        <span className="activity-detail__lane-title">{title}</span>
        <div className="activity-detail__lane-values">
          {props.metrics.map((code, index) => (
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
        className="activity-detail__lane-chart"
        viewBox="0 0 100 64"
        preserveAspectRatio="none"
        role="img"
        aria-label={t("activityDetail.timelineAria", { metric: title })}
      >
        <line
          className="activity-detail__lane-baseline"
          x1="0"
          y1="54"
          x2="100"
          y2="54"
        />
        {props.metrics.flatMap((code, metricIndex) =>
          lineSegments(numericSeries(props.history, code)).map(
            (points, segmentIndex) => (
              <polyline
                key={`${code}:${segmentIndex}`}
                data-series={metricIndex + 1}
                className="activity-detail__lane-trace"
                points={points}
              />
            ),
          ),
        )}
        <line
          className="activity-detail__lane-cursor"
          x1="99"
          y1="0"
          x2="99"
          y2="64"
        />
      </svg>
    </div>
  );
}

function ObservationLane(props: {
  history: EntityHistoryResponse | undefined;
  fields: Map<string, FrameValue>;
}) {
  const { t } = useTranslation();
  const states = series(props.history, "state");
  const waits = series(props.history, "wait_event");
  const state = textValue(props.fields.get("state"));
  const wait = textValue(props.fields.get("wait_event"));
  return (
    <div
      data-testid="activity-temporal-lane"
      data-lane="observations"
      className="activity-detail__temporal-lane"
    >
      <div className="activity-detail__lane-label">
        <span className="activity-detail__lane-title">
          {t("activityDetail.temporal.observations")}
        </span>
        <div className="activity-detail__observation-current">
          <strong>{state ?? t("activityDetail.notObserved")}</strong>
          <span>{wait ?? t("activityDetail.noWait")}</span>
        </div>
      </div>
      <div
        className="activity-detail__observation-track"
        style={{
          gridTemplateColumns: `repeat(${Math.max(states.length, 1)}, minmax(2px, 1fr))`,
        }}
        role="img"
        aria-label={t("activityDetail.observationAria")}
      >
        {states.length === 0 && (
          <span className="activity-detail__observation-empty">
            {t("activityDetail.noHistory")}
          </span>
        )}
        {states.map((stateValue, index) => {
          const observedState = textValue(stateValue);
          const observedWait = textValue(waits[index]);
          const tone =
            observedState === null
              ? "missing"
              : observedWait?.toLowerCase().startsWith("lock:")
                ? "critical"
                : observedWait !== null
                  ? "waiting"
                  : observedState.toLowerCase().startsWith("active")
                    ? "active"
                    : "idle";
          return (
            <span
              key={`${props.history?.snapshots[index]?.ts_us ?? index}`}
              data-testid="activity-observation-cell"
              data-tone={tone}
              className="activity-detail__observation-cell"
              title={
                observedState === null
                  ? t("activityDetail.notObserved")
                  : [observedState, observedWait].filter(Boolean).join(" · ")
              }
            />
          );
        })}
      </div>
    </div>
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
    <div className="activity-detail__fact" data-field={props.code}>
      <span>{colLabel(t, props.view.code, props.code)}</span>
      <strong>
        {displayValue(
          props.fields.get(props.code),
          props.columns.get(props.code),
          t("activityDetail.notObserved"),
        )}
      </strong>
    </div>
  );
}

function SnapshotMatrix(props: {
  view: ViewSpec;
  history: EntityHistoryResponse | undefined;
  fields: Map<string, FrameValue>;
  columns: Map<string, ColumnSpec>;
}) {
  const { t } = useTranslation();
  const missing = t("activityDetail.notObserved");
  return (
    <table
      data-testid="activity-snapshot-matrix"
      className="activity-detail__snapshot-matrix"
    >
      <thead>
        <tr>
          <th>{t("activityDetail.matrix.metric")}</th>
          <th>{t("activityDetail.matrix.current")}</th>
          <th>{t("activityDetail.matrix.first")}</th>
        </tr>
      </thead>
      <tbody>
        {MATRIX_COLUMNS.map((code) => {
          const first = firstObserved(series(props.history, code));
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
              <td>{displayValue(first, props.columns.get(code), missing)}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

export function ActivityDetail(props: ActivityDetailProps) {
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
  const pid = numericValue(fields.get("pid"));
  const queryId = fields.get("queryid");
  const queryIdText =
    typeof queryId === "number" || typeof queryId === "string"
      ? String(queryId)
      : null;
  const query = textValue(fields.get("query"));
  const database = textValue(fields.get("database"));
  const role = textValue(fields.get("user"));
  const application = textValue(fields.get("application"));
  const state = textValue(fields.get("state"));
  const wait = textValue(fields.get("wait_event"));
  const command = textValue(fields.get("command"));
  const quality =
    history.data?.quality.status ?? data?.quality.status ?? "complete";
  const processes =
    data?.related.filter((relation) => relation.view === "processes") ?? [];

  return (
    <section
      data-testid="activity-detail"
      className="activity-detail"
      aria-label={t("activityDetail.title", {
        pid: pid === null ? "—" : String(pid),
      })}
    >
      <header
        data-testid="activity-entity-strip"
        className="activity-detail__entity-strip"
      >
        <div className="activity-detail__breadcrumb">
          <span>{t("tabs.activity")}</span>
          <span aria-hidden="true">›</span>
          <strong>{pid === null ? "—" : `PID ${pid}`}</strong>
        </div>
        <div className="activity-detail__identity">
          {[
            ["database", database],
            ["role", role],
            ["application", application],
          ].map(([code, value]) => (
            <span key={code}>
              <small>{t(`activityDetail.${code}`)}</small>
              <strong title={value ?? undefined}>
                {value ?? t("activityDetail.notObserved")}
              </strong>
            </span>
          ))}
        </div>
        {data !== undefined && (
          <span className="activity-detail__snapshot">
            <small>{t("activityDetail.snapshot")}</small>
            <strong>{snapshotTime(data.snapshot_ts_us)}</strong>
          </span>
        )}
        <span className="activity-detail__state-signal">
          <strong>{state ?? t("activityDetail.notObserved")}</strong>
          {wait !== null && <span>{wait}</span>}
        </span>
        <span
          className="activity-detail__collection-state"
          data-status={quality}
        >
          {t(`activityDetail.collection.${quality}`, { defaultValue: quality })}
        </span>
        <button
          type="button"
          className="activity-detail__close"
          aria-label={t("activityDetail.close")}
          onClick={props.onClose}
        >
          {t("activityDetail.closeShort")}
        </button>
      </header>

      {point.isPending && (
        <div className="activity-detail__pending" role="status">
          {t("table.loading")}
        </div>
      )}
      {point.isError && (
        <div className="activity-detail__error" role="alert">
          {t("dock.row.error")}
        </div>
      )}
      {data !== undefined && (
        <>
          <div
            data-testid="activity-temporal-field"
            className="activity-detail__temporal-field"
          >
            <ObservationLane history={history.data} fields={fields} />
            {NUMERIC_LANES.map((lane) => (
              <NumericLane
                key={lane.code}
                view={props.view}
                code={lane.code}
                metrics={lane.metrics}
                history={history.data}
                fields={fields}
                columns={columns}
              />
            ))}
            {historyEnabled && history.isPending && (
              <div className="activity-detail__history-state" role="status">
                {t("activityDetail.loadingHistory")}
              </div>
            )}
            {historyEnabled && history.isError && (
              <div className="activity-detail__history-state" role="alert">
                {t("activityDetail.historyUnavailable")}
              </div>
            )}
            <div className="activity-detail__related-lane">
              <span>{t("activityDetail.relatedLane")}</span>
              <div>
                {processes.map((relation, index) => (
                  <button
                    key={`${relation.entity}:${relation.snapshot_ts_us}:lane`}
                    type="button"
                    aria-label={t("activityDetail.openProcess", {
                      number: index + 1,
                    })}
                    onClick={() =>
                      props.onOpenEntity(
                        relation.view,
                        relation.entity,
                        relation.snapshot_ts_us,
                      )
                    }
                  >
                    {t("activityDetail.processChip", { number: index + 1 })}
                  </button>
                ))}
                {queryIdText !== null && (
                  <button
                    type="button"
                    aria-label={t("activityDetail.findStatements")}
                    onClick={() => props.onFindStatements(queryIdText)}
                  >
                    {t("tabs.statements")}
                  </button>
                )}
                {processes.length === 0 && queryIdText === null && (
                  <em>{t("activityDetail.noRelated")}</em>
                )}
              </div>
            </div>
          </div>

          <div className="activity-detail__analysis-grid">
            <section
              data-testid="activity-postgres-observation"
              className="activity-detail__analysis"
            >
              <h2>{t("activityDetail.postgresObservation")}</h2>
              <div className="activity-detail__facts">
                {[
                  "state",
                  "wait_event",
                  "backend_type",
                  "query_duration_us",
                  "transaction_duration_us",
                  "queryid",
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
              <div className="activity-detail__sql">
                <span>{t("activityDetail.sql")}</span>
                <code>{query ?? t("activityDetail.sqlUnavailable")}</code>
              </div>
            </section>

            <section className="activity-detail__analysis">
              <h2>{t("activityDetail.snapshotMatrix")}</h2>
              <SnapshotMatrix
                view={props.view}
                history={history.data}
                fields={fields}
                columns={columns}
              />
              <div className="activity-detail__command">
                <span>{t("activityDetail.command")}</span>
                <code>{command ?? t("activityDetail.notObserved")}</code>
              </div>
            </section>

            <section
              data-testid="activity-related-evidence"
              className="activity-detail__analysis activity-detail__related"
            >
              <h2>{t("activityDetail.relatedEvidence")}</h2>
              <div className="activity-detail__related-group">
                <h3>{t("activityDetail.processes")}</h3>
                {processes.map((relation, index) => (
                  <button
                    key={`${relation.entity}:${relation.snapshot_ts_us}:card`}
                    type="button"
                    aria-label={t("activityDetail.openProcess", {
                      number: index + 1,
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
                        {t("activityDetail.processCandidate", {
                          number: index + 1,
                        })}
                      </strong>
                      <small>
                        {t("activityDetail.relatedAt", {
                          time: snapshotTime(relation.snapshot_ts_us),
                        })}
                      </small>
                    </span>
                    <strong>{t("activityDetail.openEvidence")}</strong>
                  </button>
                ))}
                {processes.length === 0 && (
                  <p>{t("activityDetail.noProcesses")}</p>
                )}
              </div>
              <div className="activity-detail__related-group">
                <h3>{t("activityDetail.continuations")}</h3>
                {queryIdText !== null && (
                  <button
                    type="button"
                    aria-label={t("activityDetail.findStatements")}
                    onClick={() => props.onFindStatements(queryIdText)}
                  >
                    <span>
                      <strong>{t("activityDetail.findStatements")}</strong>
                      <small>
                        {t("activityDetail.queryId", { id: queryIdText })}
                      </small>
                    </span>
                    <strong>{t("activityDetail.openEvidence")}</strong>
                  </button>
                )}
                {pid !== null && Number.isSafeInteger(pid) && (
                  <button
                    type="button"
                    aria-label={t("activityDetail.openWaits")}
                    onClick={() => props.onOpenWaits(pid)}
                  >
                    <span>
                      <strong>{t("activityDetail.openWaits")}</strong>
                      <small>{t("activityDetail.pid", { pid })}</small>
                    </span>
                    <strong>{t("activityDetail.openEvidence")}</strong>
                  </button>
                )}
              </div>
            </section>
          </div>
        </>
      )}
    </section>
  );
}
