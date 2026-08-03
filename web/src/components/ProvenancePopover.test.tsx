import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createInstance } from "i18next";
import type { ReactNode } from "react";
import { I18nextProvider } from "react-i18next";
import { afterEach, expect, test, vi } from "vitest";
import en from "../i18n/en.json";
import { ProvenancePopover } from "./ProvenancePopover";

function renderEnglish(node: ReactNode) {
  const i18n = createInstance();
  void i18n.init({ lng: "en", resources: { en: { translation: en } } });
  return render(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);
}

const completeRecord = {
  definition: "Observed disk work",
  value: "12.4",
  unit: "MiB/s",
  window: "1700000000000000–1700003600000000",
  snapshot: "1700003600000000",
  aggregation: "rate",
  formula:
    "(read_bytes_after - read_bytes_before) / (snapshot_after - snapshot_before)",
  baseline: "1699996400000000–1700000000000000",
  source: "/proc/<pid>/io",
  producer: "linux.process",
  coverage: "58/60 snapshots",
  reset: "none observed",
  sampling: "15 s cadence; short bursts may be missed",
  verdictRule: "io_pressure",
  revision: "7",
  state: "partial" as const,
  reason: "2 snapshots unavailable",
};

afterEach(() => vi.restoreAllMocks());

test("click opens persistent ordered provenance facts and omits absent fields", () => {
  renderEnglish(
    <ProvenancePopover
      triggerLabel="Show provenance"
      record={{ definition: "Observed disk work", unit: "MiB/s" }}
      renderTrigger={() => "Source"}
    />,
  );
  const trigger = screen.getByRole("button", { name: "Show provenance" });
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(trigger);
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
  const dialog = screen.getByRole("dialog", { name: "Metric provenance" });
  expect(dialog.textContent).toContain("Definition");
  expect(dialog.textContent).toContain("Observed disk work");
  expect(dialog.textContent).toContain("Unit");
  expect(dialog.textContent).not.toContain("Coverage");
  fireEvent.mouseMove(dialog);
  expect(screen.getByRole("dialog")).toBeDefined();
});

test.each(["Enter", " "])(
  "%s opens without hover and Escape restores trigger focus",
  (key) => {
    renderEnglish(
      <ProvenancePopover
        triggerLabel="Show provenance"
        record={{ definition: "Definition retained" }}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Show provenance" });
    trigger.focus();
    fireEvent.keyDown(trigger, { key });
    expect(screen.getByRole("dialog").textContent).toContain(
      "Definition retained",
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  },
);

test("outside activation restores trigger focus after the browser default focuses its target", async () => {
  renderEnglish(
    <div>
      <ProvenancePopover
        triggerLabel="Show provenance"
        record={{ definition: "Persistent fact" }}
      />
      <button type="button">Outside</button>
    </div>,
  );
  const trigger = screen.getByRole("button", { name: "Show provenance" });
  const outside = screen.getByRole("button", { name: "Outside" });
  fireEvent.click(trigger);
  fireEvent.pointerDown(outside);
  // Chromium focuses the pointer target as the pointerdown default action,
  // after the document listener has run. Reproduce that ordering explicitly.
  outside.focus();
  fireEvent.click(outside);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(outside);
  await waitFor(() => expect(document.activeElement).toBe(trigger));
});

test("all supplied fields render in contract order and formula wraps", () => {
  renderEnglish(
    <ProvenancePopover
      triggerLabel="Show provenance"
      record={completeRecord}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Show provenance" }));
  const rows = screen.getAllByTestId("provenance-row");
  expect(rows.map((row) => row.getAttribute("data-field"))).toEqual([
    "definition",
    "value",
    "unit",
    "window",
    "snapshot",
    "aggregation",
    "formula",
    "baseline",
    "source",
    "producer",
    "coverage",
    "reset",
    "sampling",
    "verdictRule",
    "revision",
    "state",
    "reason",
  ]);
  expect(screen.getByTestId("provenance-formula").style.overflowWrap).toBe(
    "anywhere",
  );
});

test("popover is clamped into the visible viewport", async () => {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      if (this.getAttribute("data-testid") === "provenance-popover") {
        return {
          left: 940,
          right: 1340,
          top: 740,
          bottom: 1040,
          width: 400,
          height: 300,
          x: 940,
          y: 740,
          toJSON: () => ({}),
        } as DOMRect;
      }
      return {
        left: 980,
        right: 1010,
        top: 740,
        bottom: 764,
        width: 30,
        height: 24,
        x: 980,
        y: 740,
        toJSON: () => ({}),
      } as DOMRect;
    },
  );
  vi.stubGlobal("innerWidth", 1024);
  vi.stubGlobal("innerHeight", 768);
  renderEnglish(
    <ProvenancePopover
      triggerLabel="Show provenance"
      record={{ definition: "Clamped" }}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Show provenance" }));
  const popover = screen.getByTestId("provenance-popover");
  await waitFor(() => expect(popover.style.left).toBe("616px"));
  expect(popover.style.top).toBe("434px");
  vi.unstubAllGlobals();
});
