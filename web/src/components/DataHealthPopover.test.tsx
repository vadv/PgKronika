import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import {
  makeSummaryQuality,
  makeViewSummaryItem,
} from "../testkit/apiFixtures";
import { DataHealthPopover } from "./DataHealthPopover";

test("renders freshness, coverage and gaps", () => {
  render(
    <DataHealthPopover
      quality={makeSummaryQuality({
        status: "partial",
        snapshots: 5,
        gaps: ["10:00→10:05"],
        active_tail: true,
      })}
      views={[]}
    />,
  );
  expect(screen.getByText(/popover\.freshness/)).toBeDefined();
  expect(screen.getByText(/partial/)).toBeDefined();
  expect(screen.getByText(/popover\.activeTail/)).toBeDefined();
  expect(screen.getByText(/5/)).toBeDefined();
  expect(screen.getByText("10:00→10:05")).toBeDefined();
});

test("lists skipped codes for gated and resource limited", () => {
  render(
    <DataHealthPopover
      quality={makeSummaryQuality({
        gated: ["pg_stat_statements"],
        resource_limited: ["pg_stat_wal"],
        unavailable_revision: ["pg_stat_io"],
      })}
      views={[]}
    />,
  );
  expect(screen.getByText("pg_stat_statements")).toBeDefined();
  expect(screen.getByText("pg_stat_wal")).toBeDefined();
  expect(screen.getByText("pg_stat_io")).toBeDefined();
});

test("summarizes collected totals and the worst three views", () => {
  const { container } = render(
    <DataHealthPopover
      quality={makeSummaryQuality()}
      views={[
        makeViewSummaryItem({
          view: "a",
          collection: {
            collected: 10,
            source_total: 10,
            read_state: "ok",
            visibility: "visible",
          },
        }),
        makeViewSummaryItem({
          view: "b",
          collection: {
            collected: 2,
            source_total: 10,
            read_state: "ok",
            visibility: "visible",
          },
        }),
        makeViewSummaryItem({
          view: "c",
          collection: {
            collected: 5,
            source_total: 10,
            read_state: "ok",
            visibility: "visible",
          },
        }),
        makeViewSummaryItem({
          view: "d",
          collection: {
            collected: 9,
            source_total: 10,
            read_state: "ok",
            visibility: "visible",
          },
        }),
        makeViewSummaryItem({ view: "e", collection: null }),
      ]}
    />,
  );
  // totals: 10+2+5+9 = 26 of 40
  expect(screen.getByText("26/40")).toBeDefined();
  const text = container.textContent ?? "";
  // worst three by ratio: b (0.2), c (0.5), d (0.9); a (1.0) excluded
  for (const v of ["b", "c", "d"]) {
    expect(text).toContain(`popover.worst: ${v}`);
  }
  expect(text).not.toContain("popover.worst: a");
});
