import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBadge } from "./TabBadge";

test("renders population for available view", () => {
  render(<TabBadge population={500} status="complete" notable={false} />);
  expect(screen.getByText("500")).toBeDefined();
});

test("renders em-dash for null population (gated)", () => {
  render(<TabBadge population={null} status="unavailable" notable={false} />);
  expect(screen.getByText("—")).toBeDefined();
});

test("notable view gets accent marker", () => {
  const { container } = render(
    <TabBadge population={3} status="complete" notable={true} />,
  );
  expect(container.querySelector("[data-notable='true']")).not.toBeNull();
});
