import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import type { UiState } from "../state/url";
import {
  makeViewSummaryItem,
  makeViewSummaryResponse,
} from "../testkit/apiFixtures";
import { StatusBar } from "./StatusBar";

const state: UiState = {
  view: "activity",
  at: null,
  span: 3600,
  baseline: null,
  preset: null,
  q: null,
  sort: null,
  order: null,
  focus: null,
  dock: null,
  entity: null,
};

test("renders keyboard hints", () => {
  render(<StatusBar state={state} summary={undefined} />);
  expect(screen.getByRole("contentinfo")).toBeDefined();
  expect(screen.getByText(/statusbar\.hints/)).toBeDefined();
});

test("embedded content does not create a nested footer landmark", () => {
  render(<StatusBar embedded state={state} summary={undefined} />);
  expect(screen.queryByRole("contentinfo")).toBeNull();
  expect(screen.getByTestId("statusbar-content").style.height).toBe("100%");
});

test("counts notable views on the right", () => {
  render(
    <StatusBar
      state={state}
      summary={makeViewSummaryResponse({
        views: [
          makeViewSummaryItem({ view: "activity", notable: true }),
          makeViewSummaryItem({ view: "statements", notable: false }),
          makeViewSummaryItem({ view: "locks", notable: true }),
        ],
      })}
    />,
  );
  const counter = screen.getByTestId("notable-count");
  expect(counter.textContent).toContain("2");
  expect(counter.textContent).toContain("statusbar.notable");
});

test("no notable counter when nothing is notable", () => {
  render(
    <StatusBar
      state={state}
      summary={makeViewSummaryResponse({
        views: [makeViewSummaryItem({ notable: false })],
      })}
    />,
  );
  expect(screen.queryByTestId("notable-count")).toBeNull();
});

test("fixed strip reports operator context without backend quality codes", () => {
  render(
    <StatusBar
      embedded
      state={{
        ...state,
        at: "1722400000000123",
        span: 900,
        baseline: "1722399000000123",
        entity: "query:42",
      }}
      summary={makeViewSummaryResponse({
        at_us: "1722400000000123",
        quality: {
          status: "partial",
          snapshots: 12,
          gaps: ["collector restart"],
          gated: [],
          unavailable_revision: [],
          resource_limited: [],
          active_tail: false,
        },
      })}
    />,
  );
  const strip = screen.getByTestId("statusbar-content");
  expect(strip.textContent).toContain("statusbar.mode.replay");
  expect(screen.getByTitle("1722400000000123")).toBeDefined();
  expect(strip.textContent).toContain("15 m");
  expect(strip.textContent).toContain("statusbar.baseline.present");
  expect(screen.queryByTestId("statusbar-quality")).toBeNull();
  expect(strip.textContent).not.toMatch(/partial|gaps|gated|quality/i);
  expect(strip.textContent).toContain("statusbar.selection: 1");
  expect(strip.style.height).toBe("100%");
  expect(strip.style.whiteSpace).toBe("nowrap");
});

test("live strip keeps the operator context calm when summary is absent", () => {
  render(<StatusBar embedded state={state} summary={undefined} />);
  const strip = screen.getByTestId("statusbar-content");
  expect(strip.textContent).toContain("statusbar.mode.live");
  expect(strip.textContent).toContain("statusbar.cursor.latest");
  expect(strip.textContent).toContain("statusbar.baseline.none");
  expect(screen.queryByTestId("statusbar-quality")).toBeNull();
  expect(strip.textContent).toContain("statusbar.selection: 0");
});
