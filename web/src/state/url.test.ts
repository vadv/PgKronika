import { expect, test } from "vitest";
import { isTimestampUs, parseHash, SPANS, toHash, type UiState } from "./url";

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

test("invalid and out-of-int64 timestamps fall back to null", () => {
  for (const bad of [
    "abc",
    "1e15",
    "12.5",
    "2024-01-01",
    "",
    " ",
    "+1722400000000000",
    "9223372036854775808",
    "-9223372036854775809",
  ]) {
    expect(parseHash(`#at=${encodeURIComponent(bad)}`).at).toBeNull();
    expect(
      parseHash(`#baseline=${encodeURIComponent(bad)}`).baseline,
    ).toBeNull();
  }
  expect(parseHash("#at=1722400000000000").at).toBe("1722400000000000");
  expect(parseHash("#baseline=-3600000000").baseline).toBe("-3600000000");
  expect(parseHash("#at=9223372036854775807").at).toBe("9223372036854775807");
  expect(parseHash("#at=-9223372036854775808").at).toBe("-9223372036854775808");
});

test("rejects overlong decimal input before BigInt parsing", () => {
  expect(isTimestampUs("9".repeat(100_000))).toBe(false);
  expect(isTimestampUs(`-${"0".repeat(100_000)}`)).toBe(false);
});

test("source is not part of the URL contract", () => {
  const parsed = parseHash("#source=prod-1&view=locks");
  expect("source" in parsed).toBe(false);
  expect(parsed.view).toBe("locks");
});

test("defaults when hash empty", () => {
  expect(parseHash("")).toEqual(fullState({ view: "activity", at: null }));
});

test("accepts integer spans from one second through 24 hours", () => {
  for (const span of [1, 37, 899, 900, 3600, 21600, 86400]) {
    expect(parseHash(`#span=${span}`).span).toBe(span);
    expect(parseHash(toHash(fullState({ span }))).span).toBe(span);
  }
});

test("keeps the four prepared span controls", () => {
  expect(SPANS).toEqual([900, 3600, 21600, 86400]);
});

test("rejects invalid spans and invalid enum values", () => {
  for (const span of ["0", "-1", "86401", "1.5", "1e3", "", " "]) {
    expect(parseHash(`#span=${encodeURIComponent(span)}`).span).toBe(3600);
  }
  const parsed = parseHash("#order=sideways&dock=panel");
  expect(parsed.order).toBeNull();
  expect(parsed.dock).toBeNull();
});

test("default span is omitted from the hash", () => {
  expect(toHash(fullState())).not.toContain("span");
});
