import {
  act,
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

afterEach(() => vi.unstubAllGlobals());

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

function renderApp() {
  vi.stubGlobal("fetch", stubFetch());
  return render(<App />);
}

test("renders the shell regions from fixtures", async () => {
  renderApp();
  expect(screen.getByTestId("app-shell")).toBeDefined();
  await waitFor(() => expect(screen.getByText("tabs.activity")).toBeDefined());
  expect(screen.getByTestId("instance-chip")).toBeDefined();
  expect(screen.getByLabelText("spine.caption")).toBeDefined();
  expect(screen.getByText(/statusbar\.hints/)).toBeDefined();
});

test("digit key selects the nth catalog view", async () => {
  renderApp();
  await waitFor(() => expect(screen.getByText("tabs.locks")).toBeDefined());
  fireEvent.keyDown(window, { key: "3" });
  expect(location.hash).toContain("view=locks");
});

test("space toggles LIVE: the hash gains and then loses the cursor", async () => {
  renderApp();
  await waitFor(() => expect(screen.getByText("tabs.activity")).toBeDefined());
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
  await waitFor(() => expect(screen.getByText("tabs.locks")).toBeDefined());
  act(() => {
    location.hash = "#view=locks";
    window.dispatchEvent(new Event("hashchange"));
  });
  await waitFor(() => {
    const tab = screen.getByText("tabs.locks").closest("[role=tab]");
    expect(tab?.getAttribute("aria-selected")).toBe("true");
  });
});
