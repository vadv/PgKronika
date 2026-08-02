import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { useCatalog } from "./catalog";
import type { ProjectionCatalog } from "./types";

afterEach(() => vi.unstubAllGlobals());

test("demo catalog preserves the current relation provenance contract", () => {
  const fixture = JSON.parse(
    readFileSync(
      resolve(process.cwd(), "scripts/catalog.fixture.json"),
      "utf8",
    ),
  ) as ProjectionCatalog;
  const activity = fixture.views.find((view) => view.code === "activity");
  const processJoin = activity?.joins.find((join) => join.right === "process");

  expect(fixture.revision).toBe(3);
  expect(processJoin).toMatchObject({
    kind: "best_effort",
    fields: ["pid", "ts"],
    provenance: "same_snapshot_pid_only",
  });
  expect(activity?.columns.map((column) => column.code)).toEqual(
    expect.arrayContaining([
      "process_link",
      "read_bytes_per_second",
      "write_bytes_per_second",
    ]),
  );
  expect(
    fixture.views.flatMap((view) => view.joins).every((join) => join.kind),
  ).toBe(true);
});

test("useCatalog fetches catalog without query parameters", async () => {
  const body: ProjectionCatalog = { revision: 1, views: [] };
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const { result } = renderHook(() => useCatalog(), { wrapper });
  await waitFor(() => expect(result.current.isSuccess).toBe(true));
  expect(result.current.data?.revision).toBe(1);
  const req = vi.mocked(fetch).mock.calls[0]?.[0] as Request;
  expect(new URL(req.url).pathname).toBe("/v1/ui/catalog");
  expect(new URL(req.url).search).toBe("");
});
