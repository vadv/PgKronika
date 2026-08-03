import { fireEvent, render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { ShellLayout } from "./ShellLayout";

function renderLayout(mobile = false) {
  return render(
    <ShellLayout
      mobile={mobile}
      globalContext={<span>global context</span>}
      primaryNavigation={mobile ? null : <span>primary navigation</span>}
      primaryNavigationLabel="Primary navigation"
      skipToMainLabel="Skip to forensic content"
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
  expect(
    screen
      .getByRole("link", { name: "Skip to forensic content" })
      .getAttribute("href"),
  ).toBe("#main-content");
  expect(main.id).toBe("main-content");
  expect(main.tabIndex).toBe(-1);
  history.replaceState(null, "", "#view=events&at=1722400000000000");
  fireEvent.click(
    screen.getByRole("link", { name: "Skip to forensic content" }),
  );
  expect(document.activeElement).toBe(main);
  expect(location.hash).toBe("#view=events&at=1722400000000000");
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
