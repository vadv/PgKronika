import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
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

const numberFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});

function formatBytes(value: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let scaled = value;
  let unit = 0;
  while (Math.abs(scaled) >= 1024 && unit < units.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  return `${numberFormat.format(Number(scaled.toFixed(1)))} ${units[unit]}`;
}

function formatNumber(value: number, unit: string | null | undefined): string {
  if (unit === "B") return formatBytes(value);
  if (unit === "%") return `${numberFormat.format(value)}%`;
  return numberFormat.format(value);
}

function formatCell(value: FrameValue, column: FrameColumnDto): string {
  if (value === null) return "—";
  if (typeof value === "boolean") return value ? "✓" : "✗";
  if (typeof value === "number") return formatNumber(value, column.unit);
  if (
    NUMERIC_TYPES.has(column.type) &&
    value.trim() !== "" &&
    !Number.isNaN(Number(value))
  ) {
    return formatNumber(Number(value), column.unit);
  }
  return value;
}

/** Verdict color from a classification result, if it is a classified one. */
function verdictColor(result: ClassificationResultDto): string | undefined {
  if (!("level" in result)) return undefined;
  if (result.level === "warning") return "var(--sev-warn)";
  if (result.level === "critical") return "var(--sev-crit)";
  return undefined;
}

function nullTitle(
  result: ClassificationResultDto | undefined,
): string | undefined {
  if (result === undefined || "level" in result) return undefined;
  return `${result.status}: ${result.reason}`;
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
  const [cursor, setCursor] = useState<string | null>(null);
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
    setCursor(null);
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
    setCursor(null);
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
    if (lastMatched.current !== data.page.matched) {
      lastMatched.current = data.page.matched;
      onMatched?.(data.page.matched);
    }
  }, [frame.data, cursor, frameKey, onMatched]);

  // A stale cursor fails the follow-up request; keep the loaded rows and
  // offer a reset instead of losing them.
  useEffect(() => {
    const error = frame.error;
    if (
      cursor !== null &&
      error instanceof ApiError &&
      (error.code === "cursor_expired" || error.status === 410)
    ) {
      setCursor(null);
      setCursorExpired(true);
    }
  }, [frame.error, cursor]);

  const presetSpec = props.preset
    ? props.view.presets.find((p) => p.code === props.preset)
    : undefined;
  const availability = new Map(
    props.view.columns.map((c) => [c.code, c.availability]),
  );

  const columns: DisplayColumn[] = [];
  if (frame.data !== undefined) {
    frame.data.columns.forEach((column, cellIndex) => {
      if (availability.get(column.code) === "available") {
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
    background: "var(--bg)",
    borderBottom: "1px solid var(--border)",
    padding: "2px 8px",
    textAlign: "start",
    fontWeight: "normal",
    textTransform: "uppercase",
    color: props.sort === code ? "var(--accent)" : "var(--fg-dim)",
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
    <section style={{ fontFamily: "var(--mono-font)", overflow: "auto" }}>
      <table
        aria-label={props.view.code}
        style={{ borderCollapse: "collapse", width: "100%" }}
      >
        <thead>
          <tr>
            {columns.map(({ column }) => (
              <th key={column.code} style={headerCellStyle(column.code)}>
                {SORTABLE_TYPES.has(column.type) ? (
                  <button
                    type="button"
                    onClick={() => cycleSort(column.code)}
                    style={{
                      fontFamily: "var(--mono-font)",
                      textTransform: "uppercase",
                      color: "inherit",
                      background: "none",
                      border: "none",
                      padding: 0,
                      cursor: "pointer",
                    }}
                  >
                    {column.code}
                    {sortArrow(column.code)}
                  </button>
                ) : (
                  `${column.code}`
                )}
              </th>
            ))}
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
                  background:
                    selected || hovered === row.entity
                      ? "var(--bg-raised)"
                      : "transparent",
                  boxShadow: selected ? "inset 2px 0 0 var(--accent)" : "none",
                }}
              >
                {columns.map(({ column, cellIndex }) => {
                  const value: FrameValue = row.cells[cellIndex] ?? null;
                  const classification = row.classifications.find(
                    (c) => c.column === column.code,
                  );
                  return (
                    <td
                      key={column.code}
                      title={
                        value === null
                          ? nullTitle(classification?.result)
                          : undefined
                      }
                      style={{
                        padding: "2px 8px",
                        borderBottom: "1px solid var(--border)",
                        color:
                          classification !== undefined
                            ? (verdictColor(classification.result) ??
                              "var(--fg)")
                            : value === null
                              ? "var(--fg-dim)"
                              : "var(--fg)",
                        whiteSpace: "nowrap",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        maxWidth: "320px",
                      }}
                    >
                      {formatCell(value, column)}
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
        <div style={{ padding: "8px", color: "var(--fg-dim)" }}>
          {t("table.loading")}
        </div>
      )}
      {frame.isError && !cursorExpired && (
        <div style={{ padding: "8px", color: "var(--sev-crit)" }}>
          {t("table.error")}
        </div>
      )}
      {frame.isSuccess && rows.length === 0 && (
        <div style={{ padding: "8px", color: "var(--fg-dim)" }}>
          {t("table.empty")}
        </div>
      )}
      {cursorExpired && (
        <div
          style={{
            display: "flex",
            gap: "8px",
            alignItems: "center",
            padding: "8px",
            color: "var(--sev-warn)",
          }}
        >
          {t("table.cursor_expired")}
          <button
            type="button"
            onClick={() => {
              setPages(null);
              setCursorExpired(false);
              void frame.refetch();
            }}
            style={{
              fontFamily: "var(--mono-font)",
              color: "var(--fg)",
              background: "var(--bg-raised)",
              border: "1px solid var(--border)",
              cursor: "pointer",
            }}
          >
            {t("table.reset")}
          </button>
        </div>
      )}
      {!cursorExpired && nextCursor !== null && (
        <button
          type="button"
          disabled={loadingMore}
          onClick={() => setCursor(nextCursor)}
          style={{
            fontFamily: "var(--mono-font)",
            color: "var(--accent)",
            background: "none",
            border: "none",
            padding: "8px",
            cursor: "pointer",
          }}
        >
          {t("table.more")}
        </button>
      )}
    </section>
  );
}
