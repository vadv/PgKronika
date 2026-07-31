import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { DataQualityResponse, StorageResponse } from "../api/types";
import {
  makeDataQualityResponse,
  makeStorageResponse,
} from "../testkit/apiFixtures";
import { DataHealthPopover } from "./DataHealthPopover";

let dqFixture: DataQualityResponse = makeDataQualityResponse();
let storageFixture: StorageResponse = makeStorageResponse();

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function stubFetch() {
  return vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    const body = url.includes("/v1/storage") ? storageFixture : dqFixture;
    return Promise.resolve(jsonResponse(body));
  });
}

function renderPopover() {
  vi.stubGlobal("fetch", stubFetch());
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return render(
    <DataHealthPopover from="1722400000000000" to="1722403600000000" />,
    { wrapper },
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  dqFixture = makeDataQualityResponse();
  storageFixture = makeStorageResponse();
});

test("renders freshness, coverage and gaps from the data quality response", async () => {
  dqFixture = makeDataQualityResponse({
    freshness: {
      state: "stale",
      age_us: "5000000",
      data_through_us: "1722400000000000",
      expected_period_us: null,
    },
    coverage: {
      observed_snapshots: 5,
      expected_snapshots: 7,
      complete_snapshots: 4,
    },
    gaps: [
      {
        from_us: "1722400000000000",
        to_us: "1722400300000000",
        reason: "collector restart",
      },
    ],
  });
  const { container } = renderPopover();
  await waitFor(() =>
    expect(screen.getByText(/popover\.freshness/)).toBeDefined(),
  );
  const text = container.textContent ?? "";
  expect(text).toContain("stale");
  expect(text).toContain("popover.age 5s");
  expect(text).toContain(new Date(1_722_400_000_000).toISOString());
  expect(text).toContain("5/7");
  expect(text).toContain("popover.complete: 4");
  expect(text).toContain("collector restart");
});

test("shows honest dashes for null freshness and coverage fields", async () => {
  dqFixture = makeDataQualityResponse({
    coverage: {
      observed_snapshots: 3,
      expected_snapshots: null,
      complete_snapshots: 3,
    },
  });
  const { container } = renderPopover();
  await waitFor(() =>
    expect(screen.getByText(/popover\.freshness/)).toBeDefined(),
  );
  const text = container.textContent ?? "";
  expect(text).toContain("popover.age —");
  expect(text).toContain("popover.dataThrough —");
  expect(text).toContain("3/—");
  expect(text).toContain("popover.none");
});

test("lists skipped lenses with reasons, hides available ones", async () => {
  dqFixture = makeDataQualityResponse({
    capabilities: [
      {
        code: "lens_pgss",
        kind: "lens",
        status: "unavailable",
        reason: "extension missing",
      },
      { code: "lens_os", kind: "lens", status: "available", reason: null },
    ],
  });
  const { container } = renderPopover();
  await waitFor(() =>
    expect(screen.getByText(/lens_pgss: extension missing/)).toBeDefined(),
  );
  const text = container.textContent ?? "";
  expect(text).toContain("popover.lensesSkipped");
  expect(text).not.toContain("lens_os");
});

test("renders storage, retention, forecast and integrity sections", async () => {
  storageFixture = makeStorageResponse({
    filesystem: {
      total_bytes: 10 * 2 ** 30,
      available_bytes: 4 * 2 ** 30,
      used_fraction: 0.6,
    },
    retention: {
      status: "ok",
      configured_limit: null,
      effective_limit_bytes: 2 ** 30,
      mode: "size",
      reason: null,
    },
    forecast: {
      full_in_days: 12,
      full_in_days_reason: null,
      window_us: "86400000000",
      write_rate_bytes_per_day: 1000,
    },
    integrity: {
      orphan_overviews: 1,
      quarantined_entries: 2,
      readable_segments: 9,
    },
  });
  const { container } = renderPopover();
  await waitFor(() =>
    expect(screen.getByText(/6\.0 GiB \/ 10\.0 GiB/)).toBeDefined(),
  );
  const text = container.textContent ?? "";
  expect(text).toContain("ok · 1.0 GiB");
  expect(text).toContain("popover.fullIn");
  expect(text).toContain("12");
  expect(text).toContain(
    "popover.readable 9 · popover.quarantined 2 · popover.orphans 1",
  );
});

test("shows an honest dash with the reason when the forecast is null", async () => {
  storageFixture = makeStorageResponse({
    forecast: {
      full_in_days: null,
      full_in_days_reason: "not enough history",
      window_us: "0",
      write_rate_bytes_per_day: null,
    },
  });
  renderPopover();
  await waitFor(() =>
    expect(screen.getByText(/popover\.fullIn/)).toBeDefined(),
  );
  const value = await screen.findByTitle("not enough history");
  expect(value.textContent).toBe("—");
});
