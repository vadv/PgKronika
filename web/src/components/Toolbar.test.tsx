import { act, fireEvent, render, screen } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import { makeViewSpec } from "../testkit/apiFixtures";
import { Toolbar, type ToolbarProps } from "./Toolbar";

function renderToolbar(overrides: Partial<ToolbarProps> = {}) {
  const props: ToolbarProps = {
    view: makeViewSpec({
      presets: [
        { code: "cpu", columns: [], sort: { column: "cpu", order: "desc" } },
        { code: "io", columns: [], sort: { column: "io", order: "desc" } },
      ],
    }),
    preset: null,
    q: null,
    matched: null,
    onSelectPreset: () => {},
    onFilter: () => {},
    ...overrides,
  };
  return render(<Toolbar {...props} />);
}

test("clicking a preset selects it, clicking the active one clears it", () => {
  const onSelectPreset = vi.fn();
  const { unmount } = renderToolbar({ onSelectPreset });
  fireEvent.click(screen.getByRole("button", { name: "cpu" }));
  expect(onSelectPreset).toHaveBeenCalledWith("cpu");
  unmount();

  onSelectPreset.mockClear();
  renderToolbar({ onSelectPreset, preset: "cpu" });
  const active = screen.getByRole("button", { name: "cpu" });
  expect(active.getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(active);
  expect(onSelectPreset).toHaveBeenCalledWith(null);
});

test("enter applies the trimmed filter, blank clears it", () => {
  const onFilter = vi.fn();
  renderToolbar({ onFilter });
  const input = screen.getByRole("searchbox");
  expect(input.getAttribute("name")).toBe("view-filter");
  expect(input.getAttribute("autocomplete")).toBe("off");
  expect(input.getAttribute("spellcheck")).toBe("false");
  fireEvent.change(input, { target: { value: "  active  " } });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(onFilter).toHaveBeenCalledWith("active");

  fireEvent.change(input, { target: { value: "   " } });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(onFilter).toHaveBeenCalledWith(null);
});

test("external q changes replace the draft", () => {
  const { rerender } = render(
    <Toolbar
      view={makeViewSpec()}
      preset={null}
      q={null}
      matched={null}
      onSelectPreset={() => {}}
      onFilter={() => {}}
    />,
  );
  const input = screen.getByRole("searchbox");
  fireEvent.change(input, { target: { value: "draft" } });
  rerender(
    <Toolbar
      view={makeViewSpec()}
      preset={null}
      q="from-url"
      matched={null}
      onSelectPreset={() => {}}
      onFilter={() => {}}
    />,
  );
  expect((input as HTMLInputElement).value).toBe("from-url");
});

test("shows the matched row count when provided", () => {
  const { container, unmount } = renderToolbar();
  expect(container.textContent).not.toContain("toolbar.rows");
  unmount();
  renderToolbar({ matched: 42 });
  expect(screen.getByText(/toolbar\.rows/)).toBeDefined();
});

test("prepared lenses map display names to presets and expose gated reasons", () => {
  const onSelectPreset = vi.fn();
  renderToolbar({
    preset: "time",
    onSelectPreset,
    lenses: [
      { code: "workload", preset: "time", availability: "available" },
      {
        code: "regression",
        preset: null,
        availability: "gated",
        reason: "baseline deltas are not projected",
      },
    ],
    contextNote: "reset-aware · query text not collected",
    filterHint: "field=value · full decimal queryid",
  });

  const workload = screen.getByRole("button", { name: "workload" });
  expect(workload.getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(workload);
  expect(onSelectPreset).not.toHaveBeenCalled();

  const regression = screen.getByRole("button", {
    name: /regression/,
  });
  expect((regression as HTMLButtonElement).disabled).toBe(false);
  expect(regression.getAttribute("aria-disabled")).toBe("true");
  expect(regression.getAttribute("title")).toContain(
    "baseline deltas are not projected",
  );
  act(() => regression.focus());
  expect(document.activeElement).toBe(regression);
  expect(screen.getByRole("status").textContent).toContain(
    "baseline deltas are not projected",
  );
  onSelectPreset.mockClear();
  fireEvent.click(regression);
  expect(onSelectPreset).not.toHaveBeenCalled();
  act(() => regression.blur());
  expect(screen.getByText(/reset-aware/)).toBeDefined();
  expect(screen.getByRole("searchbox").getAttribute("title")).toContain(
    "full decimal queryid",
  );
});
