import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  makeContextResponse,
  makeDataQualityResponse,
  makeEntityHistoryResponse,
  makeEventsResponse,
  makeFrameResponse,
  makeHealthResponse,
  makeHeatmapQuality,
  makeIncidentsResponse,
  makeSpineResponse,
  makeStorageResponse,
  makeViewSpec,
  makeViewSummaryItem,
  makeViewSummaryResponse,
} from "./testkit/apiFixtures";
import { App, queryClient } from "./App";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  queryClient.clear();
  history.replaceState(null, "", location.pathname);
  // Tests assert localized view labels, so run with the sidebar expanded.
  localStorage.setItem("pgk-sidebar", "open");
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
    makeViewSpec({
      code: "statements",
      view_code: 2,
      availability: "gated",
      presets: [
        {
          code: "memory",
          columns: ["total"],
          sort: { column: "total", order: "desc" },
        },
        {
          code: "stmt_only",
          columns: ["total"],
          sort: { column: "total", order: "desc" },
        },
      ],
      columns: [
        {
          availability: "available",
          code: "total",
          lazy: false,
          requires: [],
          type: "f64",
        },
      ],
    }),
    makeViewSpec({
      code: "locks",
      view_code: 3,
      presets: [
        {
          code: "memory",
          columns: ["total"],
          sort: { column: "total", order: "desc" },
        },
      ],
    }),
    makeViewSpec({ code: "plans", view_code: 4 }),
    makeViewSpec({
      code: "tables",
      view_code: 5,
      presets: [
        {
          code: "memory",
          columns: ["total"],
          sort: { column: "total", order: "desc" },
        },
      ],
    }),
    makeViewSpec({ code: "indexes", view_code: 6 }),
    makeViewSpec({ code: "vacuum", view_code: 7 }),
    makeViewSpec({ code: "processes", view_code: 8 }),
    makeViewSpec({ code: "events", view_code: 9 }),
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
    else if (path === "/v1/timeline/spine")
      body = makeSpineResponse({
        series: [
          {
            code: "host.load1",
            unit: "loadavg",
            aggregation: "avg",
            values: [1, 0.5],
          },
        ],
      });
    else if (path === "/v1/timeline/health") body = makeHealthResponse();
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

function renderApp(fetchImpl = stubFetch()) {
  vi.stubGlobal("fetch", fetchImpl);
  return render(<App />);
}

test("keeps the 32px navigation and time controls on a cold catalog error", async () => {
  const baseFetch = stubFetch();
  const failedFetch = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    if (new URL(url).pathname === "/v1/ui/catalog") {
      return Promise.resolve(
        new Response(JSON.stringify({ code: "catalog_unavailable" }), {
          status: 400,
          headers: { "content-type": "application/json" },
        }),
      );
    }
    return baseFetch(input);
  });

  renderApp(failedFetch);
  await waitFor(() =>
    expect(
      failedFetch.mock.calls.some(([input]) =>
        String(input instanceof Request ? input.url : input).includes(
          "/v1/ui/catalog",
        ),
      ),
    ).toBe(true),
  );

  const navigation = screen.getByRole("navigation");
  expect(navigation.style.height).toBe("32px");
  expect(screen.getAllByRole("tab")).toHaveLength(8);
  expect(
    screen
      .getAllByRole("tab")
      .every((tab) => tab.getAttribute("aria-disabled") === "true"),
  ).toBe(true);
  expect(
    within(navigation).getByRole("button", { name: /spine\.live/i }),
  ).toBeDefined();
  expect(
    within(navigation).getByRole("button", { name: /spine\.span\.86400/i }),
  ).toBeDefined();
});

