import { fireEvent, render, screen } from "@testing-library/react";
import { act } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { UiState } from "../state/url";
import {
  makeIncident,
  makeIncidentFinding,
  makeIncidentsResponse,
  makeSummaryQuality,
  makeViewSummaryResponse,
} from "../testkit/apiFixtures";
import { Header, type HeaderProps } from "./Header";

const state: UiState = {
  source: "prod-1",
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

function renderHeader(overrides: Partial<HeaderProps> = {}) {
  const props: HeaderProps = {
    state,
    summary: undefined,
    incidents: undefined,
    dataHealthOpen: false,
    onToggleDataHealth: () => {},
    onOpenIncidents: () => {},
    ...overrides,
  };
  return render(<Header {...props} />);
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

test("renders instance chip with the source name", () => {
  renderHeader();
  expect(screen.getByTestId("instance-chip").textContent).toContain("prod-1");
});

test("data chip shows unknown state without summary, ok when complete", () => {
  const { unmount } = renderHeader();
  expect(screen.getByTestId("data-health-chip").textContent).toContain(
    "header.dataUnknown",
  );
  unmount();
  renderHeader({
    summary: makeViewSummaryResponse({
      quality: makeSummaryQuality({ status: "complete" }),
    }),
  });
  expect(screen.getByTestId("data-health-chip").textContent).toContain(
    "header.dataOk",
  );
});

test("data chip shows partial when gaps or gated present, click toggles", () => {
  const onToggleDataHealth = vi.fn();
  renderHeader({
    onToggleDataHealth,
    summary: makeViewSummaryResponse({
      quality: makeSummaryQuality({ gaps: ["gap-1"], gated: ["lock_timeout"] }),
    }),
  });
  const chip = screen.getByTestId("data-health-chip");
  expect(chip.textContent).toContain("header.dataPartial");
  fireEvent.click(chip);
  expect(onToggleDataHealth).toHaveBeenCalledTimes(1);
});

test("opens the data health popover when dataHealthOpen", () => {
  renderHeader({
    dataHealthOpen: true,
    summary: makeViewSummaryResponse({
      quality: makeSummaryQuality({ snapshots: 7 }),
    }),
  });
  expect(screen.getByRole("dialog")).toBeDefined();
});

test("counts critical incidents by high-confidence findings", () => {
  const onOpenIncidents = vi.fn();
  renderHeader({
    onOpenIncidents,
    incidents: makeIncidentsResponse({
      incidents: [
        makeIncident({
          incident_key: "i1",
          findings: [makeIncidentFinding({ confidence: "high" })],
        }),
        makeIncident({
          incident_key: "i2",
          findings: [makeIncidentFinding({ confidence: "low" })],
        }),
        makeIncident({
          incident_key: "i3",
          findings: [
            makeIncidentFinding({ confidence: "medium" }),
            makeIncidentFinding({ confidence: "high" }),
          ],
        }),
        makeIncident({ incident_key: "i4", findings: [] }),
      ],
    }),
  });
  const crit = screen.getByTestId("incidents-critical");
  const warn = screen.getByTestId("incidents-warning");
  expect(crit.textContent).toContain("2");
  expect(warn.textContent).toContain("1");
  fireEvent.click(crit);
  fireEvent.click(warn);
  expect(onOpenIncidents).toHaveBeenCalledTimes(2);
});

test("no incident chips when there are no incidents with findings", () => {
  renderHeader({ incidents: makeIncidentsResponse() });
  expect(screen.queryByTestId("incidents-critical")).toBeNull();
  expect(screen.queryByTestId("incidents-warning")).toBeNull();
});

test("clock ticks every second", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-07-31T10:00:00Z"));
  renderHeader();
  const before = screen.getByTestId("clock").textContent;
  act(() => {
    vi.setSystemTime(new Date("2026-07-31T10:00:05Z"));
    vi.advanceTimersByTime(5000);
  });
  const after = screen.getByTestId("clock").textContent;
  expect(before).not.toBe(after);
});

test("copy link writes the location and shows a toast for 1.7s", () => {
  vi.useFakeTimers();
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  renderHeader();
  fireEvent.click(screen.getByRole("button", { name: /copyLink/ }));
  expect(writeText).toHaveBeenCalledWith(window.location.href);
  expect(screen.getByTestId("toast").textContent).toContain("linkCopied");
  act(() => {
    vi.advanceTimersByTime(1700);
  });
  expect(screen.queryByTestId("toast")).toBeNull();
});
