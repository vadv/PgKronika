import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBar } from "./TabBar";
import type { ViewSpec } from "../api/types";

const views = [
  { code: "activity", availability: "available" },
  { code: "statements", availability: "gated" },
] as unknown as ViewSpec[];

test("renders one tab per catalog view, gated dimmed", () => {
  render(<TabBar views={views} active="activity" onSelect={() => {}} />);
  expect(screen.getByRole("tab", { name: /activity/i })).toBeDefined();
  const gated = screen.getByRole("tab", { name: /statements/i });
  expect(gated.getAttribute("aria-disabled")).toBe("true");
});
