import { expect, test } from "vitest";
import { parseHash, toHash, type UiState } from "./url";

function fullState(overrides: Partial<UiState> = {}): UiState {
  return {
    source: "local",
    view: "statements",
    at: "1722400000000000",
    span: 3600,
    baseline: null,
    preset: null,
    q: null,
    sort: null,
    order: null,
    focus: null,
    dock: null,
    entity: null,
    ...overrides,
  };
}

test("roundtrips minimal state", () => {
  const state = fullState();
  expect(parseHash(toHash(state))).toEqual(state);
});

test("roundtrips fully populated state", () => {
  const state = fullState({
    span: 900,
    baseline: "1722390000000000",
    preset: "io",
    q: "calls>100",
    sort: "total_time",
    order: "desc",
    focus: "inc-1",
    dock: "row",
    entity: "77de",
  });
  expect(parseHash(toHash(state))).toEqual(state);
});

test("defaults when hash empty", () => {
  expect(parseHash("")).toEqual(fullState({ view: "activity", at: null }));
});

test("rejects out-of-list span and invalid enum values", () => {
  const parsed = parseHash("#span=123&order=sideways&dock=panel");
  expect(parsed.span).toBe(3600);
  expect(parsed.order).toBeNull();
  expect(parsed.dock).toBeNull();
});

test("default span is omitted from the hash", () => {
  expect(toHash(fullState())).not.toContain("span");
});
