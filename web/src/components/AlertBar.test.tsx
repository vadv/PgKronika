import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import {
  makeSummaryQuality,
  makeViewSummaryResponse,
} from "../testkit/apiFixtures";
import { AlertBar } from "./AlertBar";

test("renders nothing when not live", () => {
  const { container } = render(
    <AlertBar
      live={false}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({ status: "partial" }),
      })}
    />,
  );
  expect(container.firstChild).toBeNull();
});

test("renders nothing when live and quality is complete without gaps", () => {
  const { container } = render(
    <AlertBar
      live={true}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({ status: "complete", gaps: [] }),
      })}
    />,
  );
  expect(container.firstChild).toBeNull();
});

test("shows a stale alert when live and quality is not complete", () => {
  render(
    <AlertBar
      live={true}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({ status: "partial" }),
      })}
    />,
  );
  expect(screen.getByRole("alert").textContent).toContain("alertbar.stale");
});

test("shows a stale alert when live and gaps exist", () => {
  render(
    <AlertBar
      live={true}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({ status: "complete", gaps: ["g1"] }),
      })}
    />,
  );
  expect(screen.getByRole("alert")).toBeDefined();
});

test("renders nothing when live but summary is undefined", () => {
  const { container } = render(<AlertBar live={true} summary={undefined} />);
  expect(container.firstChild).toBeNull();
});

test("renders nothing when the only incompleteness is the active tail", () => {
  const { container } = render(
    <AlertBar
      live={true}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({
          status: "partial",
          gaps: [],
          active_tail: true,
        }),
      })}
    />,
  );
  expect(container.firstChild).toBeNull();
});

test("renders nothing when incompleteness is purely capability (gated)", () => {
  const { container } = render(
    <AlertBar
      live={true}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({
          status: "partial",
          gaps: [],
          gated: ["pg_store_plans"],
          active_tail: false,
        }),
      })}
    />,
  );
  expect(container.firstChild).toBeNull();
});

test("degradation alert spells out the reasons", () => {
  render(
    <AlertBar
      live={true}
      summary={makeViewSummaryResponse({
        quality: makeSummaryQuality({
          status: "partial",
          gaps: [],
          resource_limited: ["statements", "plans"],
          active_tail: false,
        }),
      })}
    />,
  );
  const alert = screen.getByRole("alert");
  expect(alert.textContent).toContain("alertbar.stale");
  expect(alert.textContent).toContain("alertbar.reasons.resource_limited");
});
