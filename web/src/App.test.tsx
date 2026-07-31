import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  makeContextResponse,
  makeDataQualityResponse,
  makeEntityHistoryResponse,
  makeEventsResponse,
  makeFrameResponse,
  makeHeatmapQuality,
  makeIncidentsResponse,
  makeSpineResponse,
  makeStorageResponse,
  makeViewSpec,
  makeViewSummaryItem,
  makeViewSummaryResponse,
} from "./testkit/apiFixtures";
import { App } from "./App";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  history.replaceState(null, "", location.pathname);
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

const catalogBody = {
  revision: 1,
  views: [
    makeViewSpec({ code: "activity", view_code: 1 }),
    makeViewSpec({ code: "statements", view_code: 2 }),
    makeViewSpec({ code: "locks", view_code: 3 }),
  ],
};

const heatmapBody = {
  grid: { from_us: "0", to_us: "4", bucket_count: 4 },
  ranking: { exact: true, unseen_upper: 0 },
  rows: [],
  quality: makeHeatmapQuality(),
};

function stubFetch() {
  return vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    const path = new URL(url).pathname;
    let body: unknown;
    if (path === "/v1/ui/catalog") body = catalogBody;
    else if (path === "/v1/views/summary")
      body = makeViewSummaryResponse({
        views: [makeViewSummaryItem({ view: "activity" })],
      });
    else if (path === "/v1/ui/context") body = makeContextResponse();
    else if (path === "/v1/incidents") body = makeIncidentsResponse();
    else if (path === "/v1/timeline/spine") body = makeSpineResponse();
    else if (path === "/v1/timeline/events") body = makeEventsResponse();
    else if (path === "/v1/timeline/heatmap") body = heatmapBody;
    else if (path.startsWith("/v1/frame/")) body = makeFrameResponse();
    else if (path === "/v1/data/quality") body = makeDataQualityResponse();
    else if (path === "/v1/storage") body = makeStorageResponse();
    else if (path.startsWith("/v1/entity/")) body = makeEntityHistoryResponse();
    else return Promise.resolve(new Response(null, { status: 404 }));
    return Promise.resolve(jsonResponse(body));
  });
}

function summaryFetchCount(): number {
  return (vi.mocked(globalThis.fetch).mock.calls as unknown[][]).filter((c) => {
    const u =
      typeof c[0] === "string"
        ? c[0]
        : c[0] instanceof Request
          ? c[0].url
          : String(c[0]);
    return u.includes("/v1/views/summary");
  }).length;
}

function renderApp() {
  vi.stubGlobal("fetch", stubFetch());
  return render(<App />);
}

test("renders the shell regions from fixtures", async () => {
  renderApp();
  expect(screen.getByTestId("app-shell")).toBeDefined();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.activity").length).toBeGreaterThan(0),
  );
  expect(screen.getByTestId("instance-chip")).toBeDefined();
  expect(screen.getByLabelText("spine.caption")).toBeDefined();
  expect(screen.getByText(/statusbar\.hints/)).toBeDefined();
});

test("digit key selects the nth catalog view", async () => {
  renderApp();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.locks").length).toBeGreaterThan(0),
  );
  fireEvent.keyDown(window, { key: "3" });
  expect(location.hash).toContain("view=locks");
});

test("space toggles LIVE: the hash gains and then loses the cursor", async () => {
  renderApp();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.activity").length).toBeGreaterThan(0),
  );
  expect(location.hash).not.toContain("at=");
  fireEvent.keyDown(window, { key: " " });
  expect(location.hash).toContain("at=");
  fireEvent.keyDown(window, { key: " " });
  expect(location.hash).not.toContain("at=");
});

test("Escape closes an open dock", async () => {
  history.replaceState(null, "", `${location.pathname}#dock=incidents`);
  renderApp();
  await waitFor(() =>
    expect(screen.getByLabelText("dock.title")).toBeDefined(),
  );
  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => expect(screen.queryByLabelText("dock.title")).toBeNull());
  expect(location.hash).not.toContain("dock=");
});

test("hashchange re-parses the state", async () => {
  renderApp();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.locks").length).toBeGreaterThan(0),
  );
  act(() => {
    location.hash = "#view=locks";
    window.dispatchEvent(new Event("hashchange"));
  });
  await waitFor(() => {
    const tab = screen.getAllByText("tabs.locks")[0]?.closest("[role=tab]");
    expect(tab?.getAttribute("aria-selected")).toBe("true");
  });
});

test("arrow keys step the cursor; shift+arrow jumps an hour", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=1722400000000000`,
  );
  renderApp();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.activity").length).toBeGreaterThan(0),
  );
  const before = new URLSearchParams(location.hash.slice(1)).get("at");
  fireEvent.keyDown(window, { key: "ArrowRight" });
  const stepped = new URLSearchParams(location.hash.slice(1)).get("at");
  expect(BigInt(stepped ?? "0") > BigInt(before ?? "0")).toBe(true);
  fireEvent.keyDown(window, { key: "ArrowLeft", shiftKey: true });
  const jumped = new URLSearchParams(location.hash.slice(1)).get("at");
  expect(BigInt(before ?? "0") - BigInt(jumped ?? "0")).toBe(
    3_600_000_000n - 3_600_000_000n / 48n,
  );
});

test("arrow keys on the spine slider step by its own delta, not the global one", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=1722400000000000`,
  );
  renderApp();
  const slider = await screen.findByRole("slider");
  fireEvent.keyDown(slider, { key: "ArrowRight" });
  const stepped = new URLSearchParams(location.hash.slice(1)).get("at");
  // The slider owns the key (5 min step); the global handler must not apply
  // its span/48 step on top — exactly one patch, slider-sized.
  expect(BigInt(stepped ?? "0") - 1_722_400_000_000_000n).toBe(300_000_000n);
});

test("Enter on a focused button belongs to the button, not global shortcuts", async () => {
  renderApp();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.activity").length).toBeGreaterThan(0),
  );
  const hashBefore = location.hash;
  const button = screen.getByRole("button", { name: /header.copyLink/ });
  button.focus();
  fireEvent.keyDown(button, { key: "Enter" });
  expect(location.hash).toBe(hashBefore);
});

test("LIVE cursor advances on the tick, not on every render", async () => {
  vi.useFakeTimers();
  renderApp();
  // Let the initial queries settle.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(50);
  });
  const summaryCalls0 = summaryFetchCount();
  expect(summaryCalls0).toBeGreaterThan(0);
  // A few seconds of unrelated renders must not refetch summary: `at` is
  // pinned to the tick, not recomputed per render.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(2_000);
  });
  expect(summaryFetchCount()).toBe(summaryCalls0);
  // The 15 s LIVE tick pins a new `at` and re-queries.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(15_000);
  });
  expect(summaryFetchCount()).toBeGreaterThan(summaryCalls0);
  vi.useRealTimers();
});
