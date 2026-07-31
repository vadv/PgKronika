import { render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "./App";

afterEach(() => vi.unstubAllGlobals());

test("renders app shell placeholder", () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"revision":1,"views":[]}', {
      status: 200,
      headers: { "content-type": "application/json" },
    }),
  ));
  render(<App />);
  expect(screen.getByTestId("app-shell")).toBeDefined();
});
