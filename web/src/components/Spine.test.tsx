import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeEventFact,
  makeEventsResponse,
  makeHealthPoint,
  makeHealthResponse,
} from "../testkit/apiFixtures";
import { Spine, type SpineProps } from "./Spine";

afterEach(() => vi.unstubAllGlobals());

/** Fixed cursor; the spine always shows the trailing 24 h before it. */
const AT_US = 1_722_500_000_000_000;
const WINDOW_US = 86_400_000_000;
const FROM_US = AT_US - WINDOW_US;

const healthFixture = makeHealthResponse({
  points: [
    makeHealthPoint({
      interval: { from_us: FROM_US, to_us: FROM_US + 3_600_000_000 },
      overall_score: 1,
    }),
    makeHealthPoint({
      interval: {
        from_us: FROM_US + 3_600_000_000,
        to_us: FROM_US + 7_200_000_000,
      },
      overall_score: 0.5,
    }),
    makeHealthPoint({
      interval: {
        from_us: FROM_US + 7_200_000_000,
        to_us: FROM_US + 10_800_000_000,
      },
      overall_score: null,
    }),
  ],
});

const eventsFixture = makeEventsResponse({
  events: [
    makeEventFact({
      event_kind: "pg_checkpoint_completed",
      notable_class: "info",
      occurred_at_us: FROM_US + WINDOW_US / 2,
      sort_ts_us: FROM_US + WINDOW_US / 2,
    }),
    makeEventFact({
      event_instance_id: "instance-2",
      event_kind: "pg_log_error_group_observed",
      notable_class: "panic",
      occurred_at_us: FROM_US + WINDOW_US / 4,
      sort_ts_us: FROM_US + WINDOW_US / 4,
    }),
  ],
});

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
    const body = url.includes("/v1/timeline/events")
      ? eventsFixture
      : healthFixture;
    return Promise.resolve(jsonResponse(body));
  });
}

function stubRect(svg: Element) {
  svg.getBoundingClientRect = () =>
    ({
      left: 0,
      top: 0,
      right: 1000,
      bottom: 60,
      width: 1000,
      height: 60,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }) as DOMRect;
}

function renderSpine(overrides: Partial<SpineProps> = {}) {
  vi.stubGlobal("fetch", stubFetch());
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const props: SpineProps = {
    at: String(AT_US),
    span: 3600,
    baseline: null,
    onSelectAt: () => {},
    onSelectSpan: () => {},
    onSelectBaseline: () => {},
    ...overrides,
  };
  return render(<Spine {...props} />, { wrapper });
}

test("renders gutter, health polyline, cursor and event markers", async () => {
  const { container } = renderSpine();
  await waitFor(() =>
    expect(screen.getByTestId("spine-health-line")).toBeDefined(),
  );
  expect(screen.getByTestId("spine-gutter").style.width).toBe("158px");
  expect(screen.getByTestId("spine-cursor")).toBeDefined();
  expect(container.querySelectorAll("[data-tick]")).toHaveLength(25);
  // Checkpoint marker glyph; null-score point is dropped from the polyline.
  expect(screen.getByText("▲")).toBeDefined();
  expect(screen.getByText("●")).toBeDefined();
  const points = screen.getByTestId("spine-health-line").getAttribute("points");
  expect(points?.split(" ")).toHaveLength(2);
  // Untranslated i18n keys surface verbatim: REPLAY mode for a fixed cursor.
  expect(screen.getByRole("button", { name: /replay/ })).toBeDefined();
});

test("click on the strip reports the cursor µs at that position", async () => {
  const onSelectAt = vi.fn();
  renderSpine({ onSelectAt });
  await waitFor(() =>
    expect(screen.getByTestId("spine-health-line")).toBeDefined(),
  );
  const svg = screen.getByRole("slider");
  stubRect(svg);
  fireEvent.click(svg, { clientX: 500 });
  expect(onSelectAt).toHaveBeenCalledWith(String(FROM_US + WINDOW_US / 2));
});

test("shift+click sets the baseline, a repeat nearby clears it", async () => {
  const onSelectBaseline = vi.fn();
  renderSpine({
    onSelectBaseline,
    baseline: String(FROM_US + WINDOW_US / 2),
  });
  await waitFor(() =>
    expect(screen.getByTestId("spine-baseline")).toBeDefined(),
  );
  const svg = screen.getByRole("slider");
  stubRect(svg);
  fireEvent.click(svg, { clientX: 100, shiftKey: true });
  expect(onSelectBaseline).toHaveBeenCalledWith(
    String(FROM_US + WINDOW_US / 10),
  );
  // 500px == the current baseline position; repeat shift-click clears it.
  fireEvent.click(svg, { clientX: 500, shiftKey: true });
  expect(onSelectBaseline).toHaveBeenLastCalledWith(null);
});

test("mode button toggles LIVE → REPLAY and back", async () => {
  const onSelectAt = vi.fn();
  const { unmount } = renderSpine({ at: null, onSelectAt });
  const liveButton = await screen.findByRole("button", { name: /live/ });
  fireEvent.click(liveButton);
  expect(onSelectAt).toHaveBeenCalledWith(expect.stringMatching(/^\d+$/));
  unmount();

  renderSpine({ at: String(AT_US), onSelectAt });
  const replayButton = await screen.findByRole("button", { name: /replay/ });
  fireEvent.click(replayButton);
  expect(onSelectAt).toHaveBeenLastCalledWith(null);
});

test("zoom group reports the selected span", async () => {
  const onSelectSpan = vi.fn();
  renderSpine({ onSelectSpan });
  await waitFor(() =>
    expect(screen.getByTestId("spine-health-line")).toBeDefined(),
  );
  fireEvent.click(screen.getByRole("button", { name: /86400/ }));
  expect(onSelectSpan).toHaveBeenCalledWith(86400);
});
