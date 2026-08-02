import { expect, test } from "vitest";
import type { TFunction } from "i18next";
import type { ClassificationResultDto, EvidenceDto } from "../api/types";
import {
  formatCellValue,
  fullCellValue,
  nullReasonTitle,
  whyTitle,
} from "./cellFormat";

// Tests run against the uninitialized i18next fallback: t(key) → key and
// t(key, { defaultValue }) → defaultValue, interpolation skipped.
const t = ((key: string, opts?: Record<string, unknown>) =>
  (opts?.defaultValue as string | undefined) ?? key) as TFunction<
  "translation",
  undefined
>;

test("null renders an em-dash, booleans render glyphs", () => {
  const column = { code: "calls", type: "i64" };
  expect(formatCellValue(null, column, t)).toBe("—");
  expect(formatCellValue(true, column, t)).toBe("✓");
  expect(formatCellValue(false, column, t)).toBe("✗");
});

test("timestamp columns render localized wall time from µs", () => {
  const column = { code: "time", type: "timestamp" };
  const rendered = formatCellValue("1754000000000000", column, t);
  expect(rendered).toContain("2025");
  expect(formatCellValue(1_754_000_000_000_000, column, t)).toContain("2025");
});

test("identity columns render a short hex token, full value for tooltip", () => {
  const column = { code: "queryid", type: "u64" };
  const rendered = formatCellValue("1234567890123456789", column, t);
  expect(rendered).not.toBe("1234567890123456789");
  expect(rendered.length).toBeLessThan(12);
  expect(fullCellValue("1234567890123456789", column)).toBe(
    "1234567890123456789",
  );
});

test("severity and category codes route through the dictionary", () => {
  const severity = { code: "severity_code", type: "text" };
  const category = { code: "category_code", type: "text" };
  // Missing dictionary keys in tests fall back to the raw code.
  expect(formatCellValue("error", severity, t)).toBe("error");
  expect(formatCellValue("pg.log.error_group_observed", category, t)).toBe(
    "pg.log.error_group_observed",
  );
  expect(fullCellValue("error", severity)).toBe("error");
  expect(fullCellValue("plain", { code: "query", type: "text" })).toBeNull();
});

test("process_link renders the localized relation kind, machine form in tooltip", () => {
  const column = { code: "process_link", type: "text" };
  const dict: Record<string, string> = {
    "relation.kind.best_effort.label": "приблизительная",
  };
  const localized = ((key: string, opts?: Record<string, unknown>) =>
    dict[key] ??
    (opts?.defaultValue as string | undefined) ??
    key) as TFunction<"translation", undefined>;
  expect(formatCellValue("best_effort", column, localized)).toBe(
    "приблизительная",
  );
  // Missing dictionary falls back to the honest machine code, never blank.
  expect(formatCellValue("best_effort", column, t)).toBe("best_effort");
  expect(fullCellValue("best_effort", column)).toBe("best_effort");
});

test("numeric cells honor the catalog unit", () => {
  const us = { code: "mean", type: "f64", unit: "us" };
  expect(formatCellValue(12_480_000, us, t)).toBe("12.5 s");
  const percent = { code: "hit_pct", type: "f64", unit: "percent" };
  expect(formatCellValue(99.5, percent, t)).toContain("99.5%");
  const bytesAsString = { code: "size", type: "i64", unit: "B" };
  expect(formatCellValue("2048", bytesAsString, t)).toBe("2 KiB");
  const text = { code: "query", type: "text" };
  expect(formatCellValue("select 1", text, t)).toBe("select 1");
});

function classified(
  level: string,
  evidence: EvidenceDto,
  boundary: { operator: string; value: number } | null = null,
): ClassificationResultDto {
  return { status: "classified", level, boundary, evidence };
}

test("whyTitle renders only for warning/critical verdicts", () => {
  expect(
    whyTitle(classified("ok", { kind: "scalar", observed: 1 }), t),
  ).toBeUndefined();
  expect(whyTitle(undefined, t)).toBeUndefined();
  expect(
    whyTitle({ status: "unavailable", reason: "not_collected" }, t),
  ).toBeUndefined();
  expect(
    whyTitle(
      classified(
        "critical",
        { kind: "scalar", observed: 972.84 },
        {
          operator: "at_least",
          value: 100,
        },
      ),
      t,
    ),
  ).toBe("verdict.why");
});

test("every evidence kind formats without a DSL dump", () => {
  const evidences: EvidenceDto[] = [
    { kind: "scalar", observed: 42 },
    { kind: "fraction", numerator: 3, denominator: 10, value: 0.3 },
    { kind: "limit", observed: 90, limit: 100 },
    {
      kind: "ratio_with_floor",
      count: 12,
      ratio: 0.9,
      floor: { operator: "at_least", value: 1000 },
    },
    {
      kind: "age",
      age_seconds: 3600,
      epoch_seconds: 1_754_000_000,
      now_seconds: 1_754_003_600,
    },
    {
      kind: "free_capacity",
      absolute_ceiling_bytes: { operator: "at_most", value: 4096 },
      available_bytes: 1024,
      available_fraction: 0.25,
      total_bytes: 4096,
    },
  ];
  for (const evidence of evidences) {
    const title = whyTitle(classified("warning", evidence), t);
    expect(title).toBe("verdict.why");
    expect(title).not.toContain("observed ");
  }
});

test("boundary operators pick the localized rule key", () => {
  const evidence: EvidenceDto = { kind: "scalar", observed: 1 };
  for (const op of [
    "at_least",
    ">=",
    ">",
    "at_most",
    "<=",
    "<",
    "==",
    "=",
    "!=",
  ]) {
    expect(
      whyTitle(classified("warning", evidence, { operator: op, value: 5 }), t),
    ).toBe("verdict.why");
  }
});

test("null reason pairs status and reason through the dictionary", () => {
  // Unknown codes in tests fall back to the raw code, joined with a dot.
  expect(nullReasonTitle("unavailable", "producer_gap", t)).toBe(
    "unavailable · producer_gap",
  );
  expect(nullReasonTitle("unavailable", "unavailable", t)).toBe("unavailable");
  expect(nullReasonTitle("unavailable", null, t)).toBe("unavailable");
});

test("pg state/phase enums localize with a raw fallback for unknown values", () => {
  const state = { code: "state", type: "text" };
  // Fallback t() returns the defaultValue: unknown enums pass through raw.
  expect(formatCellValue("idle in transaction", state, t)).toBe(
    "idle in transaction",
  );
  expect(fullCellValue("active", state)).toBe("active");
  const phase = { code: "phase", type: "text" };
  expect(formatCellValue("scanning heap", phase, t)).toBe("scanning heap");
  expect(fullCellValue("vacuuming heap", phase)).toBe("vacuuming heap");
});
