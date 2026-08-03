import { render, screen } from "@testing-library/react";
import { createInstance } from "i18next";
import type { ReactNode } from "react";
import { I18nextProvider } from "react-i18next";
import { expect, test } from "vitest";
import en from "../i18n/en.json";
import { SemanticBadge } from "./SemanticBadge";

function renderEnglish(node: ReactNode) {
  const i18n = createInstance();
  void i18n.init({ lng: "en", resources: { en: { translation: en } } });
  return render(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);
}

test.each([
  ["G", "Gauge"],
  ["ΔC", "Counter delta"],
  ["R", "Rate"],
  ["S", "Snapshot"],
  ["E", "Event"],
  ["EST", "Estimate"],
] as const)(
  "semantic kind %s has a visible code and non-color name",
  (kind, name) => {
    renderEnglish(<SemanticBadge kind={kind} />);
    const badge = screen.getByLabelText(`${kind}: ${name}`);
    expect(badge.textContent).toBe(kind);
  },
);

test.each([
  ["partial", "Partial"],
  ["gated", "Gated"],
  ["reset", "Reset boundary"],
  ["gap", "Gap"],
  ["unsupported", "Unsupported"],
  ["top_n", "Top-N limited"],
] as const)(
  "data state %s is named in text instead of color alone",
  (state, label) => {
    renderEnglish(<SemanticBadge state={state} reason="collector evidence" />);
    const badge = screen.getByLabelText(`${label}: collector evidence`);
    expect(badge.textContent).toContain(label);
    expect(badge.textContent).toContain("collector evidence");
  },
);

test("null is an em dash with its exact reason and never a zero", () => {
  renderEnglish(<SemanticBadge state="null" reason="permission denied" />);
  const badge = screen.getByLabelText("No value: permission denied");
  expect(badge.textContent).toContain("—");
  expect(badge.textContent).toContain("permission denied");
  expect(badge.textContent).not.toContain("0");
});

test("a semantic kind without a problem state stays visually quiet", () => {
  renderEnglish(<SemanticBadge kind="G" />);
  const badge = screen.getByLabelText("G: Gauge");
  expect(badge.getAttribute("data-state")).toBe("complete");
  expect(badge.style.color).toBe("var(--fg-dim)");
});
