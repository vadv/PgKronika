/** Cell value and verdict rendering shared by the table, the dock and every
 * other grid: one formatter per catalog type, localized verdict phrases. */
import type { TFunction } from "i18next";
import { eventKindLabel, severityLabel, statusLabel } from "../api/codes";
import type {
  ClassificationResultDto,
  EvidenceDto,
  FrameValue,
} from "../api/types";
import {
  formatByUnit,
  formatDurationUs,
  formatNumber,
  formatTimestampUs,
  isIdentityColumn,
  shortIdToken,
} from "../design/format";

type Translate = TFunction<"translation", undefined>;

/** Column shape the formatter needs (satisfied by FrameColumnDto and ColumnSpec). */
export interface CellColumn {
  code: string;
  type: string;
  unit?: string | null;
}

const NUMERIC_TYPES = new Set(["i64", "u64", "f64"]);

function numericText(value: string): boolean {
  return value.trim() !== "" && !Number.isNaN(Number(value));
}

/**
 * Wire value → display text. Codes with a human dictionary entry
 * (severity_code, category_code) render as localized labels; identity
 * columns render as short tokens — the full value stays for the tooltip.
 */
export function formatCellValue(
  value: FrameValue,
  column: CellColumn,
  t: Translate,
): string {
  if (value === null) return "—";
  if (typeof value === "boolean") return value ? "✓" : "✗";
  if (
    column.type === "timestamp" &&
    (typeof value === "number" ||
      (typeof value === "string" && numericText(value)))
  ) {
    return formatTimestampUs(value);
  }
  if (isIdentityColumn(column.code)) {
    return shortIdToken(String(value));
  }
  if (column.code === "severity_code" && typeof value === "string") {
    return severityLabel(t, value);
  }
  if (column.code === "category_code" && typeof value === "string") {
    return eventKindLabel(t, value);
  }
  if (typeof value === "number") return formatByUnit(value, column.unit);
  if (NUMERIC_TYPES.has(column.type) && numericText(value)) {
    return formatByUnit(Number(value), column.unit);
  }
  return value;
}

/** Machine form for the tooltip when the display text is humanized
 * (short identity tokens, localized severity/kind codes); null elsewhere. */
export function fullCellValue(
  value: FrameValue,
  column: CellColumn,
): string | null {
  if (value === null || typeof value === "boolean") return null;
  if (
    isIdentityColumn(column.code) ||
    column.code === "severity_code" ||
    column.code === "category_code"
  ) {
    return String(value);
  }
  return null;
}

function formatEvidence(evidence: EvidenceDto, t: Translate): string {
  switch (evidence.kind) {
    case "scalar":
      return t("verdict.evidence.scalar", {
        value: formatNumber(evidence.observed),
      });
    case "fraction":
      return t("verdict.evidence.fraction", {
        numerator: formatNumber(evidence.numerator),
        denominator: formatNumber(evidence.denominator),
        value: formatNumber(evidence.value),
      });
    case "limit":
      return t("verdict.evidence.limit", {
        observed: formatNumber(evidence.observed),
        limit: formatNumber(evidence.limit),
      });
    case "ratio_with_floor":
      return t("verdict.evidence.ratio_with_floor", {
        ratio: formatNumber(evidence.ratio),
        floor: `${evidence.floor.operator} ${formatNumber(evidence.floor.value)}`,
      });
    case "age":
      return t("verdict.evidence.age", {
        value: formatDurationUs(evidence.age_seconds * 1_000_000),
      });
    case "free_capacity":
      return t("verdict.evidence.free_capacity", {
        available: formatByUnit(evidence.available_bytes, "B"),
        total: formatByUnit(evidence.total_bytes, "B"),
      });
  }
}

/** Localized mechanical why for a verdict cell: level, rule boundary,
 * evidence — never a DSL dump like `at_least 100 · observed 972.84`. */
export function whyTitle(
  result: ClassificationResultDto | undefined,
  t: Translate,
): string | undefined {
  if (result === undefined || !("level" in result)) return undefined;
  if (result.level !== "warning" && result.level !== "critical") {
    return undefined;
  }
  const level = t(`verdict.level.${result.level}`, {
    defaultValue: result.level,
  });
  let rule = "";
  if (result.boundary != null) {
    const value = formatNumber(result.boundary.value);
    const op = result.boundary.operator;
    const key =
      op === "at_least" || op === ">=" || op === ">"
        ? "verdict.rule.at_least"
        : op === "at_most" || op === "<=" || op === "<"
          ? "verdict.rule.at_most"
          : op === "==" || op === "="
            ? "verdict.rule.eq"
            : "verdict.rule.other";
    rule = t(key, { value, op });
  }
  return t("verdict.why", {
    level,
    rule,
    evidence: formatEvidence(result.evidence, t),
  }).trim();
}

/** Localized honest-null reason: `status · reason`, both through the dictionary. */
export function nullReasonTitle(
  status: string,
  reason: string | null | undefined,
  t: Translate,
): string {
  const base = statusLabel(t, status);
  return reason != null && reason !== status
    ? `${base} · ${statusLabel(t, reason)}`
    : base;
}
