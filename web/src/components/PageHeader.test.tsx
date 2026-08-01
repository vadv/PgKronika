import { fireEvent, render, screen } from "@testing-library/react";
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

test("renders view title and one context line with population and matched", () => {
  render(<PageHeader view={view} summary={summary()} matched={12} live />);
  expect(screen.getByText("tabs.statements")).toBeDefined();
  const context = screen.getByTitle("pageheader.matchedHint");
  expect(context.textContent).toContain("pageheader.population: 500");
  expect(context.textContent).toContain("pageheader.matched: 12");
});

test("missing snapshot in live mode reads as pending, not an error", () => {
  render(<PageHeader view={view} summary={undefined} matched={null} live />);
  expect(screen.getByText("pageheader.livePending")).toBeDefined();
});

test("missing snapshot on a pinned cursor keeps the honest no-snapshot", () => {
  render(
    <PageHeader view={view} summary={undefined} matched={null} live={false} />,
  );
  expect(screen.getByText("pageheader.noSnapshot")).toBeDefined();
});

test("notable button carries the level text and drills into incidents", () => {
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
      live
      onOpenIncidents={onOpenIncidents}
    />,
  );
  const button = screen.getByRole("button", {
    name: /critical ×2/,
  });
  expect(button.textContent).toContain("critical ×2");
  fireEvent.click(button);
  expect(onOpenIncidents).toHaveBeenCalledTimes(1);
});

test("collection coverage joins the context line when present", () => {
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
      live
    />,
  );
  const context = screen.getByTitle("pageheader.matchedHint");
  expect(context.textContent).toContain("pageheader.collection: 42/45");
});
