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
});
