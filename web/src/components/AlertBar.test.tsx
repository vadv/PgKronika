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
