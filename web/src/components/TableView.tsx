import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { ApiError, isWarmingUp } from "../api/client";
import { colDesc, colLabel } from "../api/codes";
import { TipFormula, TipRow, Tooltip } from "./Tooltip";
import {
  formatCellValue,
  fullCellValue,
  nullReasonTitle,
  whyTitle,
} from "./cellFormat";
import { useFrame } from "../api/frame";
import type {
  ClassificationResultDto,
  FrameColumnDto,
  FrameRowDto,
  FrameValue,
  HeatmapResponse,
  SparkDto,
  ViewSpec,
} from "../api/types";
import { TemporalBucketRow } from "./TemporalBucketRow";

export interface TimeMatrixColumn {
  kind?: "statements" | "activity" | "plans" | "processes";
  evidenceMode?:
    | "point_samples"
    | "interval_estimates"
    | "plan_intervals"
    | "process_intervals";
  data: HeatmapResponse | undefined;
  pending: boolean;
  error: boolean;
  metricLabel: string;
  cursorUs: string | null;
  baselineUs: string | null;
  onRetry: () => void;
}

export interface TableViewProps {
  view: ViewSpec;
  at: string;
  span: number;
  preset: string | null;
  q: string | null;
  sort: string | null;
  order: "asc" | "desc" | null;
  entity: string | null;
  onSort: (sort: string | null, order: "asc" | "desc" | null) => void;
  onSelectRow: (entity: string) => void;
  onMatched?: (matched: number) => void;
  timeMatrix?: TimeMatrixColumn | null;
  /** Dense joined PG/backend + Linux process snapshot used by Activity overview. */
  activitySnapshot?: boolean;
}

interface DisplayColumn {
  column: FrameColumnDto;
  /** Positional index into `FrameRowDto.cells` (cells follow the full answer). */
  cellIndex: number;
}

interface Pages {
  key: string;
  rows: FrameRowDto[];
  next: string | null;
  columns: FrameColumnDto[];
  matched: number;
}

/** Column types the backend accepts as frame sort keys. */
const SORTABLE_TYPES = new Set(["i64", "u64", "f64", "timestamp"]);
/** Column types whose wire value may arrive as a decimal string. */
const NUMERIC_TYPES = new Set(["i64", "u64", "f64"]);
const ROW_HEIGHT = 28;
const TIME_MATRIX_ROW_HEIGHT = 27;
const ACTIVITY_MATRIX_ROW_HEIGHT = 34;
const ROW_OVERSCAN = 4;
const FALLBACK_VIEWPORT_HEIGHT = 650;
const STATEMENT_IDENTITY_COLUMNS = new Set(["queryid", "database", "user"]);
const PLAN_IDENTITY_COLUMNS = new Set(["planid", "queryid"]);
const PROCESS_IDENTITY_COLUMNS = new Set(["pid", "type"]);
const ACTIVITY_IDENTITY_COLUMNS = new Set([
  "pid",
  "database",
  "user",
  "application",
]);
const ACTIVITY_OS_COLUMNS = new Set([
  "cpu",
  "rss",
  "threads",
  "read_bytes_per_second",
  "write_bytes_per_second",
  "command",
]);

type ActivityEvidenceGroup = "pg" | "relation" | "os";

function activityEvidenceGroup(code: string): ActivityEvidenceGroup {
  if (code === "process_link") return "relation";
  return ACTIVITY_OS_COLUMNS.has(code) ? "os" : "pg";
}

function activityColumnWidth(code: string): string | undefined {
  switch (code) {
    case "state":
      return "110px";
    case "wait_event":
      return "132px";
    case "queryid":
      return "108px";
    case "query_duration_us":
    case "transaction_duration_us":
      return "96px";
    case "process_link":
      return "90px";
    case "cpu":
      return "70px";
    case "rss":
      return "76px";
    case "threads":
      return "64px";
    case "read_bytes_per_second":
    case "write_bytes_per_second":
      return "84px";
    case "command":
    case "query":
      return "240px";
    default:
      return undefined;
  }
}

function boundedUniqueRows(
  existing: FrameRowDto[],
  incoming: FrameRowDto[],
  matched: number,
): FrameRowDto[] {
  const limit = Math.max(0, matched);
  const rows = existing.slice(0, limit);
  const entities = new Set(rows.map((row) => row.entity));
  for (const row of incoming) {
    if (rows.length >= limit) break;
    if (entities.has(row.entity)) continue;
    entities.add(row.entity);
    rows.push(row);
  }
  return rows;
}

