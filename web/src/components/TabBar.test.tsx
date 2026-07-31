import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBar } from "./TabBar";
import type { ViewSpec, ViewSummaryItem } from "../api/types";

const views = [
  { code: "activity", availability: "available" },
  { code: "statements", availability: "gated" },
] as unknown as ViewSpec[];

const summaries = new Map<string, ViewSummaryItem>([
  [
    "activity",
    {
      view: "activity",
      snapshot_ts_us: "1",
      population: 142,
      status: "complete",
      notable: false,
      collection: null,
    },
  ],
]);

test("renders one tab per catalog view, gated dimmed", () => {
  render(
    <TabBar
      views={views}
      active="activity"
      onSelect={() => {}}
      summaries={summaries}
    />,
  );
  expect(screen.getByRole("tab", { name: /activity/i })).toBeDefined();
  const gated = screen.getByRole("tab", { name: /statements/i });
  expect(gated.getAttribute("aria-disabled")).toBe("true");
});

test("renders population badge from summary, none for gated tab", () => {
  render(
    <TabBar
      views={views}
      active="activity"
      onSelect={() => {}}
      summaries={summaries}
    />,
  );
  expect(screen.getByText("142")).toBeDefined();
  const gated = screen.getByRole("tab", { name: /statements/i });
  expect(gated.querySelector("[data-notable]")).toBeNull();
});
