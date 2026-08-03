import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { makeEventFact, makeEventsResponse } from "../testkit/apiFixtures";
import { EventsSignalPanel } from "./EventsSignalPanel";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function wrapper({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false } },
        })
      }
    >
      {children}
    </QueryClientProvider>
  );
}

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

test("Signals keeps newest lanes human and free of collection quality codes", async () => {
  const events = Array.from({ length: 8 }, (_, index) =>
    makeEventFact({
      event_instance_id: `event-${index}`,
      event_kind: "pg.checkpoint.completed",
      sort_ts_us: 1_722_400_000_000_000 + index,
      occurred_at_us: 1_722_400_000_000_000 + index,
      occurrence_count: index + 1,
      evidence_quality: index === 7 ? "derived_exact" : "exact",
      identity_quality: "content_derived",
    }),
  );
  const fetchMock = vi
    .fn()
    .mockResolvedValue(response(makeEventsResponse({ events })));
  vi.stubGlobal("fetch", fetchMock);
  render(
    <EventsSignalPanel
      from="1722396400000000"
      to="1722400000000000"
      preset="timeline"
      onInvestigate={() => {}}
    />,
    { wrapper },
  );

  const lanes = await screen.findAllByTestId("event-signal-lane");
  expect(lanes).toHaveLength(5);
  expect(lanes[0]?.getAttribute("data-event-instance")).toBe("event-7");
  expect(screen.getByTestId("event-signals-summary").textContent).toContain(
    "eventsSignals.summary",
  );
  expect(lanes[0]?.textContent).toContain("×8");
  expect(lanes[0]?.textContent).not.toMatch(
    /derived_exact|content_derived|quality/i,
  );
  expect(lanes[0]?.getAttribute("aria-label")).not.toMatch(
    /derived_exact|content_derived|quality/i,
  );
  const url = new URL((fetchMock.mock.calls[0]?.[0] as Request).url);
  expect(url.pathname).toBe("/v1/timeline/events");
  expect(url.searchParams.get("from")).toBe("1722396400000000");
  expect(url.searchParams.get("to")).toBe("1722400000000000");
  expect(url.searchParams.get("limit")).toBe("50");
});

test("a family lens filters Signals and routes opaque entity identity only to an investigation screen", async () => {
  const checkpoint = makeEventFact({
    event_instance_id: "checkpoint-1",
    event_kind: "pg.checkpoint.completed",
    sort_ts_us: 1_722_400_000_000_123,
    occurred_at_us: 1_722_400_000_000_123,
    entity: { kind: "database", id: "opaque-content-id" },
    identity_quality: "content_derived",
    evidence_quality: "derived_exact",
  });
  const error = makeEventFact({
    event_instance_id: "error-1",
    event_kind: "pg.log.error_group_observed",
    sort_ts_us: 1_722_400_000_000_456,
  });
  vi.stubGlobal(
    "fetch",
    vi
      .fn()
      .mockResolvedValue(
        response(makeEventsResponse({ events: [error, checkpoint] })),
      ),
  );
  const onInvestigate = vi.fn();
  render(
    <EventsSignalPanel
      from="1722396400000000"
      to="1722400000000000"
      preset="checkpoints"
      onInvestigate={onInvestigate}
    />,
    { wrapper },
  );

  const lane = await screen.findByTestId("event-signal-lane");
  expect(lane.getAttribute("data-event-instance")).toBe("checkpoint-1");
  expect(lane.getAttribute("title")).toBe("eventsSignals.tooltip");
  expect(screen.queryByText(/error-1/i)).toBeNull();
  fireEvent.click(lane);
  expect(onInvestigate).toHaveBeenCalledWith(
    "tables",
    "1722400000000123",
    "checkpoint-1",
  );
  expect(onInvestigate.mock.calls[0]).not.toContain("opaque-content-id");
});

test("Signals keeps retained facts visible without collection diagnostics", async () => {
  const fetchMock = vi.fn().mockResolvedValue(
    response(
      makeEventsResponse({
        completeness: "partial",
        retained_exactness: "lower_bound",
        events: [
          makeEventFact({
            event_instance_id: "lost-1",
            event_kind: "collector.pg_log_gap",
            loss: {
              lost_count_lower_bound: 4,
              reasons: ["producer_gap"],
            },
          }),
        ],
      }),
    ),
  );
  vi.stubGlobal("fetch", fetchMock);
  const { rerender } = render(
    <EventsSignalPanel
      from="1722396400000000"
      to="1722400000000000"
      preset="collector_health"
      onInvestigate={() => {}}
    />,
    { wrapper },
  );

  await waitFor(() =>
    expect(screen.getByTestId("event-signals-summary").textContent).toContain(
      "eventsSignals.summary",
    ),
  );
  expect(screen.getByTestId("events-signal-panel").textContent).not.toMatch(
    /partial|lower_bound|producer_gap/i,
  );

  fetchMock.mockResolvedValueOnce(response(makeEventsResponse({ events: [] })));
  rerender(
    <EventsSignalPanel
      from="1722396400000001"
      to="1722400000000001"
      preset="collector_health"
      onInvestigate={() => {}}
    />,
  );
  await waitFor(() =>
    expect(screen.queryAllByTestId("event-signal-lane")).toHaveLength(0),
  );
  expect(screen.getByText("eventsSignals.empty")).toBeDefined();
});

test("Signals offers a bounded retry after an event request fails", async () => {
  const fetchMock = vi
    .fn()
    .mockRejectedValueOnce(new Error("timeline unavailable"))
    .mockResolvedValueOnce(
      response(
        makeEventsResponse({
          events: [
            makeEventFact({
              event_instance_id: "recovered-event",
              event_kind: "pg.checkpoint.completed",
            }),
          ],
        }),
      ),
    );
  vi.stubGlobal("fetch", fetchMock);
  render(
    <EventsSignalPanel
      from="1722396400000000"
      to="1722400000000000"
      preset="timeline"
      onInvestigate={() => {}}
    />,
    { wrapper },
  );

  fireEvent.click(
    await screen.findByRole("button", { name: "eventsSignals.retry" }),
  );

  expect(
    (await screen.findByTestId("event-signal-lane")).getAttribute(
      "data-event-instance",
    ),
  ).toBe("recovered-event");
  expect(fetchMock).toHaveBeenCalledTimes(2);
  const retryUrl = new URL((fetchMock.mock.calls[1]?.[0] as Request).url);
  expect(retryUrl.searchParams.get("limit")).toBe("50");
});
