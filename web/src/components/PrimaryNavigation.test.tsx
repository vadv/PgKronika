import { act, fireEvent, render, screen } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import { buildNavigationGroups } from "../navigation/model";
import { makeViewSpec } from "../testkit/apiFixtures";
import { PrimaryNavigation } from "./PrimaryNavigation";

const groups = buildNavigationGroups([
  makeViewSpec({ code: "activity", availability: "available" }),
  makeViewSpec({ code: "statements", availability: "gated" }),
  makeViewSpec({ code: "plans", availability: "available" }),
  makeViewSpec({ code: "tables", availability: "available" }),
  makeViewSpec({ code: "indexes", availability: "available" }),
  makeViewSpec({ code: "vacuum", availability: "not_collected" }),
  makeViewSpec({ code: "processes", availability: "available" }),
  makeViewSpec({ code: "locks", availability: "available" }),
  makeViewSpec({ code: "events", availability: "available" }),
]);

function renderNavigation(
  overrides: Partial<React.ComponentProps<typeof PrimaryNavigation>> = {},
) {
  const props: React.ComponentProps<typeof PrimaryNavigation> = {
    groups,
    activeView: "activity",
    isLive: true,
    span: 3600,
    onSelect: vi.fn(),
    onToggleLive: vi.fn(),
    onSelectSpan: vi.fn(),
    ...overrides,
  };
  return { ...render(<PrimaryNavigation {...props} />), props };
}

test("renders grouped destinations without permanent Processes or Locks tabs", () => {
  renderNavigation();

  expect(screen.getByTestId("primary-navigation").style.height).toBe("32px");
  for (const group of ["workload", "data", "host", "events"]) {
    expect(screen.getByText(`navigation.group.${group}`)).toBeDefined();
  }
  expect(screen.getAllByRole("tab")).toHaveLength(8);
  expect(screen.queryByRole("tab", { name: /tabs\.processes/i })).toBeNull();
  expect(screen.queryByRole("tab", { name: /tabs\.locks/i })).toBeNull();
  expect(
    screen.getByRole("tab", { name: /navigation\.destination\.os/i }),
  ).toBeDefined();
});

test("renders calm inline group ordinals with ruled separators", () => {
  renderNavigation();

  const firstOrdinal = screen.getByTestId("navigation-group-ordinal-workload");
  const dataOrdinal = screen.getByTestId("navigation-group-ordinal-data");
  expect(firstOrdinal.textContent).toBe("1");
  expect(firstOrdinal.style.borderRadius).toBe("");
  expect(firstOrdinal.style.background).toBe("");
  expect(
    dataOrdinal.parentElement?.parentElement?.style.borderInlineStart,
  ).toBe("1px solid var(--border)");
});

test("reports catalog availability and selects OS through its processes backing view", () => {
  const { props } = renderNavigation();
  const statements = screen.getByRole("tab", { name: /tabs\.statements/i });

  expect(statements.getAttribute("aria-disabled")).toBe("true");
  fireEvent.click(statements);
  expect(props.onSelect).not.toHaveBeenCalled();

  fireEvent.click(
    screen.getByRole("tab", { name: /navigation\.destination\.os/i }),
  );
  expect(props.onSelect).toHaveBeenCalledWith("processes");
});

test("uses roving tab focus with arrows, Home, and End while skipping unavailable tabs", () => {
  renderNavigation();
  const activity = screen.getByRole("tab", { name: /tabs\.activity/i });
  const plans = screen.getByRole("tab", { name: /tabs\.plans/i });
  const events = screen.getByRole("tab", { name: /^tabs\.events/i });

  expect(activity.tabIndex).toBe(0);
  expect(plans.tabIndex).toBe(-1);
  act(() => activity.focus());
  fireEvent.keyDown(activity, { key: "ArrowRight" });
  expect(document.activeElement).toBe(plans);
  expect(plans.tabIndex).toBe(0);

  fireEvent.keyDown(plans, { key: "End" });
  expect(document.activeElement).toBe(events);
  fireEvent.keyDown(events, { key: "ArrowRight" });
  expect(document.activeElement).toBe(activity);
  fireEvent.keyDown(activity, { key: "ArrowLeft" });
  expect(document.activeElement).toBe(events);
  fireEvent.keyDown(events, { key: "Home" });
  expect(document.activeElement).toBe(activity);
});

test("owns Live/Replay and prepared-span actions in the primary bar", () => {
  const { props, rerender } = renderNavigation();
  fireEvent.click(screen.getByRole("button", { name: /spine\.live/i }));
  expect(props.onToggleLive).toHaveBeenCalledTimes(1);

  const day = screen.getByRole("button", { name: /spine\.span\.86400/i });
  fireEvent.click(day);
  expect(props.onSelectSpan).toHaveBeenCalledWith(86400);

  rerender(
    <PrimaryNavigation
      {...props}
      activeView="processes"
      isLive={false}
      span={86400}
    />,
  );
  expect(
    screen
      .getByRole("tab", { name: /navigation\.destination\.os/i })
      .getAttribute("aria-selected"),
  ).toBe("true");
  expect(screen.getByRole("button", { name: /spine\.replay/i })).toBeDefined();
  expect(day.getAttribute("aria-pressed")).toBe("true");
});

test("a Locks deep link leaves every destination unselected", () => {
  renderNavigation({ activeView: "locks" });
  expect(
    screen
      .getAllByRole("tab")
      .every((tab) => tab.getAttribute("aria-selected") === "false"),
  ).toBe(true);
});