/** Verdict tint (background wash + foreground) from a classification result. */
function verdictTintOf(
  result: ClassificationResultDto,
): { background: string; color: string } | undefined {
  if (!("level" in result)) return undefined;
  if (result.level === "warning")
    return { background: "var(--sev-warn-bg)", color: "var(--sev-warn-fg)" };
  if (result.level === "critical")
    return { background: "var(--sev-crit-bg)", color: "var(--sev-crit-fg)" };
  return undefined;
}

const SPARK_WIDTH = 60;
const SPARK_HEIGHT = 14;

function Sparkline(props: { spark: SparkDto }) {
  const values = props.spark.values;
  const present = values.filter((v): v is number => v !== null);
  if (present.length < 2) {
    return <span style={{ color: "var(--fg-dim)" }}>—</span>;
  }
  const min = Math.min(...present);
  const range = Math.max(...present) - min || 1;
  const points = values
    .map((v, i) =>
      v === null
        ? null
        : `${((i / (values.length - 1)) * SPARK_WIDTH).toFixed(1)},${(
            SPARK_HEIGHT -
            1 -
            ((v - min) / range) * (SPARK_HEIGHT - 2)
          ).toFixed(1)}`,
    )
    .filter((p): p is string => p !== null)
    .join(" ");
  return (
    <svg
      width={SPARK_WIDTH}
      height={SPARK_HEIGHT}
      aria-hidden="true"
      data-spark={props.spark.complete ? "complete" : "partial"}
    >
      <polyline
        points={points}
        fill="none"
        stroke={props.spark.complete ? "var(--accent)" : "var(--fg-dim)"}
        strokeWidth={1}
      />
    </svg>
  );
}

