import { useEffect, useRef, useState } from "react";
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
  SparkDto,
  ViewSpec,
} from "../api/types";

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
}

/** Column types the backend accepts as frame sort keys. */
const SORTABLE_TYPES = new Set(["i64", "u64", "f64", "timestamp"]);
/** Column types whose wire value may arrive as a decimal string. */
const NUMERIC_TYPES = new Set(["i64", "u64", "f64"]);

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

export function TableView(props: TableViewProps) {
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
  const lastMatched = useRef<number | null>(null);

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
    lastMatched.current = null;
  }, [frameKey]);

  // Cursor page arrived: append its rows and fall back to the first page.
  useEffect(() => {
    const data = frame.data;
    if (data === undefined || cursor === null) return;
    setPages((p) => ({
      key: frameKey,
      rows: [...(p !== null && p.key === frameKey ? p.rows : []), ...data.rows],
      next: data.page.next ?? null,
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
        : { key: frameKey, rows: data.rows, next: data.page.next ?? null },
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

  const columns: DisplayColumn[] = [];
  if (frame.data !== undefined) {
    frame.data.columns.forEach((column, cellIndex) => {
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

  const rows = pages !== null && pages.key === frameKey ? pages.rows : [];
  const nextCursor =
    pages !== null && pages.key === frameKey ? pages.next : null;
  const loadingMore = cursor !== null && frame.isLoading;

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
        data-testid="ranked-matrix-body"
        style={{ minHeight: 0, overflow: "auto" }}
      >
        <table
          aria-label={props.view.code}
          style={{ borderCollapse: "collapse", width: "100%" }}
        >
          <thead>
            <tr>
              {columns.map(({ column }, columnIndex) => {
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
                    style={{
                      ...headerCellStyle(column.code),
                      ...(columnIndex === 0
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
              <th style={headerCellStyle("")}>{t("table.spark")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const selected = props.entity === row.entity;
              return (
                // The whole row is one selectable control; keyboard activation
                // mirrors click through onKeyDown below.
                <tr
                  key={row.entity}
                  tabIndex={0}
                  aria-selected={selected}
                  data-entity={row.entity}
                  onClick={() => props.onSelectRow(row.entity)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") props.onSelectRow(row.entity);
                  }}
                  onMouseEnter={() => setHovered(row.entity)}
                  onMouseLeave={() =>
                    setHovered((h) => (h === row.entity ? null : h))
                  }
                  style={{
                    cursor: "pointer",
                    height: "28px",
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
                  {columns.map(({ column, cellIndex }, columnIndex) => {
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
                          value === null && notClassified !== undefined
                            ? nullReasonTitle(
                                notClassified.status,
                                notClassified.reason,
                                t,
                              )
                            : (full ?? whyTitle(classification?.result, t))
                        }
                        style={{
                          padding: "2px 10px",
                          borderBottom: "1px solid var(--border)",
                          fontSize: "var(--text-md)",
                          textAlign: numeric ? "end" : "start",
                          ...(tint ?? {
                            color:
                              value === null ? "var(--fg-dim)" : "var(--fg)",
                          }),
                          ...(columnIndex === 0
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
                  <td
                    style={{
                      padding: "2px 8px",
                      borderBottom: "1px solid var(--border)",
                    }}
                  >
                    <Sparkline spark={row.spark} />
                  </td>
                </tr>
              );
            })}
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
