import { expect, test } from "vitest";
import { heatColor } from "./heatmapColor";

test("null is empty cell, zero is the cold end", () => {
  expect(heatColor(null)).toBe("transparent");
  expect(heatColor(0)).not.toBe(heatColor(1));
});

test("monotonic ramp through token stops", () => {
  expect(heatColor(0.25)).toBe("var(--heat-1)");
  expect(heatColor(0.5)).toBe("var(--heat-2)");
  expect(heatColor(0.75)).toBe("var(--heat-3)");
  expect(heatColor(1)).toBe("var(--heat-4)");
});
