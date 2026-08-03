import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { TemporalBucketRow, bucketPosition } from "./TemporalBucketRow";

describe("bucketPosition", () => {
  test("aligns a timestamp to the shared half-open bucket grid", () => {
    expect(bucketPosition("150", "100", "200", 10)).toBe(5);
    expect(bucketPosition("200", "100", "200", 10)).toBe(9);
    expect(bucketPosition("99", "100", "200", 10)).toBeNull();
    expect(bucketPosition("201", "100", "200", 10)).toBeNull();
  });
});

describe("TemporalBucketRow", () => {
  test("keeps an unavailable series distinct from a zero-valued series", () => {
    render(
      <TemporalBucketRow
        row={null}
        bucketCount={96}
        gridFromUs="100"
        gridToUs="200"
        cursorUs="150"
        baselineUs={null}
        metricLabel="total time"
      />,
    );

    expect(screen.getAllByTestId("time-matrix-bucket")).toHaveLength(96);
    expect(screen.getByTestId("temporal-row").dataset.evidence).toBe(
      "unavailable",
    );
    expect(screen.getByTestId("time-matrix-cursor").style.left).toBe("50%");
    expect(
      screen
        .getAllByTestId("time-matrix-bucket")
        .every((cell) => cell.dataset.empty === "true"),
    ).toBe(true);
  });

  test("marks Activity evidence as separated point samples", () => {
    render(
      <TemporalBucketRow
        row={null}
        bucketCount={4}
        gridFromUs="100"
        gridToUs="200"
        cursorUs={null}
        baselineUs={null}
        metricLabel="observed activity"
        mode="point_samples"
      />,
    );

    const row = screen.getByTestId("activity-sample-row");
    expect(row.dataset.mode).toBe("point_samples");
    expect(row.querySelector("svg, polyline, path")).toBeNull();
  });

  test("labels Activity interval-derived metrics without calling them point samples", () => {
    render(
      <TemporalBucketRow
        row={{
          entity: "pid:1",
          label: "pid 1",
          unit: "us",
          score: { lower: 0, upper: 10 },
          values: [10, null],
        }}
        bucketCount={2}
        gridFromUs="100"
        gridToUs="200"
        cursorUs="150"
        baselineUs={null}
        metricLabel="wait"
        mode="interval_estimates"
      />,
    );

    const row = screen.getByTestId("activity-interval-row");
    expect(row.dataset.mode).toBe("interval_estimates");
    expect(row.getAttribute("aria-label")).toContain(
      "activity.matrix.intervalRowLabel",
    );
    expect(screen.getAllByTestId("time-matrix-bucket")[0]?.title).toContain(
      "activity.matrix.intervalValue",
    );
  });
});
