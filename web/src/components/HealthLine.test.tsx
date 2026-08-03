import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { type ReactNode } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  makeEventsResponse,
  makeHealthPoint,
  makeHealthResponse,
  makeIncidentsResponse,
  makeSpineResponse,
} from "../testkit/apiFixtures";
import { TimeGeometryProvider, useTimeGeometry } from "../state/timeGeometry";
import { HealthLine } from "./HealthLine";

const AT_US = 1_722_500_000_000_000;
const WINDOW_US = 3_600_000_000;
const FROM_US = AT_US - WINDOW_US;

class TestPointerEvent extends MouseEvent {
  readonly pointerId: number;

  constructor(type: string, init: PointerEventInit = {}) {
    super(type, init);
    this.pointerId = init.pointerId ?? 0;
  }
}

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

type FailingSource = "health" | "spine" | "events" | "incidents";

function stubHealthFetch(failing: readonly FailingSource[] = []) {
  const points = Array.from({ length: 96 }, (_, index) =>
    makeHealthPoint({
      interval: {
        from_us: FROM_US + index * (WINDOW_US / 96),
        to_us: FROM_US + (index + 1) * (WINDOW_US / 96),
      },
      overall_state: index === 48 ? "degraded" : "normal",
    }),
  );
  return vi.fn((input: RequestInfo | URL) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : input.href;
    const fails = (source: FailingSource) => failing.includes(source);
    if (url.includes("/v1/timeline/health")) {
      if (fails("health"))
        return Promise.resolve(new Response("health down", { status: 500 }));
      return Promise.resolve(response(makeHealthResponse({ points })));
    }
    if (url.includes("/v1/timeline/events")) {
      if (fails("events"))
        return Promise.resolve(new Response("events down", { status: 500 }));
      return Promise.resolve(response(makeEventsResponse({ events: [] })));
    }
    if (url.includes("/v1/incidents")) {
      if (fails("incidents"))
        return Promise.resolve(new Response("incidents down", { status: 500 }));
      return Promise.resolve(response(makeIncidentsResponse()));
    }
    if (fails("spine"))
      return Promise.resolve(new Response("spine down", { status: 500 }));
    return Promise.resolve(
      response(
        makeSpineResponse({
          grid: {
            from_us: String(FROM_US),
            to_us: String(AT_US),
            bucket_count: 2,
          },
          series: [
            {
              code: "host.load1",
              unit: "loadavg",
              aggregation: "avg",
              values: [0.4, 0.8],
            },
          ],
        }),
      ),
    );
  });
}

function HoverProbe() {
  const { hoverUs } = useTimeGeometry();
  return <output aria-label="external hover">{hoverUs ?? "none"}</output>;
}

function renderHealthLine(failing: readonly FailingSource[] = []) {
  vi.stubGlobal("fetch", stubHealthFetch(failing));
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>
      <TimeGeometryProvider>{children}</TimeGeometryProvider>
    </QueryClientProvider>
  );
  return render(
    <>
      <HealthLine />
      <HoverProbe />
    </>,
    { wrapper },
  );
}

beforeEach(() => {
  vi.stubGlobal("PointerEvent", TestPointerEvent);
  location.hash = `#view=activity&at=${AT_US}`;
});

afterEach(() => {
  vi.unstubAllGlobals();
  location.hash = "";
});

test("public Health line is exactly 60 px and leaves mode and zoom to navigation", async () => {
  renderHealthLine();
  await waitFor(() => expect(screen.getByRole("slider")).toBeDefined());

  const region = screen.getByTestId("health-line");
  expect(getComputedStyle(region).height).toBe("60px");
  expect(getComputedStyle(region).boxSizing).toBe("border-box");
  expect(screen.getAllByRole("slider")).toHaveLength(1);
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.queryByRole("group", { name: /spine\.zoom/i })).toBeNull();
  expect(region.getAttribute("aria-describedby")).toBeTruthy();
  expect(screen.getByTestId("health-line-meaning").textContent).toContain(
    "healthLine.coincidence",
  );
});

test.each(["spine", "events"] as const)(
  "%s failure stays out of normal Health chrome while retained observations remain",
  async (source) => {
    renderHealthLine([source]);
    await waitFor(() =>
      expect(screen.getByTestId("spine-score").textContent).not.toContain("—"),
    );
    expect(screen.queryByTestId("health-line-source-state")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
  },
);

test("a missing Health source is shown locally without technical status", async () => {
  renderHealthLine(["health"]);
  await waitFor(() =>
    expect(screen.getByTestId("health-score-state").textContent).toContain(
      "data.noSnapshotCurrent",
    ),
  );
  expect(screen.getByTestId("spine-score").textContent).toContain("—");
  expect(screen.getByTestId("health-line").textContent).not.toMatch(
    /partial|source|coverage|gap/i,
  );
});

test("incident transport does not suppress observed Health", async () => {
  renderHealthLine(["incidents"]);
  await waitFor(() =>
    expect(screen.getByTestId("spine-score").textContent).not.toContain("—"),
  );
  expect(screen.queryByTestId("health-score-state")).toBeNull();
  expect(screen.queryByRole("button")).toBeNull();
});

test("pointer hover writes the shared time bucket for an external consumer", async () => {
  renderHealthLine();
  const slider = await screen.findByRole("slider");
  slider.getBoundingClientRect = () =>
    ({
      left: 0,
      top: 0,
      right: 1000,
      bottom: 40,
      width: 1000,
      height: 40,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }) as DOMRect;

  fireEvent.pointerMove(slider, { clientX: 250, pointerId: 7 });
  expect(screen.getByLabelText("external hover").textContent).toBe(
    String(FROM_US + WINDOW_US / 4),
  );
  fireEvent.pointerLeave(slider, { pointerId: 7 });
  expect(screen.getByLabelText("external hover").textContent).toBe("none");
});

test("brush draft is visible before one provider commit changes the replay window", async () => {
  renderHealthLine();
  const slider = await screen.findByRole("slider");
  slider.getBoundingClientRect = () =>
    ({
      left: 0,
      top: 0,
      right: 1000,
      bottom: 40,
      width: 1000,
      height: 40,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }) as DOMRect;
  Object.assign(slider, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });

  fireEvent.pointerDown(slider, {
    clientX: 200,
    pointerId: 11,
    button: 0,
  });
  fireEvent.pointerMove(slider, {
    clientX: 650,
    pointerId: 11,
    buttons: 1,
  });
  const draft = screen.getByTestId("health-brush-draft");
  expect(draft.getAttribute("x")).toBe("200");
  expect(draft.getAttribute("width")).toBe("450");
  expect(new URLSearchParams(location.hash.slice(1)).get("span")).toBeNull();

  fireEvent.pointerUp(slider, {
    clientX: 650,
    pointerId: 11,
    button: 0,
  });
  const hash = new URLSearchParams(location.hash.slice(1));
  expect(hash.get("at")).toBe(String(FROM_US + WINDOW_US * 0.65));
  expect(hash.get("span")).toBe("1620");
});
