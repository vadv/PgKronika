import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import type { ViewSummaryItem } from "../api/types";
import { makeViewSpec } from "../testkit/apiFixtures";
import { Sidebar } from "./Sidebar";

const views = [
  makeViewSpec({ code: "activity", availability: "available" }),
  makeViewSpec({ code: "statements", availability: "available" }),
  makeViewSpec({ code: "plans", availability: "gated" }),
  makeViewSpec({ code: "vacuum", availability: "not_collected" }),
];

function summary(overrides: Partial<ViewSummaryItem> = {}): ViewSummaryItem {
  return {
    view: "activity",
    snapshot_ts_us: "1",
    population: 142,
    status: "complete",
    notable: false,
    notable_level: "none",
    notable_count: 0,
    collection: null,
    ...overrides,
  };
}

test("active view is marked, gated views do not select", () => {
  const onSelect = vi.fn();
  render(
    <Sidebar
      views={views}
      active="statements"
      onSelect={onSelect}
      summaries={new Map()}
    />,
  );
  expect(
    screen
      .getByRole("tab", { name: /statements/ })
      .getAttribute("aria-selected"),
  ).toBe("true");
  fireEvent.click(screen.getByRole("tab", { name: /activity/ }));
  expect(onSelect).toHaveBeenCalledWith("activity");
  fireEvent.click(screen.getByRole("tab", { name: /plans/ }));
  expect(onSelect).toHaveBeenCalledTimes(1);
});

test("population and notable marker render from summary", () => {
  render(
    <Sidebar
      views={views}
      active="activity"
      onSelect={() => {}}
      summaries={
        new Map([
          [
            "activity",
            summary({
              notable: true,
              notable_level: "critical",
              notable_count: 3,
            }),
          ],
        ])
      }
    />,
  );
  expect(screen.getByText("142")).toBeDefined();
  expect(screen.getByTitle("critical ×3")).toBeDefined();
});

test("availability dots distinguish available, gated and not_collected", () => {
  const { container } = render(
    <Sidebar
      views={views}
      active="activity"
      onSelect={() => {}}
      summaries={new Map()}
    />,
  );
  const dots = container.querySelectorAll("[aria-hidden='true']");
  const colors = [...dots].map((d) => (d as HTMLElement).style.background);
  expect(colors).toContain("var(--sev-ok)");
  expect(colors).toContain("var(--sev-warn)");
  expect(colors).toContain("var(--fg-dim)");
});

test("sidebar tooltip shows availability and population", async () => {
  render(
    <Sidebar
      views={views}
      active="activity"
      onSelect={() => {}}
      summaries={new Map([["activity", summary()]])}
    />,
  );
  fireEvent.mouseEnter(screen.getByRole("tab", { name: /activity/ }));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  const tip = screen.getByRole("tooltip").textContent ?? "";
  expect(tip).toContain("available");
  expect(tip).toContain("142");
});
