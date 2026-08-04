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
  formatTimestampUs,
  shortIdToken,
} from "../design/format";
import "./PlanDetail.css";

const MAX_HISTORY_SECONDS = 21_600;
const HISTORY_COLUMNS = [
  "calls",
  "mean",
  "rows",
  "shared_hit",
  "shared_read",
] as const;

const NUMERIC_LANES = [
  { code: "execution", metrics: ["mean", "calls"] },
  { code: "rows", metrics: ["rows"] },
  { code: "buffers", metrics: ["shared_hit", "shared_read"] },
] as const;

export interface PlanDetailProps {
  view: ViewSpec;
  entity: string;
  at: string;
  span: number;
  onClose: () => void;
  onOpenEntity: (view: string, entity: string, at: string) => void;
  onFindStatements: (queryId: string) => void;
  onFindPlans: (queryId: string) => void;
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
  if (typeof value !== "string" && typeof value !== "number") return null;
  const text = String(value).trim();
  return text === "" ? null : text;
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

function series(
  history: EntityHistoryResponse | undefined,
  code: string,
): (FrameValue | null)[] {
  if (history === undefined) return [];
  const index = history.columns.indexOf(code);
  if (index < 0) return history.snapshots.map(() => null);
  return history.snapshots.map((snapshot) =>
    snapshot.present ? (snapshot.values[index] ?? null) : null,
  );
}

function numericSeries(
  history: EntityHistoryResponse | undefined,
  code: string,
): (number | null)[] {
  return series(history, code).map((value) => numericValue(value));
}

function timelinePosition(
  timestampUs: string,
  fromUs: string,
  toUs: string,
  fallback: number,
): number {
  try {
    const span = BigInt(toUs) - BigInt(fromUs);
    if (span <= 0n) return fallback;
    const offset = BigInt(timestampUs) - BigInt(fromUs);
    const scaled = Number((offset * 100_000n) / span) / 1_000;
    return Math.min(100, Math.max(0, scaled));
  } catch {
    return fallback;
  }
}

function lineSegments(
  history: EntityHistoryResponse | undefined,
  code: string,
  fromUs: string,
  toUs: string,
): string[] {
  const values = numericSeries(history, code);
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
    const timestamp = history?.snapshots[index]?.ts_us;
    const x =
      timestamp === undefined
        ? (index / width) * 100
        : timelinePosition(timestamp, fromUs, toUs, (index / width) * 100);
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

function formattedPlan(value: FrameValue | undefined, missing: string): string {
  if (typeof value !== "string" && typeof value !== "number") return missing;
  const plan = String(value);
  if (plan.trim() === "") return missing;
  try {
    return JSON.stringify(JSON.parse(plan) as unknown, null, 2);
  } catch {
    return plan;
  }
}

function historyBucket(
  timestampUs: string,
  fromUs: string,
  toUs: string,
  fallback: number,
): number {
  const position = timelinePosition(timestampUs, fromUs, toUs, fallback);
  return Math.min(96, Math.floor((position / 100) * 96) + 1);
}

function ObservationLane(props: {
  history: EntityHistoryResponse | undefined;
  fromUs: string;
  toUs: string;
}) {
  const { t } = useTranslation();
  const snapshots = props.history?.snapshots ?? [];
  const presentCount = snapshots.filter((snapshot) => snapshot.present).length;
  let selectedIndex = -1;
  snapshots.forEach((snapshot, index) => {
    if (snapshot.present) selectedIndex = index;
  });
  return (
    <div
      data-testid="plan-temporal-lane"
      data-lane="observations"
      className="plan-detail__temporal-lane"
    >
      <div className="plan-detail__lane-label">
        <span className="plan-detail__lane-title">
          {t("planDetail.temporal.observations")}
        </span>
        <strong className="plan-detail__observation-count">
          {t("planDetail.observationCount", { count: presentCount })}
        </strong>
      </div>
      <div
        className="plan-detail__observation-track"
        role="img"
        aria-label={t("planDetail.observationAria")}
      >
        {snapshots.length === 0 && (
          <span className="plan-detail__observation-empty">
            {t("planDetail.noHistory")}
          </span>
        )}
        {snapshots.map((snapshot, index) => (
          <span
            key={snapshot.ts_us}
            data-testid="plan-observation-cell"
            data-bucket={historyBucket(
              snapshot.ts_us,
              props.fromUs,
              props.toUs,
              (index / Math.max(1, snapshots.length - 1)) * 100,
            )}
            style={{
              gridColumn: historyBucket(
                snapshot.ts_us,
                props.fromUs,
                props.toUs,
                (index / Math.max(1, snapshots.length - 1)) * 100,
              ),
            }}
            data-tone={
              !snapshot.present
                ? "missing"
                : index === selectedIndex
                  ? "selected"
                  : "observed"
            }
            className="plan-detail__observation-cell"
            title={t(
              snapshot.present
                ? "planDetail.observedAt"
                : "planDetail.notObservedAt",
              { time: snapshotTime(snapshot.ts_us) },
            )}
          />
        ))}
      </div>
    </div>
  );
}

function NumericLane(props: {
  view: ViewSpec;
  code: string;
  metrics: readonly string[];
  history: EntityHistoryResponse | undefined;
  fields: Map<string, FrameValue>;
  columns: Map<string, ColumnSpec>;
  fromUs: string;
  toUs: string;
}) {
  const { t } = useTranslation();
  const missing = t("planDetail.notObserved");
  return (
    <div
      data-testid="plan-temporal-lane"
      data-lane={props.code}
      className="plan-detail__temporal-lane"
    >
      <div className="plan-detail__lane-label">
        <span className="plan-detail__lane-title">
          {t(`planDetail.temporal.${props.code}`)}
        </span>
        <div className="plan-detail__lane-values">
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
        className="plan-detail__lane-chart"
        viewBox="0 0 100 64"
        preserveAspectRatio="none"
        role="img"
        aria-label={t("planDetail.timelineAria", {
          metric: t(`planDetail.temporal.${props.code}`),
        })}
      >
        <line
          className="plan-detail__lane-baseline"
          x1="0"
          y1="54"
          x2="100"
          y2="54"
        />
        {props.metrics.flatMap((code, metricIndex) =>
          lineSegments(props.history, code, props.fromUs, props.toUs).map(
            (points, segmentIndex) => (
              <polyline
                key={`${code}:${segmentIndex}`}
                data-series={metricIndex + 1}
                className="plan-detail__lane-trace"
                points={points}
              />
            ),
          ),
        )}
        <line
          className="plan-detail__lane-cursor"
          x1="99"
          y1="0"
          x2="99"
          y2="64"
        />
      </svg>
    </div>
  );
}

function MetricMatrix(props: {
  view: ViewSpec;
  history: EntityHistoryResponse | undefined;
  fields: Map<string, FrameValue>;
  columns: Map<string, ColumnSpec>;
}) {
  const { t } = useTranslation();
  const missing = t("planDetail.notObserved");
  return (
    <table
      data-testid="plan-metric-matrix"
      className="plan-detail__metric-matrix"
    >
      <thead>
        <tr>
          <th>{t("planDetail.matrix.metric")}</th>
          <th>{t("planDetail.matrix.current")}</th>
          <th>{t("planDetail.matrix.first")}</th>
        </tr>
      </thead>
      <tbody>
        {HISTORY_COLUMNS.map((code) => (
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
              {displayValue(
                firstObserved(series(props.history, code)),
                props.columns.get(code),
                missing,
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function PlanDetail(props: PlanDetailProps) {
  const { t } = useTranslation();
  const columns = columnsByCode(props.view);
  const historyColumns = HISTORY_COLUMNS.filter((code) => columns.has(code));
  const historyEnabled =
    props.view.capabilities.history && historyColumns.length > 0;
  const historyFrom = boundedFrom(props.at, props.span);
  const point = useEntityPoint({
    view: props.view.code,
    entity: props.entity,
    at: props.at,
    includeRelated: true,
  });
  const history = useEntityHistory({
    view: props.view.code,
    entity: props.entity,
    from: historyFrom,
    to: props.at,
    columns: historyColumns,
    buckets: 96,
    enabled: historyEnabled,
  });
  const data = point.data;
  const fields = pointFields(data);
  const planId = textValue(fields.get("planid"));
  const queryId = textValue(fields.get("queryid"));
  const planIdLabel =
    planId === null ? t("planDetail.unknownPlan") : shortIdToken(planId);
  const queryIdLabel =
    queryId === null ? t("planDetail.unknownQuery") : shortIdToken(queryId);
  const quality =
    history.data?.quality.status ?? data?.quality.status ?? "complete";
  const statements =
    data?.related.filter((relation) => relation.view === "statements") ?? [];

  return (
    <section
      data-testid="plan-detail"
      className="plan-detail"
      aria-label={t("planDetail.title", { id: planIdLabel })}
    >
      <header
        data-testid="plan-entity-strip"
        className="plan-detail__entity-strip"
      >
        <div className="plan-detail__breadcrumb">
          <span>{t("tabs.plans")}</span>
          <span aria-hidden="true">›</span>
          <strong>{planIdLabel}</strong>
        </div>
        <div className="plan-detail__identity">
          <span>
            <small>{t("planDetail.planId")}</small>
            <strong title={planId ?? undefined}>{planIdLabel}</strong>
          </span>
          <span>
            <small>{t("planDetail.queryId")}</small>
            <strong title={queryId ?? undefined}>{queryIdLabel}</strong>
          </span>
          <span>
            <small>{t("planDetail.callWindow")}</small>
            <strong>
              {displayValue(
                fields.get("first_call"),
                columns.get("first_call"),
                t("planDetail.notObserved"),
              )}
              <span aria-hidden="true"> → </span>
              {displayValue(
                fields.get("last_call"),
                columns.get("last_call"),
                t("planDetail.notObserved"),
              )}
            </strong>
          </span>
        </div>
        {data !== undefined && (
          <span className="plan-detail__snapshot">
            <small>{t("planDetail.snapshot")}</small>
            <strong>{snapshotTime(data.snapshot_ts_us)}</strong>
          </span>
        )}
        <span className="plan-detail__collection-state" data-status={quality}>
          {t(`planDetail.collection.${quality}`, { defaultValue: quality })}
        </span>
        <button
          type="button"
          className="plan-detail__close"
          aria-label={t("planDetail.close")}
          onClick={props.onClose}
        >
          {t("planDetail.closeShort")}
        </button>
      </header>

      {point.isPending && (
        <div className="plan-detail__pending" role="status">
          {t("table.loading")}
        </div>
      )}
      {point.isError && (
        <div className="plan-detail__error" role="alert">
          {t("dock.row.error")}
        </div>
      )}
      {data !== undefined && (
        <>
          <div
            data-testid="plan-temporal-field"
            className="plan-detail__temporal-field"
          >
            <ObservationLane
              history={history.data}
              fromUs={historyFrom}
              toUs={props.at}
            />
            {NUMERIC_LANES.map((lane) => (
              <NumericLane
                key={lane.code}
                view={props.view}
                code={lane.code}
                metrics={lane.metrics}
                history={history.data}
                fields={fields}
                columns={columns}
                fromUs={historyFrom}
                toUs={props.at}
              />
            ))}
            {historyEnabled && history.isPending && (
              <div className="plan-detail__history-state" role="status">
                {t("planDetail.loadingHistory")}
              </div>
            )}
            {historyEnabled && history.isError && (
              <div className="plan-detail__history-state" role="alert">
                {t("planDetail.historyUnavailable")}
              </div>
            )}
            <div className="plan-detail__related-lane">
              <span>{t("planDetail.relatedLane")}</span>
              <div>
                {statements.map((relation, index) => (
                  <button
                    key={`${relation.entity}:${relation.snapshot_ts_us}:lane`}
                    type="button"
                    aria-label={t("planDetail.openStatement", {
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
                    {t("planDetail.statementChip", { number: index + 1 })}
                  </button>
                ))}
                {queryId !== null && (
                  <>
                    <button
                      type="button"
                      aria-label={t("planDetail.findStatements")}
                      onClick={() => props.onFindStatements(queryId)}
                    >
                      {t("tabs.statements")}
                    </button>
                    <button
                      type="button"
                      aria-label={t("planDetail.findPlans")}
                      onClick={() => props.onFindPlans(queryId)}
                    >
                      {t("planDetail.otherPlansShort")}
                    </button>
                  </>
                )}
                {statements.length === 0 && queryId === null && (
                  <em>{t("planDetail.noRelated")}</em>
                )}
              </div>
            </div>
          </div>

          <div className="plan-detail__analysis-grid">
            <section
              data-testid="plan-body-evidence"
              className="plan-detail__analysis plan-detail__plan-body"
            >
              <h2>{t("planDetail.savedPlan")}</h2>
              <pre>
                <code>
                  {formattedPlan(
                    fields.get("plan"),
                    t("planDetail.planUnavailable"),
                  )}
                </code>
              </pre>
            </section>

            <section className="plan-detail__analysis">
              <h2>{t("planDetail.metricHistory")}</h2>
              <MetricMatrix
                view={props.view}
                history={history.data}
                fields={fields}
                columns={columns}
              />
              <div className="plan-detail__call-window">
                <span>{t("planDetail.firstCall")}</span>
                <strong>
                  {displayValue(
                    fields.get("first_call"),
                    columns.get("first_call"),
                    t("planDetail.notObserved"),
                  )}
                </strong>
                <span>{t("planDetail.lastCall")}</span>
                <strong>
                  {displayValue(
                    fields.get("last_call"),
                    columns.get("last_call"),
                    t("planDetail.notObserved"),
                  )}
                </strong>
              </div>
            </section>

            <section
              data-testid="plan-related-evidence"
              className="plan-detail__analysis plan-detail__related"
            >
              <h2>{t("planDetail.relatedEvidence")}</h2>
              <div className="plan-detail__related-group">
                <h3>{t("planDetail.statementCandidates")}</h3>
                {statements.map((relation, index) => (
                  <button
                    key={`${relation.entity}:${relation.snapshot_ts_us}:card`}
                    type="button"
                    aria-label={t("planDetail.openStatement", {
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
                        {t("planDetail.statementCandidate", {
                          number: index + 1,
                        })}
                      </strong>
                      <small>
                        {t("planDetail.observedAt", {
                          time: snapshotTime(relation.snapshot_ts_us),
                        })}
                      </small>
                    </span>
                    <strong>{t("planDetail.open")}</strong>
                  </button>
                ))}
                {statements.length === 0 && (
                  <p>{t("planDetail.noStatements")}</p>
                )}
              </div>
              <div className="plan-detail__continuations">
                <h3>{t("planDetail.continuations")}</h3>
                <button
                  type="button"
                  disabled={queryId === null}
                  aria-label={t("planDetail.findStatements")}
                  onClick={() =>
                    queryId !== null && props.onFindStatements(queryId)
                  }
                >
                  <span>
                    <strong>{t("planDetail.findStatements")}</strong>
                    <small>{queryIdLabel}</small>
                  </span>
                </button>
                <button
                  type="button"
                  disabled={queryId === null}
                  aria-label={t("planDetail.findPlans")}
                  onClick={() => queryId !== null && props.onFindPlans(queryId)}
                >
                  <span>
                    <strong>{t("planDetail.findPlans")}</strong>
                    <small>{queryIdLabel}</small>
                  </span>
                </button>
              </div>
            </section>
          </div>
        </>
      )}
    </section>
  );
}
