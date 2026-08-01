import { fireEvent, render, screen } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import { makeIncident, makeIncidentFinding } from "../testkit/apiFixtures";
import { FocusBar, formatIntervalTime } from "./FocusBar";

test("renders key, interval and the first finding scope", () => {
  render(
    <FocusBar
      incident={makeIncident({
        incident_key: "incident-42",
        summary_code: "anomaly.os_cpu.irq",
        interval: { from: 1_722_400_000_000_000, to: 1_722_403_600_000_000 },
        findings: [
          makeIncidentFinding({
            scope: {
              logical_section: "pg_stat_database",
              identity: [],
              column: "xact",
            },
          }),
        ],
      })}
      onExit={() => {}}
    />,
  );
  expect(screen.getByRole("status")).toBeDefined();
  // The bar shows the localized summary title; the binary key stays out of
  // the headline.
  expect(screen.getByText("anomaly.os_cpu.irq")).toBeDefined();
  expect(screen.queryByText("incident-42")).toBeNull();
  expect(screen.getByText("pg_stat_database·xact")).toBeDefined();
  const interval = `${formatIntervalTime(1_722_400_000_000_000)}→${formatIntervalTime(1_722_403_600_000_000)}`;
  expect(screen.getByText(interval)).toBeDefined();
});

test("omits the scope when the incident has no findings", () => {
  const { container } = render(
    <FocusBar incident={makeIncident({ findings: [] })} onExit={() => {}} />,
  );
  expect(container.textContent).not.toContain("pg_stat_database");
});

test("exit button calls onExit", () => {
  const onExit = vi.fn();
  render(<FocusBar incident={makeIncident()} onExit={onExit} />);
  fireEvent.click(screen.getByRole("button", { name: "focus.exit" }));
  expect(onExit).toHaveBeenCalledTimes(1);
});
