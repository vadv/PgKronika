import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import type { ViewSummaryItem } from "../api/types";
import { makeViewSpec } from "../testkit/apiFixtures";
import { PageHeader } from "./PageHeader";

const view = makeViewSpec({ code: "statements", canonical_metric: "time" });

function summary(overrides: Partial<ViewSummaryItem> = {}): ViewSummaryItem {
  return {
    view: "statements",
    snapshot_ts_us: "1722400000000000",
    population: 500,
    status: "complete",
    notable: false,
    notable_level: "none",
    notable_count: 0,
    collection: null,
    ...overrides,
  };
}

test("renders view title, population and matched", () => {
  render(<PageHeader view={view} summary={summary()} matched={12} />);
  expect(screen.getByText("tabs.statements")).toBeDefined();
  expect(screen.getByText("500")).toBeDefined();
  expect(screen.getByText("12")).toBeDefined();
});

test("missing summary and matched render honest dashes", () => {
  render(<PageHeader view={view} summary={undefined} matched={null} />);
  expect(screen.getByText("—")).toBeDefined();
  expect(screen.getByText("pageheader.noSnapshot")).toBeDefined();
});

test("notable stat is tinted and drills into incidents", () => {
  const onOpenIncidents = vi.fn();
  render(
    <PageHeader
      view={view}
      summary={summary({
        notable: true,
        notable_level: "critical",
        notable_count: 2,
      })}
      matched={null}
      onOpenIncidents={onOpenIncidents}
    />,
  );
  const button = screen.getByRole("button", {
    name: /pageheader.notable/,
  });
  expect(button.textContent).toContain("critical ×2");
  fireEvent.click(button);
  expect(onOpenIncidents).toHaveBeenCalledTimes(1);
});

test("collection N/M renders when present", () => {
  render(
    <PageHeader
      view={view}
      summary={summary({
        collection: {
          collected: 42,
          source_total: 45,
          read_state: "complete",
          visibility: "full",
        },
      })}
      matched={null}
    />,
  );
  expect(screen.getByText("42/45")).toBeDefined();
});

test("view code tooltip shows canonical metric and availability", async () => {
  render(<PageHeader view={view} summary={summary()} matched={null} />);
  fireEvent.mouseEnter(screen.getByText("statements", { exact: true }));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  const tip = screen.getByRole("tooltip").textContent ?? "";
  expect(tip).toContain("time");
  expect(tip).toContain("available");
});
