import { expect, test } from "vitest";
import { makeEventFact } from "../testkit/apiFixtures";
import { eventMatchesQuery } from "./EventsSignalPanel";
import { groupEventFamilies } from "./EventsWorkspace";

test("event family ranking preserves occurrence density and strongest tone", () => {
  const groups = groupEventFamilies([
    makeEventFact({
      event_instance_id: "deadlock-1",
      event_kind: "pg.database.deadlock_delta",
      occurrence_count: 5,
    }),
    makeEventFact({
      event_instance_id: "deadlock-2",
      event_kind: "pg.database.deadlock_delta",
      occurrence_count: 2,
    }),
    makeEventFact({
      event_instance_id: "checkpoint-1",
      event_kind: "pg.checkpoint.completed",
      occurrence_count: 3,
    }),
  ]);

  expect(groups).toEqual([
    {
      code: "pg.database.deadlock_delta",
      count: 7,
      facts: 2,
      tone: "crit",
    },
    {
      code: "pg.checkpoint.completed",
      count: 3,
      facts: 1,
      tone: "info",
    },
  ]);
});

test("typed event filters override the prepared family lens client-side", () => {
  const fatal = makeEventFact({
    event_kind: "pg.log.error_group_observed",
    payload: {
      kind: "error",
      category: "internal",
      severity: "fatal",
      sqlstate: null,
      dropped_field_count: 0,
    },
  });
  expect(eventMatchesQuery(fatal, "severity_code=fatal")).toBe(true);
  expect(eventMatchesQuery(fatal, "category_code=pg.checkpoint.*")).toBe(false);
  expect(eventMatchesQuery(fatal, "pg.log.error")).toBe(true);
});

test("typed event filters preserve the frame glob and term grammar", () => {
  const fatal = makeEventFact({
    event_kind: "pg.log.error_group_observed",
    entity: { kind: "database", id: "orders" },
    payload: {
      kind: "error",
      category: "internal",
      severity: "fatal",
      sqlstate: null,
      dropped_field_count: 0,
    },
  });

  expect(
    eventMatchesQuery(
      fatal,
      "severity_code=fatal category_code=pg.log.* entity_kind=data?ase",
    ),
  ).toBe(true);
  expect(eventMatchesQuery(fatal, "category_code=*ERROR*")).toBe(true);
  expect(eventMatchesQuery(fatal, 'category_code="pg.log.error_*"')).toBe(true);
  expect(eventMatchesQuery(fatal, "category_code=pg.checkpoint.*")).toBe(false);
  expect(eventMatchesQuery(fatal, "unknown_field=*")).toBe(false);
});

test("typed event filters reject more than 256 UTF-8 bytes before matching", () => {
  const boundary = "x".repeat(256);
  expect(
    eventMatchesQuery(makeEventFact({ event_kind: boundary }), boundary),
  ).toBe(true);

  const oversized = "é".repeat(129);
  expect(
    eventMatchesQuery(makeEventFact({ event_kind: oversized }), oversized),
  ).toBe(false);
});
