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
  expect(screen.getByText(/statusbar\.hints/)).toBeDefined();
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
