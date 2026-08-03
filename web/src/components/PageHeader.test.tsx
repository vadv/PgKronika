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

test("loaded live rows read as retained evidence while summary catches up", () => {
  render(<PageHeader view={view} summary={undefined} matched={504} live />);
  expect(screen.getByText("pageheader.liveRetained")).toBeDefined();
  expect(screen.queryByText("pageheader.livePending")).toBeNull();
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

test("collection coverage stays out of the normal page header", () => {
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
  expect(context.textContent).not.toContain("pageheader.collection");
  expect(context.textContent).not.toContain("42/45");
});

test("source provenance stays in explicit diagnostics instead of the page header", () => {
  render(
    <PageHeader
      view={makeViewSpec({
        code: "statements",
        inputs: [
          {
            code: "pg.stat_statements",
            availability: "available",
            logical_sections: ["pg_stat_statements"],
            type_ids: [11],
          },
        ],
      })}
      summary={summary({
        snapshot_ts_us: "1722400000000123",
        status: "partial",
        collection: {
          collected: 42,
          source_total: 45,
          read_state: "partial",
          visibility: "bounded",
        },
      })}
      matched={null}
      live={false}
    />,
  );
  expect(
    screen.queryByRole("button", { name: "pageheader.provenance.trigger" }),
  ).toBeNull();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByText("tabs.statements")).toBeDefined();
});
