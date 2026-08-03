import { expect, test } from "vitest";
import type { ColumnSpec, ViewSpec } from "../api/types";
import { makeViewSpec } from "../testkit/apiFixtures";
import { compileForensicSearch } from "./compile";

function column(
  code: string,
  type: ColumnSpec["type"] = "text",
  lazy = false,
): ColumnSpec {
  return {
    code,
    type,
    lazy,
    requires: [],
    availability: "available",
  };
}

const views: ViewSpec[] = [
  makeViewSpec({
    code: "activity",
    columns: [
      column("pid", "i64"),
      column("database"),
      column("application"),
      column("query", "text", true),
    ],
  }),
  makeViewSpec({
    code: "statements",
    columns: [
      column("queryid", "i64"),
      column("database"),
      column("query", "text", true),
    ],
  }),
  makeViewSpec({
    code: "plans",
    columns: [column("planid", "i64"), column("queryid", "i64")],
  }),
  makeViewSpec({
    code: "processes",
    columns: [column("pid", "i64"), column("cgroup")],
  }),
  makeViewSpec({
    code: "events",
    columns: [column("type"), column("message")],
  }),
];

test("compiles colon aliases only for compatible server projections", () => {
  const plan = compileForensicSearch("pid:18422", views);
  expect(plan.error).toBeNull();
  expect(plan.groups.map((group) => [group.view.code, group.q])).toEqual([
    ["activity", "pid=18422"],
    ["processes", "pid=18422"],
  ]);
  expect(plan.groups.every((group) => group.reason === "pid = 18422")).toBe(
    true,
  );
});

test("AND terms retain only a view that can prove every keyed match", () => {
  const plan = compileForensicSearch("pid:18422 database:orders", views);
  expect(plan.groups.map((group) => [group.view.code, group.q])).toEqual([
    ["activity", "pid=18422 database=orders"],
  ]);
});

test("free text is a bounded substring glob and never targets lazy SQL", () => {
  const plan = compileForensicSearch('"cart items"', views);
  expect(plan.groups).toHaveLength(5);
  expect(plan.groups[0]?.q).toBe('"*cart items*"');
  expect(plan.groups[0]?.reason).toBe("materialized fields contain cart items");
});

test("reports public-field gaps instead of fabricating OID or device matches", () => {
  for (const query of ["oid:16402", "device:8:0"]) {
    const plan = compileForensicSearch(query, views);
    expect(plan.groups).toEqual([]);
    expect(plan.error?.code).toBe("unsupported_key");
    if (plan.error?.code === "unsupported_key") {
      expect(plan.error.key).toBe(query.split(":", 1)[0]);
    }
  }
});

test("rejects unknown keys, empty values, more than 16 terms and over 256 bytes", () => {
  expect(compileForensicSearch("wat:value", views).error?.code).toBe(
    "unsupported_key",
  );
  expect(compileForensicSearch("pid:", views).error?.code).toBe(
    "invalid_query",
  );
  expect(
    compileForensicSearch(
      Array.from({ length: 17 }, (_, index) => `x${index}`).join(" "),
      views,
    ).error?.code,
  ).toBe("too_many_terms");
  expect(compileForensicSearch("x".repeat(257), views).error?.code).toBe(
    "query_too_long",
  );
});

test("quotes values with the server's field/term separators so they stay literal", () => {
  const keyed = compileForensicSearch("app:web=prod", views);
  expect(keyed.groups.map((group) => group.q)).toEqual([
    'application="web=prod"',
  ]);
  const free = compileForensicSearch("shared_buffers=128MB", views);
  expect(free.error).toBeNull();
  expect(free.groups[0]?.q).toBe('"*shared_buffers=128MB*"');
});

test("excludes unavailable views and preserves explicit user globs", () => {
  const unavailable = makeViewSpec({
    code: "activity",
    availability: "not_collected",
    columns: [column("application")],
  });
  const plan = compileForensicSearch("app:web-*", [unavailable, ...views]);
  expect(plan.groups).toEqual([
    expect.objectContaining({ view: views[0], q: "application=web-*" }),
  ]);
});
