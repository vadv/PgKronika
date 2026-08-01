import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBar } from "./TabBar";
import type { ViewSummaryItem } from "../api/types";
import { makeViewSpec } from "../testkit/apiFixtures";

const views = [
  makeViewSpec({ code: "activity", availability: "available" }),
  makeViewSpec({ code: "statements", availability: "gated" }),
];

const summaries = new Map<string, ViewSummaryItem>([
  [
    "activity",
    {
      view: "activity",
      snapshot_ts_us: "1",
      population: 142,
      status: "complete",
      notable: false,
      notable_level: "none",
      notable_count: 0,
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

test("tab tooltip shows availability, population and notable state", async () => {
  const richSummaries = new Map<string, ViewSummaryItem>([
    [
      "activity",
      {
        view: "activity",
        snapshot_ts_us: "1",
        population: 142,
        status: "complete",
        notable: true,
        notable_level: "warning",
        notable_count: 2,
        collection: null,
      },
    ],
  ]);
  render(
    <TabBar
      views={views}
      active="activity"
      onSelect={() => {}}
      summaries={richSummaries}
    />,
  );
  fireEvent.mouseEnter(screen.getByRole("tab", { name: /activity/i }));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  const tip = screen.getByRole("tooltip").textContent ?? "";
  expect(tip).toContain("available");
  expect(tip).toContain("142");
  expect(tip).toContain("warning");
  fireEvent.mouseLeave(screen.getByRole("tab", { name: /activity/i }));
  // Gated tab reports its availability reason.
  fireEvent.mouseEnter(screen.getByRole("tab", { name: /statements/i }));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  expect(screen.getByRole("tooltip").textContent).toContain("gated");
});
