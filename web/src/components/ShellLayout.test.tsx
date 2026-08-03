import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { ShellLayout } from "./ShellLayout";

function renderLayout(mobile = false) {
  return render(
    <ShellLayout
      mobile={mobile}
      globalContext={<span>global context</span>}
      primaryNavigation={mobile ? null : <span>primary navigation</span>}
      primaryNavigationLabel="Primary navigation"
      status={<span>status context</span>}
    >
      <section>evidence</section>
    </ShellLayout>,
  );
}

test("provides semantic desktop header, navigation, main, and footer regions", () => {
  renderLayout();

  const header = screen.getByRole("banner");
  const navigation = screen.getByRole("navigation", {
    name: "Primary navigation",
  });
  const main = screen.getByRole("main");
  const footer = screen.getByRole("contentinfo");

  expect(header.dataset.shellRegion).toBe("global-context");
  expect(navigation.dataset.shellRegion).toBe("primary-navigation");
  expect(main.dataset.shellRegion).toBe("main");
  expect(footer.dataset.shellRegion).toBe("status");
  expect(header.style.height).toBe("44px");
  expect(navigation.style.height).toBe("32px");
  expect(footer.style.height).toBe("24px");
  expect(main.style.overflow).toBe("hidden");
  expect(main.style.display).toBe("flex");
  expect(footer.tabIndex).toBe(0);
});

test("keeps mobile incident triage in normal document flow", () => {
  renderLayout(true);

  const shell = screen.getByTestId("app-shell");
  const main = screen.getByRole("main");
  expect(shell.dataset.shellLayout).toBe("mobile");
  expect(shell.style.minHeight).toBe("");
  expect(shell.style.display).toBe("block");
  expect(main.style.overflow).toBe("visible");
  expect(screen.queryByRole("navigation")).toBeNull();
  expect(screen.getByText("evidence")).toBeDefined();
});
