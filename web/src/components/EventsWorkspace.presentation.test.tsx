import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import {
  makeEventFact,
  makeEventsResponse,
  makeViewSpec,
} from "../testkit/apiFixtures";
import { EventsWorkspace } from "./EventsWorkspace";

vi.mock("./HeatmapStrip", () => ({
  HeatmapStrip: () => <div data-testid="heatmap-placeholder" />,
}));

vi.mock("./EventsSignalPanel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./EventsSignalPanel")>();
  return {
    ...actual,
    EventsSignalPanel: () => <div data-testid="signals-placeholder" />,
  };
});

afterEach(() => vi.unstubAllGlobals());

function wrapper({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      {children}
    </QueryClientProvider>
  );
}

test("Events rows prioritize occurrences and hide collection quality", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify(
          makeEventsResponse({
            completeness: "partial",
            retained_exactness: "lower_bound",
            events: [
              makeEventFact({
                event_instance_id: "deadlock-3",
                event_kind: "pg.database.deadlock_delta",
                occurrence_count: 3,
                entity: { kind: "database", id: "opaque-database-id" },
                identity_quality: "content_derived",
                evidence_quality: "derived_exact",
              }),
            ],
          }),
        ),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );

  render(
    <EventsWorkspace
      view={makeViewSpec({ code: "events" })}
      from="1722396400000000"
      to="1722400000000000"
      metric="count"
      preset={null}
      q={null}
      selectedRange={{
        fromUs: "1722396400000000",
        toUs: "1722400000000000",
      }}
      cursorUs="1722400000000000"
      hoverUs={null}
      brushDraft={null}
      baselineUs={null}
      onMetricChange={() => {}}
      onSelectEntity={() => {}}
      onInvestigate={() => {}}
    />,
    { wrapper },
  );

  const row = await screen.findByTestId("event-range-row");
  expect(row.textContent).toContain("×3");
  expect(row.textContent).toContain("database");
  expect(row.textContent).not.toMatch(
    /content_derived|derived_exact|opaque-database-id|quality/i,
  );
  expect(screen.getByTestId("events-workspace").textContent).not.toMatch(
    /partial|lower_bound|completeness|retention|known loss/i,
  );
  expect(document.querySelector(".event-families__quality")).toBeNull();
  expect(
    screen.getAllByText("eventsWorkspace.occurrences").length,
  ).toBeGreaterThan(1);
});