test("keeps unavailable destinations and time controls while catalog is pending", async () => {
  const baseFetch = stubFetch();
  let resolveCatalog: ((response: Response) => void) | undefined;
  const pendingCatalog = new Promise<Response>((resolve) => {
    resolveCatalog = resolve;
  });
  const pendingFetch = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? pendingCatalog
      : baseFetch(input);
  });

  renderApp(pendingFetch);
  const navigation = screen.getByRole("navigation");
  expect(navigation.style.height).toBe("32px");
  expect(screen.getAllByRole("tab")).toHaveLength(8);
  expect(
    screen
      .getAllByRole("tab")
      .every((tab) => tab.getAttribute("aria-disabled") === "true"),
  ).toBe(true);
  expect(
    within(navigation).getByRole("button", { name: /spine\.live/i }),
  ).toBeDefined();

  await act(async () => {
    resolveCatalog?.(jsonResponse(catalogBody));
  });
  await waitFor(() =>
    expect(
      screen
        .getByRole("tab", { name: /tabs\.activity/i })
        .getAttribute("aria-disabled"),
    ).toBe("false"),
  );
});

test("renders the shell regions from fixtures", async () => {
  renderApp();
  expect(screen.getByTestId("app-shell")).toBeDefined();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.activity").length).toBeGreaterThan(0),
  );
  expect(screen.getByTestId("instance-chip")).toBeDefined();
  expect(screen.getByLabelText("spine.caption")).toBeDefined();
  expect(screen.getByText(/statusbar\.hints/)).toBeDefined();
  expect(screen.getByRole("banner").dataset.shellRegion).toBe("global-context");
  expect(screen.getByRole("navigation").dataset.shellRegion).toBe(
    "primary-navigation",
  );
  expect(screen.getByRole("main").dataset.shellRegion).toBe("main");
  expect(screen.getByRole("contentinfo").dataset.shellRegion).toBe("status");
});

test("marks every desktop analytical boundary for bounded viewport layout", async () => {
  renderApp();
  await waitFor(() =>
    expect(screen.getByRole("table", { name: "activity" })).toBeDefined(),
  );

  const content = screen.getByTestId("desktop-forensic-content");
  expect(content.dataset.layoutBoundary).toBe("analytical-content");
  expect(content.style.minHeight).toBe("0");
  expect(content.style.overflow).toBe("hidden");
  expect(
    document.querySelector('[data-shell-region="analytical-center"]'),
  ).not.toBeNull();
  expect(
    document.querySelector('[data-shell-region="ranked-matrix"]'),
  ).not.toBeNull();

  const screenContext = document.querySelector(
    '[data-shell-region="screen-context"]',
  );
  expect(screenContext).not.toBeNull();
  expect((screenContext as HTMLElement).style.height).toBe("72px");
  expect(
    within(screenContext as HTMLElement).getByRole("searchbox"),
  ).toBeDefined();
  expect(screen.getAllByRole("searchbox")).toHaveLength(1);
});

test("keeps a gated view reason inside the fixed screen context", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=statements&at=1722400000000000`,
  );
  renderApp();
  const context = await waitFor(() => {
    const element = document.querySelector(
      '[data-shell-region="screen-context"]',
    );
    expect(element).not.toBeNull();
    return element as HTMLElement;
  });

  expect(within(context).getByRole("status")).toBeDefined();
  expect(within(context).queryByRole("searchbox")).toBeNull();
});

test("mobile keeps incident triage in normal flow without permanent navigation", async () => {
  vi.stubGlobal(
    "matchMedia",
    vi.fn().mockReturnValue({
      matches: true,
      media: "(max-width: 760px)",
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }),
  );
  renderApp();

  expect(await screen.findByTestId("mobile-triage")).toBeDefined();
  expect(screen.getByTestId("app-shell").dataset.shellLayout).toBe("mobile");
  expect(screen.getByRole("main").style.overflow).toBe("visible");
  expect(screen.queryByRole("navigation")).toBeNull();
});

test("mobile statements keeps the heatmap and ranked table available", async () => {
  history.replaceState(null, "", `${location.pathname}#view=statements`);
  vi.stubGlobal(
    "matchMedia",
    vi.fn().mockReturnValue({
      matches: true,
      media: "(max-width: 760px)",
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }),
  );
  const availableCatalog = structuredClone(catalogBody);
  const statements = availableCatalog.views.find(
    (view) => view.code === "statements",
  );
  if (statements !== undefined) {
    statements.availability = "available";
    statements.presets = [
      {
        code: "time",
        columns: ["total"],
        sort: { column: "total", order: "desc" },
      },
    ];
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  const workspace = await screen.findByTestId("mobile-statements-workspace");
  expect(
    within(workspace).getByRole("table", { name: "statements" }),
  ).toBeDefined();
  expect(within(workspace).getByText("heatmap.metric")).toBeDefined();
  expect(screen.getByTestId("app-shell").dataset.shellLayout).toBe("mobile");
});

