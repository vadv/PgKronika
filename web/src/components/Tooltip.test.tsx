import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { act } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { TipRow, Tooltip } from "./Tooltip";

afterEach(() => {
  vi.useRealTimers();
});

test("tooltip opens after the hover delay and closes on leave", async () => {
  vi.useFakeTimers();
  render(
    <Tooltip content={<span>hint-body</span>}>
      <button type="button">anchor</button>
    </Tooltip>,
  );
  const anchor = screen.getByRole("button", { name: "anchor" });
  expect(screen.queryByRole("tooltip")).toBeNull();
  fireEvent.mouseEnter(anchor);
  // The hover delay is 250 ms; advancing past it opens the tip synchronously.
  act(() => {
    vi.advanceTimersByTime(300);
  });
  expect(screen.getByRole("tooltip").textContent).toContain("hint-body");
  fireEvent.mouseLeave(anchor);
  expect(screen.queryByRole("tooltip")).toBeNull();
});

test("tooltip opens on focus for keyboard users", async () => {
  render(
    <Tooltip content={<span>focus-hint</span>}>
      <button type="button">anchor</button>
    </Tooltip>,
  );
  fireEvent.focus(screen.getByRole("button", { name: "anchor" }));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  expect(screen.getByRole("tooltip").textContent).toContain("focus-hint");
  fireEvent.blur(screen.getByRole("button", { name: "anchor" }));
  expect(screen.queryByRole("tooltip")).toBeNull();
});

test("preferAbove flips the tip above the anchor", async () => {
  render(
    <Tooltip content={<span>above-hint</span>} preferAbove>
      <button type="button">anchor</button>
    </Tooltip>,
  );
  fireEvent.mouseEnter(screen.getByRole("button", { name: "anchor" }));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  const tip = screen.getByRole("tooltip");
  expect(tip.style.bottom).toBe("100%");
});

test("TipRow renders label and mono value", () => {
  render(<TipRow label="formula" value="x / y" mono />);
  const row = screen.getByText("x / y");
  expect(row.style.fontFamily).toContain("mono");
  expect(screen.getByText("formula")).toBeDefined();
});
