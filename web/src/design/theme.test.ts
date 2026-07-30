import { expect, test, vi } from "vitest";
import { applyTheme, resolveTheme } from "./theme";

test("defaults to dark when system prefers dark", () => {
  window.matchMedia = vi.fn().mockReturnValue({ matches: false }) as never;
  localStorage.clear();
  expect(resolveTheme()).toBe("dark");
});

test("defaults to light when system prefers light", () => {
  window.matchMedia = vi.fn().mockReturnValue({ matches: true }) as never;
  localStorage.clear();
  expect(resolveTheme()).toBe("light");
});

test("manual choice wins over system", () => {
  localStorage.setItem("pgk-theme", "light");
  expect(resolveTheme()).toBe("light");
});

test("applyTheme sets data-theme on documentElement", () => {
  applyTheme("light");
  expect(document.documentElement.dataset.theme).toBe("light");
});
