import { expect, test } from "vitest";
import en from "./en.json";
import ru from "./ru.json";

test("ru and en dictionaries have identical keys", () => {
  expect(Object.keys(ru).sort()).toEqual(Object.keys(en).sort());
});