function TableViewImpl(props: TableViewProps) {
  const { t } = useTranslation();
  // The continuation cursor belongs to the intent (frameKey) it was taken
  // from; a stale cursor must never ride along with new key arguments.
  const [cursorState, setCursorState] = useState<{
    key: string;
    value: string;
  } | null>(null);
  const [pages, setPages] = useState<Pages | null>(null);
  const [cursorExpired, setCursorExpired] = useState(false);
  const [hovered, setHovered] = useState<string | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(
    FALLBACK_VIEWPORT_HEIGHT,
  );
  const [pendingFocusEntity, setPendingFocusEntity] = useState<string | null>(
    null,
  );
  const scrollBody = useRef<HTMLDivElement | null>(null);
  const lastMatched = useRef<number | null>(null);
  // Which selection this frame has already brought into view. Appended
  // cursor pages must not re-scroll to it and yank the reader off the tail.
  const scrolledEntity = useRef<string | null>(null);

  const frameKey = [
    props.view.code,
    props.at,
    props.span,
    props.preset,
    props.q,
    props.sort,
    props.order,
  ]
    .map((v) => v ?? "")
    .join("|");

  const cursor =
    cursorState !== null && cursorState.key === frameKey
      ? cursorState.value
      : null;

  const frame = useFrame({
    view: props.view.code,
    at: props.at,
    span: props.span,
    preset: props.preset,
    q: props.q,
    sort: props.sort,
    order: props.order,
    limit: 200,
    cursor,
  });

  // Fresh key arguments invalidate accumulated pages and the cursor.
  useEffect(() => {
    setPages((p) => (p === null || p.key === frameKey ? p : null));
    setCursorState(null);
    setCursorExpired(false);
    setScrollTop(0);
    if (scrollBody.current !== null) scrollBody.current.scrollTop = 0;
    lastMatched.current = null;
    scrolledEntity.current = null;
  }, [frameKey]);

  useLayoutEffect(() => {
    const body = scrollBody.current;
    if (body === null) return;
    const measure = () =>
      setViewportHeight(body.clientHeight || FALLBACK_VIEWPORT_HEIGHT);
    measure();
    if (typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(measure);
    observer.observe(body);
    return () => observer.disconnect();
  }, []);

  // Cursor page arrived: append its rows and fall back to the first page.
  useEffect(() => {
    const data = frame.data;
    if (data === undefined || cursor === null) return;
    setPages((p) => ({
      key: frameKey,
      rows: boundedUniqueRows(
        p !== null && p.key === frameKey ? p.rows : [],
        data.rows,
        data.page.matched,
      ),
      next: data.page.next ?? null,
      columns: p !== null && p.key === frameKey ? p.columns : data.columns,
      matched: data.page.matched,
    }));
    setCursorState(null);
  }, [frame.data, cursor, frameKey]);

  // First page arrived: adopt it unless it is just the cached base of an
  // already accumulated list; report the matched count once per change.
  const onMatched = props.onMatched;
  useEffect(() => {
    const data = frame.data;
    if (data === undefined || cursor !== null) return;
    setPages((p) =>
      p !== null && p.key === frameKey
        ? p
        : {
            key: frameKey,
            rows: boundedUniqueRows([], data.rows, data.page.matched),
            next: data.page.next ?? null,
            columns: data.columns,
            matched: data.page.matched,
          },
    );
    // The fresh first page after a cursor expiry replaces the dead one.
    setCursorExpired(false);
    if (lastMatched.current !== data.page.matched) {
      lastMatched.current = data.page.matched;
      onMatched?.(data.page.matched);
    }
  }, [frame.data, cursor, frameKey, onMatched]);

  // A stale cursor fails the follow-up request: drop the continuation AND
  // the accumulated pages — the contract is an automatic refetch of the
  // first page of the same intent plus an explicit notice, not stale rows.
  useEffect(() => {
    const error = frame.error;
    if (
      cursor !== null &&
      error instanceof ApiError &&
      (error.code === "cursor_expired" || error.status === 410)
    ) {
      setCursorState(null);
      setPages(null);
      setCursorExpired(true);
    }
  }, [frame.error, cursor]);

  const presetSpec = props.preset
    ? props.view.presets.find((p) => p.code === props.preset)
    : undefined;
  const columnMeta = new Map(
    props.view.columns.map((c) => [
      c.code,
      { availability: c.availability, reason: c.unavailable_reason ?? null },
    ]),
  );
  const columnSpec = new Map(props.view.columns.map((c) => [c.code, c]));

  const accumulated = pages !== null && pages.key === frameKey ? pages : null;
  const responseColumns = frame.data?.columns ?? accumulated?.columns ?? [];
  const matched = frame.data?.page.matched ?? accumulated?.matched ?? 0;
  const columns: DisplayColumn[] = [];
  if (responseColumns.length > 0) {
    responseColumns.forEach((column, cellIndex) => {
      // `hidden` columns are materialized only for sort/filter — never shown.
      // Unavailable (gated/not_collected) columns stay visible as honest
      // nulls with their availability reason instead of being dropped.
      if (!column.hidden) {
        columns.push({ column, cellIndex });
      }
    });
    if (presetSpec !== undefined) {
      const presetOrder = new Map(
        presetSpec.columns.map((code, i) => [code, i]),
      );
      const ordered = columns
        .filter((d) => presetOrder.has(d.column.code))
        .sort(
          (a, b) =>
            (presetOrder.get(a.column.code) ?? 0) -
            (presetOrder.get(b.column.code) ?? 0),
        );
      columns.length = 0;
      columns.push(...ordered);
    }
  }

  const rows = useMemo(
    () => (pages !== null && pages.key === frameKey ? pages.rows : []),
    [frameKey, pages],
  );
  const nextCursor =
    pages !== null && pages.key === frameKey ? pages.next : null;
  const loadingMore = cursor !== null && frame.isLoading;
  const timeMatrix = props.timeMatrix ?? null;
  const timeMatrixMode = timeMatrix !== null;
  const matrixKind = timeMatrix?.kind ?? "statements";
  const activityMatrixMode = timeMatrixMode && matrixKind === "activity";
  const activitySnapshotMode = props.activitySnapshot === true;
  const activityGroupedMode = activityMatrixMode || activitySnapshotMode;
  const plansMatrixMode = timeMatrixMode && matrixKind === "plans";
  const processesMatrixMode = timeMatrixMode && matrixKind === "processes";
  const matrixClass = activitySnapshotMode
    ? "activity-time-matrix"
    : `${matrixKind}-time-matrix`;
  const matrixNamespace = activitySnapshotMode
    ? "activity"
    : processesMatrixMode
      ? "host"
      : matrixKind;
  const matrixLoadError = timeMatrix?.error
    ? t(`${matrixNamespace}.matrix.loadError`)
    : null;
  const matrixIdentityClass = `${matrixClass}__identity`;
  const matrixTimelineClass = `${matrixClass}__timeline`;
  const matrixTimelineCellClass = `${matrixClass}__timeline-cell`;
  const identityCodes = activitySnapshotMode
    ? new Set(["pid"])
    : activityMatrixMode
      ? ACTIVITY_IDENTITY_COLUMNS
      : plansMatrixMode
        ? PLAN_IDENTITY_COLUMNS
        : processesMatrixMode
          ? PROCESS_IDENTITY_COLUMNS
          : STATEMENT_IDENTITY_COLUMNS;
  const rowHeight = activityGroupedMode
    ? ACTIVITY_MATRIX_ROW_HEIGHT
    : timeMatrixMode
      ? TIME_MATRIX_ROW_HEIGHT
      : ROW_HEIGHT;
  const identityColumns =
    timeMatrixMode || activitySnapshotMode
      ? columns.filter(({ column }) => identityCodes.has(column.code))
      : [];
  const displayColumns =
    timeMatrixMode || activitySnapshotMode
      ? columns.filter(({ column }) => !identityCodes.has(column.code))
      : columns;
  const activityColumnGroups = displayColumns.reduce<
    { code: ActivityEvidenceGroup; span: number }[]
  >((groups, { column }) => {
    const code = activityEvidenceGroup(column.code);
    const previous = groups.at(-1);
    if (previous?.code === code) previous.span += 1;
    else groups.push({ code, span: 1 });
    return groups;
  }, []);
  const renderedColumnCount =
    displayColumns.length +
    ((timeMatrixMode || activitySnapshotMode) && identityColumns.length > 0
      ? 1
      : 0);
  const heatmapRows = new Map(
    (timeMatrix?.data?.rows ?? []).map((row) => [row.entity, row]),
  );
  const bucketCount = timeMatrix?.data?.grid.bucket_count ?? 96;
  const gridFromUs = timeMatrix?.data?.grid.from_us ?? "0";
  const gridToUs =
    timeMatrix?.data?.grid.to_us ?? String(Math.max(1, bucketCount));
  const heatmapMax = Math.max(
    0,
    ...(timeMatrix?.data?.rows ?? []).flatMap((row) =>
      row.values.filter((value): value is number => value !== null),
    ),
  );
  const startIndex = Math.max(
    0,
    Math.floor(scrollTop / rowHeight) - ROW_OVERSCAN,
  );
  const windowSize = Math.ceil(viewportHeight / rowHeight) + ROW_OVERSCAN * 2;
  const endIndex = Math.min(rows.length, startIndex + windowSize);
  const visibleRows = rows.slice(startIndex, endIndex);
  const topSpacerHeight = startIndex * rowHeight;
  const bottomSpacerHeight = (rows.length - endIndex) * rowHeight;

  useEffect(() => {
    if (pendingFocusEntity === null || scrollBody.current === null) return;
    const target = [
      ...scrollBody.current.querySelectorAll("tr[data-entity]"),
    ].find((row) => row.getAttribute("data-entity") === pendingFocusEntity);
    if (!(target instanceof HTMLElement)) return;
    target.focus();
    setPendingFocusEntity(null);
  }, [pendingFocusEntity, startIndex, endIndex]);

  useEffect(() => {
    const body = scrollBody.current;
    if (body === null || props.entity === null) {
      scrolledEntity.current = null;
      return;
    }
    // Bring a selection into view once; later cursor pages grow `rows` but
    // must not re-run the scroll on an already-handled selection.
    if (scrolledEntity.current === props.entity) return;
    const index = rows.findIndex((row) => row.entity === props.entity);
    if (index < 0) return;
    scrolledEntity.current = props.entity;
    const rowTop = index * rowHeight;
    const rowBottom = rowTop + rowHeight;
    if (
      rowTop >= body.scrollTop &&
      rowBottom <= body.scrollTop + body.clientHeight
    )
      return;
    const nextTop = Math.max(0, rowTop - rowHeight * 2);
    body.scrollTop = nextTop;
    setScrollTop(nextTop);
  }, [props.entity, rowHeight, rows]);

  const headerCellStyle = (code: string): React.CSSProperties => ({
    position: "sticky",
    top: 0,
    zIndex: 1,
    background: "var(--bg-raised)",
    borderBottom: "1px solid var(--border-strong)",
    padding: "6px 10px 4px",
    textAlign: "start",
    fontFamily: "var(--ui-font)",
    fontSize: "var(--text-xs)",
    fontWeight: 600,
    color: props.sort === code ? "var(--accent-strong)" : "var(--fg-dim)",
    whiteSpace: "nowrap",
  });

  const sortArrow = (code: string): string => {
    if (props.sort !== code) return "";
    return props.order === "asc" ? " ↑" : " ↓";
  };

  const cycleSort = (code: string) => {
    if (props.sort !== code) {
      props.onSort(code, "desc");
    } else if (props.order === "desc") {
      props.onSort(code, "asc");
    } else {
      props.onSort(null, null);
    }
  };

  return (
    <section
      data-shell-region="ranked-matrix"
      style={{
        display: "flex",
        flex: "1 1 auto",
        flexDirection: "column",
        minHeight: 0,
        fontFamily: "var(--mono-font)",
        overflow: "hidden",
        background: "var(--bg-raised)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-md)",
      }}
    >
      <div
        ref={scrollBody}
        data-testid="ranked-matrix-body"
        data-loaded-rows={rows.length}
        data-rendered-rows={visibleRows.length}
        onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
        style={{ minHeight: 0, overflow: "auto" }}
      >
        <table
          aria-label={props.view.code}
          aria-rowcount={
            (matched || rows.length) + (activityGroupedMode ? 2 : 1)
          }
          className={
            activitySnapshotMode
              ? `${matrixClass} activity-snapshot-table`
              : timeMatrixMode
                ? matrixClass
                : undefined
          }
          data-testid={
            activitySnapshotMode
              ? "activity-snapshot-table"
              : timeMatrixMode
                ? matrixClass
                : undefined
          }
          style={{ borderCollapse: "collapse", width: "100%" }}
        >
          <thead>
            {activityGroupedMode && (
              <tr className="activity-time-matrix__groups">
                <th className={matrixIdentityClass}>
                  {t("activity.matrix.group.pg")}
                </th>
                {activityColumnGroups.map((group, index) => (
                  <th
                    key={`${group.code}-${index}`}
                    colSpan={group.span}
                    data-evidence-group={group.code}
                  >
                    {group.code === "relation"
                      ? t("activity.matrix.group.link")
                      : group.code === "os"
                        ? t("activity.matrix.group.os")
                        : t("activity.matrix.group.pgContext")}
                  </th>
                ))}
                {timeMatrix !== null && (
                  <th className={matrixTimelineClass}>
                    {t("activity.matrix.group.samples")}
                  </th>
                )}
              </tr>
            )}
            <tr>
              {(timeMatrixMode || activitySnapshotMode) &&
                identityColumns.length > 0 && (
                  <th
                    className={matrixIdentityClass}
                    style={{
                      ...headerCellStyle("queryid"),
                      left: 0,
                      top: activityGroupedMode ? "18px" : 0,
                      zIndex: 3,
                    }}
                  >
                    <button
                      type="button"
                      onClick={() =>
                        cycleSort(
                          activityGroupedMode
                            ? "pid"
                            : plansMatrixMode
                              ? "planid"
                              : processesMatrixMode
                                ? "pid"
                                : "queryid",
                        )
                      }
                      style={{
                        color: "inherit",
                        background: "none",
                        border: "none",
                        padding: 0,
                        cursor: "pointer",
                      }}
                    >
                      {activitySnapshotMode
                        ? colLabel(t, props.view.code, "pid")
                        : activityMatrixMode
                          ? t("activity.matrix.identity")
                          : plansMatrixMode
                            ? t("plans.matrix.identity")
                            : processesMatrixMode
                              ? t("host.matrix.identity")
                              : t("statements.matrix.identity")}
                      {sortArrow(
                        activityGroupedMode
                          ? "pid"
                          : plansMatrixMode
                            ? "planid"
                            : processesMatrixMode
                              ? "pid"
                              : "queryid",
                      )}
                    </button>
                  </th>
                )}
              {displayColumns.map(({ column }, columnIndex) => {
                const meta = columnMeta.get(column.code);
                const unavailable =
                  meta !== undefined && meta.availability !== "available";
                const spec = columnSpec.get(column.code);
                const label = colLabel(t, props.view.code, column.code);
                const desc = colDesc(t, props.view.code, column.code);
                const tip = (
                  <span style={{ display: "grid", gap: "2px" }}>
                    {desc !== null && <span>{desc}</span>}
                    <TipRow
                      label={t("tooltip.code")}
                      value={`${column.code} · ${column.type}${column.unit != null ? ` · ${column.unit}` : ""}`}
                      mono
                    />
                    {spec?.formula != null && (
                      <TipFormula
                        label={t("tooltip.formula")}
                        value={spec.formula}
                      />
                    )}
                    {spec?.source != null && (
                      <TipRow
                        label={t("tooltip.source")}
                        value={spec.source}
                        mono
                      />
                    )}
                    {spec?.threshold_metric != null && (
                      <TipRow
                        label={t("tooltip.threshold")}
                        value={spec.threshold_metric}
                        mono
                      />
                    )}
                    {spec?.lazy === true && (
                      <TipRow
                        label={t("tooltip.lazy")}
                        value={t("tooltip.lazyValue")}
                      />
                    )}
                    {unavailable && (
                      <TipRow
                        label={t(`availability.${meta.availability}`, {
                          defaultValue: meta.availability,
                        })}
                        value={meta.reason ?? "—"}
                      />
                    )}
                  </span>
                );
                return (
                  <th
                    key={column.code}
                    data-evidence-group={
                      activityGroupedMode
                        ? activityEvidenceGroup(column.code)
                        : undefined
                    }
                    style={{
                      ...headerCellStyle(column.code),
                      ...(activityGroupedMode
                        ? {
                            top: "18px",
                            width: activityColumnWidth(column.code),
                          }
                        : undefined),
                      ...(!timeMatrixMode &&
                      !activitySnapshotMode &&
                      columnIndex === 0
                        ? { left: 0, zIndex: 3 }
                        : undefined),
                      color: unavailable
                        ? "var(--fg-dim)"
                        : headerCellStyle(column.code).color,
                    }}
                  >
                    <Tooltip content={tip}>
                      {SORTABLE_TYPES.has(column.type) ? (
                        <button
                          type="button"
                          onClick={() => cycleSort(column.code)}
                          style={{
                            color: "inherit",
                            background: "none",
                            border: "none",
                            padding: 0,
                            cursor: "pointer",
                          }}
                        >
                          {label}
                          {sortArrow(column.code)}
                        </button>
                      ) : (
                        <span>{label}</span>
                      )}
                    </Tooltip>
                  </th>
                );
              })}
              {timeMatrix !== null ? (
                <th
                  className={matrixTimelineClass}
                  style={{
                    ...headerCellStyle(""),
                    ...(activityGroupedMode ? { top: "18px" } : undefined),
                  }}
                >
                  <span>
                    {activityMatrixMode
                      ? t(
                          timeMatrix?.evidenceMode === "interval_estimates"
                            ? "activity.matrix.intervals"
                            : "activity.matrix.samples",
                          { count: bucketCount },
                        )
                      : plansMatrixMode
                        ? t("plans.matrix.heatmap", { count: bucketCount })
                        : processesMatrixMode
                          ? t("host.matrix.heatmap", { count: bucketCount })
                          : t("statements.matrix.heatmap", {
                              count: bucketCount,
                            })}
                  </span>
                  <span className={`${matrixClass}__metric`}>
                    {timeMatrix.metricLabel}
                  </span>
                  {timeMatrix.pending && (
                    <span className={`${matrixClass}__status`}>
                      {t("statements.matrix.loading")}
                    </span>
                  )}
                  {matrixLoadError !== null && (
                    <span role="alert" className={`${matrixClass}__status`}>
                      {matrixLoadError}
                    </span>
                  )}
                  {timeMatrix.error && (
                    <button
                      type="button"
                      className={`${matrixClass}__retry`}
                      aria-label={`${matrixLoadError}. ${t("table.retry")}`}
                      onClick={timeMatrix.onRetry}
                    >
                      {t("table.retry")}
                    </button>
                  )}
                </th>
              ) : !activitySnapshotMode ? (
                <th style={headerCellStyle("")}>{t("table.spark")}</th>
              ) : null}
            </tr>
          </thead>
          <tbody>
            {topSpacerHeight > 0 && (
              <tr aria-hidden="true" data-virtual-spacer="top">
                <td
                  colSpan={renderedColumnCount + 1}
                  style={{ height: `${topSpacerHeight}px`, padding: 0 }}
                />
              </tr>
            )}
            {visibleRows.map((row, visibleIndex) => {
              const selected = props.entity === row.entity;
              return (
                // The whole row is one selectable control; keyboard activation
                // mirrors click through onKeyDown below.
                <tr
                  key={row.entity}
                  tabIndex={0}
                  aria-rowindex={
                    startIndex + visibleIndex + (activityGroupedMode ? 3 : 2)
                  }
                  aria-selected={selected}
                  data-entity={row.entity}
                  onClick={() => props.onSelectRow(row.entity)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") props.onSelectRow(row.entity);
                    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                      e.preventDefault();
                      const direction = e.key === "ArrowDown" ? 1 : -1;
                      const targetIndex = Math.min(
                        rows.length - 1,
                        Math.max(0, startIndex + visibleIndex + direction),
                      );
                      const target = rows[targetIndex];
                      const body = scrollBody.current;
                      if (target === undefined || body === null) return;
                      const targetTop = targetIndex * rowHeight;
                      if (
                        targetTop < body.scrollTop ||
                        targetTop + rowHeight >
                          body.scrollTop + body.clientHeight
                      ) {
                        const nextTop = Math.max(0, targetTop - rowHeight * 2);
                        body.scrollTop = nextTop;
                        setScrollTop(nextTop);
                      }
                      setPendingFocusEntity(target.entity);
                    }
                  }}
                  onMouseEnter={() => setHovered(row.entity)}
                  onMouseLeave={() =>
                    setHovered((h) => (h === row.entity ? null : h))
                  }
                  style={{
                    cursor: "pointer",
                    height: `${rowHeight}px`,
                    background:
                      selected === true
                        ? "var(--active-bg)"
                        : hovered === row.entity
                          ? "var(--hover-bg)"
                          : "transparent",
                    boxShadow: selected
                      ? "inset 2px 0 0 var(--accent)"
                      : "none",
                    transition: "background var(--transition-fast)",
                  }}
                >
                  {(timeMatrixMode || activitySnapshotMode) &&
                    identityColumns.length > 0 &&
                    (() => {
                      const primary =
                        identityColumns.find(
                          ({ column }) =>
                            column.code ===
                            (activityGroupedMode
                              ? "pid"
                              : plansMatrixMode
                                ? "planid"
                                : processesMatrixMode
                                  ? "pid"
                                  : "queryid"),
                        ) ?? identityColumns[0];
                      if (primary === undefined) return null;
                      const primaryValue: FrameValue =
                        row.cells[primary.cellIndex] ?? null;
                      const secondary = identityColumns
                        .filter(
                          ({ column }) => column.code !== primary.column.code,
                        )
                        .map(({ column, cellIndex }) => {
                          const value: FrameValue =
                            row.cells[cellIndex] ?? null;
                          return {
                            display: formatCellValue(value, column, t),
                            full:
                              fullCellValue(value, column) ??
                              formatCellValue(value, column, t),
                          };
                        })
                        .filter(({ display }) => display !== "—");
                      return (
                        <td
                          className={matrixIdentityClass}
                          title={
                            [
                              fullCellValue(primaryValue, primary.column) ??
                                formatCellValue(
                                  primaryValue,
                                  primary.column,
                                  t,
                                ),
                              ...secondary.map(({ full }) => full),
                            ]
                              .filter(Boolean)
                              .join(" · ") || undefined
                          }
                        >
                          <span className={`${matrixClass}__identity-primary`}>
                            {formatCellValue(primaryValue, primary.column, t)}
                          </span>
                          {secondary.length > 0 && (
                            <span className={`${matrixClass}__identity-meta`}>
                              {secondary
                                .map(({ display }) => display)
                                .join(" · ")}
                            </span>
                          )}
                        </td>
                      );
                    })()}
                  {displayColumns.map(({ column, cellIndex }, columnIndex) => {
                    const value: FrameValue = row.cells[cellIndex] ?? null;
                    const classification = row.classifications.find(
                      (c) => c.column === column.code,
                    );
                    const tint =
                      classification !== undefined
                        ? verdictTintOf(classification.result)
                        : undefined;
                    const numeric = NUMERIC_TYPES.has(column.type);
                    const full = fullCellValue(value, column);
                    const classificationResult = classification?.result;
                    const notClassified =
                      classificationResult !== undefined &&
                      !("level" in classificationResult)
                        ? classificationResult
                        : undefined;
                    return (
                      <td
                        key={column.code}
                        title={
                          column.code === "process_link" && value !== null
                            ? (colDesc(t, props.view.code, column.code) ??
                              full ??
                              undefined)
                            : value === null && notClassified !== undefined
                              ? nullReasonTitle(
                                  notClassified.status,
                                  notClassified.reason,
                                  t,
                                )
                              : (full ?? whyTitle(classification?.result, t))
                        }
                        style={{
                          padding: "2px 10px",
                          ...(activityGroupedMode
                            ? { width: activityColumnWidth(column.code) }
                            : undefined),
                          borderBottom: "1px solid var(--border)",
                          fontSize: "var(--text-md)",
                          textAlign: numeric ? "end" : "start",
                          ...(tint ?? {
                            color:
                              value === null ? "var(--fg-dim)" : "var(--fg)",
                          }),
                          ...(activityGroupedMode &&
                          column.code === "process_link" &&
                          value !== null
                            ? {
                                color: "var(--accent-strong)",
                                fontWeight: 600,
                              }
                            : undefined),
                          ...(!timeMatrixMode &&
                          !activitySnapshotMode &&
                          columnIndex === 0
                            ? {
                                position: "sticky",
                                left: 0,
                                zIndex: 1,
                                background:
                                  tint?.background ??
                                  (selected
                                    ? "var(--active-bg)"
                                    : hovered === row.entity
                                      ? "var(--hover-bg)"
                                      : "var(--bg-raised)"),
                              }
                            : undefined),
                          whiteSpace: "nowrap",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          maxWidth: "320px",
                        }}
                      >
                        {formatCellValue(value, column, t)}
                      </td>
                    );
                  })}
                  {timeMatrix !== null ? (
                    <td className={matrixTimelineCellClass}>
                      <TemporalBucketRow
                        row={heatmapRows.get(row.entity) ?? null}
                        bucketCount={bucketCount}
                        gridFromUs={gridFromUs}
                        gridToUs={gridToUs}
                        cursorUs={timeMatrix.cursorUs}
                        baselineUs={timeMatrix.baselineUs}
                        metricLabel={timeMatrix.metricLabel}
                        unavailableLabel={matrixLoadError ?? undefined}
                        max={heatmapMax}
                        mode={
                          activityMatrixMode
                            ? (timeMatrix?.evidenceMode ?? "point_samples")
                            : plansMatrixMode
                              ? "plan_intervals"
                              : processesMatrixMode
                                ? "process_intervals"
                                : "range"
                        }
                      />
                    </td>
                  ) : !activitySnapshotMode ? (
                    <td
                      style={{
                        padding: "2px 8px",
                        borderBottom: "1px solid var(--border)",
                      }}
                    >
                      <Sparkline spark={row.spark} />
                    </td>
                  ) : null}
                </tr>
              );
            })}
            {bottomSpacerHeight > 0 && (
              <tr aria-hidden="true" data-virtual-spacer="bottom">
                <td
                  colSpan={renderedColumnCount + 1}
                  style={{ height: `${bottomSpacerHeight}px`, padding: 0 }}
                />
              </tr>
            )}
          </tbody>
        </table>
        {frame.isLoading && cursor === null && (
          <div style={{ padding: "8px" }} aria-busy="true">
            {frame.failureCount > 0 && isWarmingUp(frame.failureReason) && (
              <div
                role="status"
                style={{
                  color: "var(--fg-dim)",
                  fontFamily: "var(--ui-font)",
                  fontSize: "var(--text-sm)",
                  marginBlockEnd: "8px",
                }}
              >
                {t("loading.warming")}
              </div>
            )}
            {[0, 1, 2, 3].map((i) => (
              <div
                key={i}
                style={{
                  height: "18px",
                  marginBlockEnd: "8px",
                  background: "var(--skeleton)",
                  borderRadius: "var(--radius-sm)",
                  animation: "pgk-pulse 1.4s ease-in-out infinite",
                  width: `${88 - i * 12}%`,
                }}
              />
            ))}
          </div>
        )}
        {frame.isError && !cursorExpired && (
          <div
            role="alert"
            style={{
              display: "flex",
              alignItems: "center",
              gap: "8px",
              padding: "12px",
              color: "var(--sev-crit-fg)",
            }}
          >
            {isWarmingUp(frame.error) ? t("error.warming") : t("table.error")}
            <button
              type="button"
              onClick={() => void frame.refetch()}
              style={{
                fontFamily: "var(--ui-font)",
                fontSize: "var(--text-sm)",
                color: "var(--fg)",
                background: "var(--bg-raised)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius-sm)",
                padding: "2px 8px",
                cursor: "pointer",
              }}
            >
              {t("table.retry")}
            </button>
          </div>
        )}
        {frame.isSuccess && rows.length === 0 && (
          <div
            style={{
              padding: "24px 12px",
              textAlign: "center",
              color: "var(--fg-dim)",
              fontFamily: "var(--ui-font)",
            }}
          >
            {t("table.empty")}
          </div>
        )}
        {cursorExpired && (
          <div
            role="status"
            style={{
              padding: "8px 12px",
              color: "var(--sev-warn-fg)",
              background: "var(--sev-warn-bg)",
              borderRadius: "var(--radius-sm)",
              margin: "8px",
            }}
          >
            {t("table.cursor_expired")}
          </div>
        )}
        {!cursorExpired && nextCursor !== null && (
          <button
            type="button"
            data-testid="table-load-more"
            disabled={loadingMore}
            onClick={() => setCursorState({ key: frameKey, value: nextCursor })}
            style={{
              fontFamily: "var(--ui-font)",
              fontSize: "var(--text-sm)",
              color: "var(--accent-strong)",
              background: "none",
              border: "none",
              padding: "8px 12px",
              cursor: "pointer",
            }}
          >
            {t("table.more")} →
          </button>
        )}
      </div>
    </section>
  );
}

export const TableView = memo(TableViewImpl);
