import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { App } from "./App";

test("renders app shell placeholder", () => {
  render(<App />);
  expect(screen.getByTestId("app-shell")).toBeDefined();
});