test("digit key follows visible available destination order, not catalog order", async () => {
  renderApp();
  await waitFor(() =>
    expect(
      screen
        .getByRole("tab", { name: /tabs\.plans/i })
        .getAttribute("aria-disabled"),
    ).toBe("false"),
  );
  fireEvent.keyDown(window, { key: "2" });
  expect(location.hash).toContain("view=plans");
});

test("primary navigation is the sole shell owner of Live and prepared spans", async () => {
  renderApp();
  const navigation = await screen.findByTestId("primary-navigation");

  expect(screen.getAllByRole("button", { name: /spine\.live/i })).toHaveLength(
    1,
  );
  fireEvent.click(
    within(navigation).getByRole("button", { name: /spine\.live/i }),
  );
  expect(location.hash).toContain("at=");
  fireEvent.click(
    within(navigation).getByRole("button", { name: /spine\.span\.86400/i }),
  );
  expect(new URLSearchParams(location.hash.slice(1)).get("span")).toBe("86400");
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

test("slash opens global forensic search and keeps its text out of share state", async () => {
  renderApp();
  await waitFor(() =>
    expect(screen.getAllByText("tabs.activity").length).toBeGreaterThan(0),
  );
  fireEvent.keyDown(document.body, { key: "/" });
  const dialog = await screen.findByRole("dialog", {
    name: /Forensic search|search\.title/i,
  });
  const search = within(dialog).getByRole("searchbox");
  expect(document.activeElement).toBe(search);
  fireEvent.change(search, { target: { value: "pid:18422" } });
  expect(location.hash).not.toContain("pid");
  expect(location.hash).not.toContain("q=");
  fireEvent.keyDown(search, { key: "Escape" });
  await waitFor(() =>
    expect(
      screen.queryByRole("dialog", {
        name: /Forensic search|search\.title/i,
      }),
    ).toBeNull(),
  );
});

test("a Locks hash renders its contextual deep-link surface without selecting OS", async () => {
  renderApp();
  await waitFor(() =>
    expect(
      screen.getAllByText("navigation.destination.os").length,
    ).toBeGreaterThan(0),
  );
  act(() => {
    location.hash = "#view=locks";
    window.dispatchEvent(new Event("hashchange"));
  });
  await waitFor(() =>
    expect(screen.getByTestId("contextual-deep-link").textContent).toContain(
      "navigation.deepLink.locks",
    ),
  );
  expect(
    screen
      .getAllByRole("tab")
      .every((tab) => tab.getAttribute("aria-selected") === "false"),
  ).toBe(true);
});

test("a Processes hash explicitly selects OS while retaining deep-link context", async () => {
  history.replaceState(null, "", `${location.pathname}#view=processes`);
  renderApp();

  const os = await screen.findByRole("tab", {
    name: /navigation\.destination\.os/i,
  });
  expect(os.getAttribute("aria-selected")).toBe("true");
  expect(screen.getByTestId("contextual-deep-link").textContent).toContain(
    "navigation.deepLink.processes",
  );
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
  // The slider owns the shared span/48 step; the global handler must not add
  // a second step on top — exactly one 75 s patch.
  expect(BigInt(stepped ?? "0") - 1_722_400_000_000_000n).toBe(75_000_000n);
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

test("an arbitrary replay span reaches heatmap consumers exactly", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=1722400000000000&span=37`,
  );
  renderApp();

  await waitFor(() => {
    const heatmapCall = vi
      .mocked(globalThis.fetch)
      .mock.calls.map(([input]) =>
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : input.href,
      )
      .find((url) => new URL(url).pathname === "/v1/timeline/heatmap");
    expect(heatmapCall).toBeDefined();
    const params = new URL(heatmapCall ?? location.href).searchParams;
    expect(params.get("from")).toBe("1722399963000000");
    expect(params.get("to")).toBe("1722400000000000");

    const healthCall = vi
      .mocked(globalThis.fetch)
      .mock.calls.map(([input]) =>
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : input.href,
      )
      .find((url) => new URL(url).pathname === "/v1/timeline/health");
    expect(healthCall).toBeDefined();
    expect(new URL(healthCall ?? location.href).searchParams.get("step")).toBe(
      "385417",
    );
  });
});

test("real timeline and data-health endpoints share the pinned LIVE geometry", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-08-03T12:00:00.123Z"));
  renderApp();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(50);
  });
  fireEvent.click(screen.getByTestId("data-health-chip"));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(50);
  });

  const expectedTo = "1785758400123000";
  const expectedFrom = "1785754800123000";
  const expectedPreviousFrom = "1785751200123000";
  const calls = () =>
    vi
      .mocked(globalThis.fetch)
      .mock.calls.map(
        ([input]) =>
          new URL(
            typeof input === "string"
              ? input
              : input instanceof Request
                ? input.url
                : input.href,
          ),
      );
  const latest = (path: string) =>
    calls()
      .filter((url) => url.pathname === path)
      .at(-1) ?? new URL(location.href);

  for (const path of [
    "/v1/timeline/spine",
    "/v1/timeline/events",
    "/v1/timeline/heatmap",
    "/v1/data/quality",
  ]) {
    expect(latest(path).searchParams.get("from")).toBe(expectedFrom);
    expect(latest(path).searchParams.get("to")).toBe(expectedTo);
  }
  expect(latest("/v1/timeline/health").searchParams.get("from")).toBe(
    expectedPreviousFrom,
  );
  expect(latest("/v1/timeline/health").searchParams.get("to")).toBe(expectedTo);

  const qualityCalls = calls().filter(
    (url) => url.pathname === "/v1/data/quality",
  ).length;
  await act(async () => {
    await vi.advanceTimersByTimeAsync(2_000);
  });
  expect(
    calls().filter((url) => url.pathname === "/v1/data/quality").length,
  ).toBe(qualityCalls);
  expect(latest("/v1/data/quality").searchParams.get("to")).toBe(expectedTo);
  vi.useRealTimers();
});

test("switching view drops preset and sort the next view does not have", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=statements&preset=stmt_only&sort=total&order=desc`,
  );
  renderApp();
  await waitFor(() =>
    expect(
      screen
        .getByRole("tab", { name: /tabs\.plans/i })
        .getAttribute("aria-disabled"),
    ).toBe("false"),
  );
  expect(location.hash).toContain("preset=stmt_only");
  fireEvent.keyDown(window, { key: "2" });
  const params = new URLSearchParams(location.hash.slice(1));
  expect(params.get("view")).toBe("plans");
  expect(params.get("preset")).toBeNull();
  expect(params.get("sort")).toBeNull();
  expect(params.get("order")).toBeNull();
});

test("switching view keeps a preset both views share", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=statements&preset=memory`,
  );
  renderApp();
  await waitFor(() =>
    expect(
      screen
        .getByRole("tab", { name: /tabs\.tables/i })
        .getAttribute("aria-disabled"),
    ).toBe("false"),
  );
  fireEvent.keyDown(window, { key: "3" });
  expect(new URLSearchParams(location.hash.slice(1)).get("view")).toBe(
    "tables",
  );
  expect(new URLSearchParams(location.hash.slice(1)).get("preset")).toBe(
    "memory",
  );
});

test("switching a statement lens drops the prior explicit sort", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=statements&preset=time&sort=rows&order=desc`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const statements = availableCatalog.views.find(
    (view) => view.code === "statements",
  );
  if (statements !== undefined) {
    statements.availability = "available";
    statements.presets = [
      {
        code: "time",
        columns: ["total", "rows"],
        sort: { column: "total", order: "desc" },
      },
      {
        code: "io",
        columns: ["blks_read"],
        sort: { column: "blks_read", order: "desc" },
      },
    ];
    statements.columns = [
      {
        availability: "available",
        code: "total",
        lazy: false,
        requires: [],
        type: "f64",
      },
      {
        availability: "available",
        code: "rows",
        lazy: false,
        requires: [],
        type: "u64",
      },
      {
        availability: "available",
        code: "blks_read",
        lazy: false,
        requires: [],
        type: "u64",
      },
    ];
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  fireEvent.click(await screen.findByRole("button", { name: /buffers/i }));
  const params = new URLSearchParams(location.hash.slice(1));
  expect(params.get("preset")).toBe("io");
  expect(params.get("sort")).toBeNull();
  expect(params.get("order")).toBeNull();
});

test("Activity owns prepared forensic lenses, point evidence and a 96-bucket heatmap", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=1722400000000000`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const activity = availableCatalog.views.find(
    (view) => view.code === "activity",
  );
  if (activity !== undefined) {
    activity.presets = [
      "overview",
      "waits_locks",
      "duration",
      "cpu",
      "disk_io",
      "replication",
      "sampling",
    ].map((code) => ({
      code,
      columns: [],
      sort: { column: "metric", order: "desc" as const },
    }));
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  expect(await screen.findByTestId("activity-point-evidence")).toBeDefined();
  const overview = screen.getByRole("button", { name: /^overview$/i });
  expect(overview.getAttribute("aria-pressed")).toBe("true");
  expect(
    screen
      .getByRole("button", { name: /^memory /i })
      .getAttribute("aria-disabled"),
  ).toBe("true");
  expect(
    screen
      .getByRole("button", { name: /^xidHorizon /i })
      .getAttribute("aria-disabled"),
  ).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: /^cpu$/i }));
  expect(new URLSearchParams(location.hash.slice(1)).get("preset")).toBe("cpu");
  expect(screen.getByTestId("activity-process-evidence")).toBeDefined();

  await waitFor(() => {
    const heatmap = fetchImpl.mock.calls
      .map(
        ([input]) =>
          new URL(
            typeof input === "string"
              ? input
              : input instanceof Request
                ? input.url
                : input.href,
          ),
      )
      .find(
        (url) =>
          url.pathname === "/v1/timeline/heatmap" &&
          url.searchParams.get("view") === "activity",
      );
    expect(heatmap?.searchParams.get("buckets")).toBe("96");
  });
});

test("Activity resource lenses inherit partial-source availability from the catalog", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=1722400000000000`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const activity = availableCatalog.views.find(
    (view) => view.code === "activity",
  );
  if (activity !== undefined) {
    activity.columns = [
      {
        availability: "gated",
        unavailable_reason: "not_collected",
        code: "cpu",
        lazy: false,
        requires: ["process", "instance"],
        type: "f64",
      },
      {
        availability: "not_collected",
        unavailable_reason: "not_collected",
        code: "read_bytes_per_second",
        lazy: false,
        requires: ["process"],
        type: "f64",
      },
      {
        availability: "gated",
        unavailable_reason: "missing_privilege_or_version",
        code: "replication_state",
        lazy: false,
        requires: ["replication_replicas"],
        type: "text",
      },
    ];
    activity.presets = [
      {
        code: "overview",
        columns: [],
        sort: { column: "cpu", order: "desc" as const },
      },
      {
        code: "cpu",
        columns: ["cpu"],
        sort: { column: "cpu", order: "desc" as const },
      },
      {
        code: "disk_io",
        columns: ["read_bytes_per_second"],
        sort: { column: "read_bytes_per_second", order: "desc" as const },
      },
      {
        code: "replication",
        columns: ["replication_state"],
        sort: { column: "replication_state", order: "desc" as const },
      },
    ];
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  await screen.findByTestId("activity-point-evidence");
  for (const name of [/^cpu /i, /^diskIo /i, /^replication /i]) {
    expect(
      screen.getByRole("button", { name }).getAttribute("aria-disabled"),
    ).toBe("true");
  }
  fireEvent.click(screen.getByRole("button", { name: /^cpu /i }));
  expect(new URLSearchParams(location.hash.slice(1)).get("preset")).toBeNull();
});

test("Plans owns change lanes, fork provenance and keeps Compare explicitly gated", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=plans&at=1722400000000000`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const plans = availableCatalog.views.find((view) => view.code === "plans");
  if (plans !== undefined) {
    plans.presets = ["time", "io", "rows", "change_timeline"].map((code) => ({
      code,
      columns: [],
      sort: { column: "metric", order: "desc" as const },
    }));
    plans.joins = [
      {
        left: "plans",
        right: "statements",
        kind: "best_effort",
        fields: ["queryid", "dbid", "userid"],
        cardinality: "many_to_one",
        provenance: "ossc_queryid_dbid_userid_attribution",
      },
      {
        left: "plans",
        right: "statements",
        kind: "best_effort",
        fields: ["queryid_stat_statements", "dbid", "userid"],
        cardinality: "many_to_one",
        provenance: "vadv_queryid_stat_statements_dbid_userid_attribution",
      },
    ];
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  expect(
    await screen.findByText("ossc_queryid_dbid_userid_attribution"),
  ).toBeDefined();
  expect(
    screen
      .getByRole("button", { name: /^planCompare /i })
      .getAttribute("aria-disabled"),
  ).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: /^planChanges$/i }));
  expect(new URLSearchParams(location.hash.slice(1)).get("preset")).toBe(
    "change_timeline",
  );
  await waitFor(() => {
    expect(
      fetchImpl.mock.calls.some(([input]) => {
        const url = new URL(
          typeof input === "string"
            ? input
            : input instanceof Request
              ? input.url
              : input.href,
        );
        return (
          url.pathname === "/v1/frame/plans" &&
          url.searchParams.get("preset") === "change_timeline" &&
          url.searchParams.get("limit") === "3"
        );
      }),
    ).toBe(true);
  });
});

test("OS keeps host pressure separate from process rows and uses a dense range heatmap", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=processes&at=1722400000000000`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const processes = availableCatalog.views.find(
    (view) => view.code === "processes",
  );
  if (processes !== undefined) {
    processes.presets = [
      "pressure",
      "cpu",
      "memory",
      "disk_io",
      "cgroup",
      "processes",
      "data_quality",
    ].map((code) => ({
      code,
      columns: [],
      sort: { column: "metric", order: "desc" as const },
    }));
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  const evidence = await screen.findByTestId("host-pressure-evidence");
  expect(evidence.closest("aside")?.dataset.view).toBe("processes");
  expect(
    screen
      .getByRole("button", { name: /^osPressure$/i })
      .getAttribute("aria-pressed"),
  ).toBe("true");
  for (const name of [/^osNetwork /i, /^osFilesystems /i]) {
    expect(
      screen.getByRole("button", { name }).getAttribute("aria-disabled"),
    ).toBe("true");
  }

  await waitFor(() => {
    const calls = fetchImpl.mock.calls.map(
      ([input]) =>
        new URL(
          typeof input === "string"
            ? input
            : input instanceof Request
              ? input.url
              : input.href,
        ),
    );
    expect(
      calls.some(
        (url) =>
          url.pathname === "/v1/timeline/heatmap" &&
          url.searchParams.get("view") === "processes" &&
          url.searchParams.get("buckets") === "96",
      ),
    ).toBe(true);
    expect(
      calls.some(
        (url) =>
          url.pathname === "/v1/timeline/spine" &&
          url.searchParams.get("buckets") === "24",
      ),
    ).toBe(true);
  });
});

test("Tables and Indexes expose same-snapshot context while unsupported analysis stays gated", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=tables&at=1722400000000000`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const tables = availableCatalog.views.find((view) => view.code === "tables");
  const indexes = availableCatalog.views.find(
    (view) => view.code === "indexes",
  );
  if (tables !== undefined) {
    tables.presets = [
      "health",
      "vacuum_risk",
      "io",
      "scan_pattern",
      "size",
      "xid_mxid",
    ].map((code) => ({
      code,
      columns: [],
      sort: { column: "metric", order: "desc" as const },
    }));
    tables.joins = [
      {
        left: "tables",
        right: "vacuum",
        kind: "temporal",
        fields: ["datid", "relid", "ts"],
        cardinality: "zero_or_many",
        provenance: "same_snapshot_database_relation_oid",
      },
    ];
  }
  if (indexes !== undefined) {
    indexes.presets = ["usage", "io", "size", "unused", "table_context"].map(
      (code) => ({
        code,
        columns: [],
        sort: { column: "metric", order: "desc" as const },
      }),
    );
    indexes.joins = [
      {
        left: "indexes",
        right: "tables",
        kind: "temporal",
        fields: ["datid", "relid", "ts"],
        cardinality: "zero_or_one",
        provenance: "same_snapshot_database_relation_oid",
      },
    ];
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  expect(
    (await screen.findByTestId("infrastructure-evidence-panel")).dataset.view,
  ).toBe("tables");
  expect(
    screen
      .getByRole("button", { name: /^tableDependencies /i })
      .getAttribute("aria-disabled"),
  ).toBe("true");
  expect(
    screen
      .getByRole("button", { name: /^tableSizeGrowth /i })
      .getAttribute("aria-disabled"),
  ).toBe("true");
  fireEvent.click(screen.getByRole("tab", { name: /tabs\.indexes/i }));
  expect(
    (await screen.findByTestId("index-table-provenance")).textContent,
  ).toContain("same_snapshot_database_relation_oid");
  for (const name of [
    /^indexSizeGrowth /i,
    /^indexDuplication /i,
    /^indexInvalidBuild /i,
  ]) {
    expect(
      screen.getByRole("button", { name }).getAttribute("aria-disabled"),
    ).toBe("true");
  }
});

test("Vacuum defaults to point progress and discloses unsafe lifetime joins", async () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=vacuum&at=1722400000000000`,
  );
  const availableCatalog = structuredClone(catalogBody);
  const vacuum = availableCatalog.views.find((view) => view.code === "vacuum");
  if (vacuum !== undefined) {
    vacuum.presets = ["progress", "phase", "dead_items"].map((code) => ({
      code,
      columns: [],
      sort: { column: "metric", order: "desc" as const },
    }));
    vacuum.joins = [
      {
        left: "vacuum",
        right: "tables",
        kind: "temporal",
        fields: ["datid", "relid", "ts"],
        cardinality: "zero_or_one",
        provenance: "same_snapshot_database_relation_oid",
      },
    ];
  }
  const baseFetch = stubFetch();
  const fetchImpl = vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    return new URL(url).pathname === "/v1/ui/catalog"
      ? Promise.resolve(jsonResponse(availableCatalog))
      : baseFetch(input);
  });
  renderApp(fetchImpl);

  expect(await screen.findByTestId("vacuum-lifetime-warning")).toBeDefined();
  expect(
    screen
      .getByRole("button", { name: /^vacuumProgress$/i })
      .getAttribute("aria-pressed"),
  ).toBe("true");
  for (const name of [
    /^vacuumWraparound /i,
    /^vacuumThroughput /i,
    /^vacuumBlockers /i,
    /^vacuumHistory /i,
  ]) {
    expect(
      screen.getByRole("button", { name }).getAttribute("aria-disabled"),
    ).toBe("true");
  }
});
