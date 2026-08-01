import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { TabBadge } from "./TabBadge";

test("renders population for available view", () => {
  render(<TabBadge population={500} status="complete" notable={false} />);
  expect(screen.getByText("500")).toBeDefined();
});

test("renders nothing for null population without notables", () => {
  const { container } = render(
    <TabBadge population={null} status="unavailable" notable={false} />,
  );
  expect(container.firstChild).toBeNull();
});

test("null population with notables gets the warning mark, no dash", () => {
  render(<TabBadge population={null} status="unavailable" notable={true} />);
  expect(screen.getByText("!")).toBeDefined();
});

test("notable view gets accent marker", () => {
  const { container } = render(
    <TabBadge population={3} status="complete" notable={true} />,
  );
  expect(container.querySelector("[data-notable='true']")).not.toBeNull();
});
