import { expect, test } from "vitest";
import { parseHash, toHash } from "./url";

test("roundtrips state", () => {
  const state = { view: "statements", at: "1722400000000000" };
  expect(parseHash(toHash(state))).toEqual(state);
});

test("defaults when hash empty", () => {
  expect(parseHash("")).toEqual({
    view: "activity",
    at: null,
  });
});
