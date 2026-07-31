import { expect, test } from "vitest";
import { parseHash, toHash, type UiState } from "./url";

function fullState(overrides: Partial<UiState> = {}): UiState {
  return {
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
    sort: "total_time",
    order: "desc",
    focus: "inc-1",
    dock: "row",
    entity: "77de",
  });
  expect(parseHash(toHash(state))).toEqual(state);
});

test("q is transient: neither parsed nor serialized", () => {
  expect(parseHash("#q=calls%3E100").q).toBeNull();
  const state = fullState({ q: "calls>100" });
  expect(toHash(state)).not.toContain("q=");
  expect(parseHash(toHash(state))).toEqual(fullState({ q: null }));
});

test("invalid at/baseline fall back to null instead of crashing BigInt", () => {
  for (const bad of ["abc", "1e15", "12.5", "2024-01-01", "", " "]) {
    expect(parseHash(`#at=${encodeURIComponent(bad)}`).at).toBeNull();
    expect(
      parseHash(`#baseline=${encodeURIComponent(bad)}`).baseline,
    ).toBeNull();
  }
  expect(parseHash("#at=1722400000000000").at).toBe("1722400000000000");
  expect(parseHash("#baseline=-3600000000").baseline).toBe("-3600000000");
});

test("source is not part of the URL contract", () => {
  const parsed = parseHash("#source=prod-1&view=locks");
  expect("source" in parsed).toBe(false);
  expect(parsed.view).toBe("locks");
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
