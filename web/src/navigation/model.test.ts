import { expect, test } from "vitest";
import { makeViewSpec } from "../testkit/apiFixtures";
import {
  availableDestinations,
  buildNavigationGroups,
  destinationForView,
} from "./model";

const catalogViews = [
  makeViewSpec({ code: "events", availability: "available" }),
  makeViewSpec({ code: "locks", availability: "available" }),
  makeViewSpec({ code: "processes", availability: "available" }),
  makeViewSpec({ code: "vacuum", availability: "not_collected" }),
  makeViewSpec({ code: "indexes", availability: "available" }),
  makeViewSpec({ code: "tables", availability: "available" }),
  makeViewSpec({ code: "plans", availability: "unsupported_type" }),
  makeViewSpec({ code: "statements", availability: "gated" }),
  makeViewSpec({ code: "activity", availability: "available" }),
];

test("builds the approved groups and destinations independently of catalog order", () => {
  const groups = buildNavigationGroups(catalogViews);

  expect(groups.map((group) => group.id)).toEqual([
    "workload",
    "data",
    "host",
    "events",
  ]);
  expect(groups.map((group) => group.labelKey)).toEqual([
    "navigation.group.workload",
    "navigation.group.data",
    "navigation.group.host",
    "navigation.group.events",
  ]);
  expect(
    groups.map((group) =>
      group.destinations.map((destination) => destination.id),
    ),
  ).toEqual([
    ["activity", "statements", "plans"],
    ["tables", "indexes", "vacuum"],
    ["os"],
    ["events"],
  ]);
});

test("propagates catalog availability and maps OS to the processes API view", () => {
  const groups = buildNavigationGroups(catalogViews);
  const destinations = groups.flatMap((group) => group.destinations);

  expect(
    destinations.map(({ id, viewCode, availability }) => ({
      id,
      viewCode,
      availability,
    })),
  ).toEqual([
    { id: "activity", viewCode: "activity", availability: "available" },
    { id: "statements", viewCode: "statements", availability: "gated" },
    { id: "plans", viewCode: "plans", availability: "unsupported_type" },
    { id: "tables", viewCode: "tables", availability: "available" },
    { id: "indexes", viewCode: "indexes", availability: "available" },
    { id: "vacuum", viewCode: "vacuum", availability: "not_collected" },
    { id: "os", viewCode: "processes", availability: "available" },
    { id: "events", viewCode: "events", availability: "available" },
  ]);
  expect(destinations.map(({ id }) => String(id))).not.toContain("processes");
  expect(destinations.map(({ id }) => String(id))).not.toContain("locks");
});

test("keeps deep-link selection honest and numbers only visible available destinations", () => {
  const groups = buildNavigationGroups(catalogViews);

  expect(destinationForView(groups, "processes")?.id).toBe("os");
  expect(destinationForView(groups, "locks")).toBeNull();
  expect(availableDestinations(groups).map(({ id }) => id)).toEqual([
    "activity",
    "tables",
    "indexes",
    "os",
    "events",
  ]);
});

test("a missing catalog view stays visible but unavailable", () => {
  const groups = buildNavigationGroups(
    catalogViews.filter(({ code }) => code !== "indexes"),
  );
  const indexes = groups
    .flatMap((group) => group.destinations)
    .find(({ id }) => id === "indexes");

  expect(indexes?.availability).toBe("not_collected");
  expect(indexes?.catalogView).toBeNull();
});
