import { describe, expect, test } from "vitest";
import {
  AREA_CHART_HEIGHT,
  AREA_CHART_WIDTH,
  areaChartSegments,
  breakableCode,
  formatByUnit,
  formatCompactNumber,
  formatDurationUs,
  formatTimestampUs,
  isIdentityColumn,
  shortIdToken,
} from "./format";

describe("formatCompactNumber", () => {
  test("keeps dense counters readable without clipping their magnitude", () => {
    expect(formatCompactNumber(12_400_000)).toBe("12.4M");
    expect(formatCompactNumber(9_920_000)).toBe("9.92M");
    expect(formatCompactNumber(12_400)).toBe("12.4k");
    expect(formatCompactNumber(842)).toBe("842");
  });
});

describe("formatDurationUs", () => {
  test("picks the unit a human reads fastest", () => {
    expect(formatDurationUs(842)).toBe("842 µs");
    expect(formatDurationUs(12_480)).toBe("12.5 ms");
    expect(formatDurationUs(1_070_000)).toBe("1.07 s");
    expect(formatDurationUs(412_000_000)).toBe("6.87 m");
    expect(formatDurationUs(93_600_000_000)).toBe("26 h");
    expect(formatDurationUs(200_000_000_000)).toBe("2.31 d");
  });
});

describe("formatByUnit", () => {
  test("routes through the catalog unit codes", () => {
    expect(formatByUnit(12_480_000, "us")).toBe("12.5 s");
    expect(formatByUnit(3.2, "ms")).toBe("3.2 ms");
    expect(formatByUnit(93_600, "seconds")).toBe("26 h");
    expect(formatByUnit(18.4, "percent")).toBe("18.4%");
    expect(formatByUnit(0.62, "ratio")).toBe("62%");
    expect(formatByUnit(9_814_220, "B")).toContain("MiB");
    expect(formatByUnit(412_884, "kib")).toContain("MiB");
    expect(formatByUnit(88_220_000, "bytes_per_second")).toContain("/s");
    expect(formatByUnit(10, "per_second")).toBe("10/s");
    expect(formatByUnit(12_400_000, "count")).toBe(
      new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(
        12_400_000,
      ),
    );
  });
});

describe("shortIdToken", () => {
  test("uint64 decimals become short hex tokens", () => {
    expect(shortIdToken("91802204411207101")).toBe("1462…1dbd");
    expect(shortIdToken("84102200")).toBe("5034c38");
  });

  test("non-numeric tokens are cut plainly", () => {
    expect(shortIdToken("AQAEBQAABbbbccccdd")).toBe("AQAEBQAA…");
    expect(shortIdToken("short")).toBe("short");
  });

  test("signed int64 normalizes to the unsigned 64-bit form", () => {
    // queryid arrives as a signed bigint string; the same uint64 must tokenize
    // identically whether the wire carries the negative or unsigned spelling.
    expect(shortIdToken("-1999008735841373854")).toBe("e442…b162");
    expect(shortIdToken("16447735337868177762")).toBe("e442…b162");
  });
});

test("isIdentityColumn", () => {
  expect(isIdentityColumn("queryid")).toBe(true);
  expect(isIdentityColumn("planid")).toBe(true);
  expect(isIdentityColumn("pid")).toBe(false);
});

test("formatTimestampUs renders localized wall time", () => {
  const rendered = formatTimestampUs("1754000000000000");
  expect(rendered).toContain("2025");
});

test("breakableCode breaks at segment bounds only", () => {
  expect(breakableCode("pg.log.error_group_observed")).toBe(
    "pg.\u200Blog.\u200Berror_group_observed",
  );
});

describe("areaChartSegments", () => {
  test("fills one polygon per contiguous run of samples", () => {
    const w = AREA_CHART_WIDTH;
    const h = AREA_CHART_HEIGHT;
    expect(areaChartSegments([0, 4], 4)).toEqual([
      `M0,${h} L${w},0 L${w},${h} L0,${h} Z`,
    ]);
  });

  test("breaks the shape at a gap instead of bridging it", () => {
    const h = AREA_CHART_HEIGHT;
    expect(areaChartSegments([1, 2, null, 3, 4], 4)).toEqual([
      `M0,24 L60,16 L60,${h} L0,${h} Z`,
      `M180,8 L240,0 L240,${h} L180,${h} Z`,
    ]);
  });

  test("draws nothing for an all-missing series", () => {
    expect(areaChartSegments([null, null, null], 4)).toEqual([]);
  });

  test("drops an isolated single sample instead of faking a shape for it", () => {
    const h = AREA_CHART_HEIGHT;
    expect(areaChartSegments([1, 2, null, 5], 4)).toEqual([
      `M0,24 L80,16 L80,${h} L0,${h} Z`,
    ]);
  });

  test("clamps out-of-range samples instead of drawing past the baseline", () => {
    const h = AREA_CHART_HEIGHT;
    expect(areaChartSegments([-3, 2], 4)).toEqual([
      `M0,${h} L240,16 L240,${h} L0,${h} Z`,
    ]);
  });
});
