import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { act, createElement, type ReactNode } from "react";
import { afterEach, expect, test, vi } from "vitest";
import type { UiState } from "../state/url";
import {
  makeContextResponse,
  makeDataQualityResponse,
  makeIncident,
  makeIncidentFinding,
  makeIncidentsResponse,
  makeStorageResponse,
} from "../testkit/apiFixtures";
import { Header, type HeaderProps } from "./Header";

const state: UiState = {
  view: "activity",
  at: null,
  span: 3600,
  baseline: null,
  preset: null,
  q: null,
  sort: null,
  order: null,
  focus: null,
  dock: null,
  entity: null,
};

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
    const body = url.includes("/v1/storage")
      ? makeStorageResponse()
      : makeDataQualityResponse();
    return Promise.resolve(jsonResponse(body));
  });
}

function renderHeader(overrides: Partial<HeaderProps> = {}) {
  vi.stubGlobal("fetch", stubFetch());
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  const props: HeaderProps = {
    state,
    context: undefined,
    incidents: undefined,
    shareUrl: "https://pgkronika.local/#view=activity&at=1722400000000000",
    dataHealthOpen: false,
    onToggleDataHealth: () => {},
    onOpenIncidents: () => {},
    ...overrides,
  };
  return render(<Header {...props} />, { wrapper });
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test("instance chip falls back to local, shows the context hostname", () => {
  const { unmount } = renderHeader();
  expect(screen.getByTestId("instance-chip").textContent).toContain("local");
  unmount();
  renderHeader({
    context: makeContextResponse({ instance: { hostname: "prod-1" } }),
  });
  expect(screen.getByTestId("instance-chip").textContent).toContain("prod-1");
});

test("role chip shows an honest dash with the reason in the title", () => {
  const { unmount } = renderHeader();
  expect(screen.getByTestId("role-chip").textContent).toBe("—");
  unmount();
  renderHeader({
    context: makeContextResponse({
      instance: { role: null, role_reason: "pg_control unavailable" },
    }),
  });
  const chip = screen.getByTestId("role-chip");
  expect(chip.textContent).toBe("—");
  expect(chip.getAttribute("title")).toBe("pg_control unavailable");
});

test("role chip shows the role and replica lag when replication is present", () => {
  renderHeader({
    context: makeContextResponse({
      instance: { role: "primary" },
      replication: {
        instance: {
          streaming_replicas: 2,
          replay_lag_us: 3_000_000,
          replay_lag_reason: null,
          timeline_id: 1,
        },
        replicas: [],
      },
    }),
  });
  const chip = screen.getByTestId("role-chip");
  expect(chip.textContent).toContain("primary");
  expect(chip.textContent).toContain("header.replicas");
  expect(chip.textContent).toContain("header.lag 3s");
});

test("role chip shows an honest dash for a null replay lag", () => {
  renderHeader({
    context: makeContextResponse({
      replication: {
        instance: {
          streaming_replicas: 1,
          replay_lag_us: null,
          replay_lag_reason: "no replay in progress",
          timeline_id: 1,
        },
        replicas: [],
      },
    }),
  });
  const chip = screen.getByTestId("role-chip");
  expect(chip.textContent).toContain("header.lag —");
  expect(chip.innerHTML).toContain("no replay in progress");
});

test("data chip shows unknown state without context, ok when complete", () => {
  const { unmount } = renderHeader();
  expect(screen.getByTestId("data-health-chip").textContent).toContain(
    "header.dataUnknown",
  );
  unmount();
  renderHeader({ context: makeContextResponse() });
  expect(screen.getByTestId("data-health-chip").textContent).toContain(
    "header.dataOk",
  );
});

test("data chip shows partial when gaps or gated present, click toggles", () => {
  const onToggleDataHealth = vi.fn();
  renderHeader({
    onToggleDataHealth,
    context: makeContextResponse({
      quality: {
        status: "partial",
        gaps: [{ from_us: "1", to_us: "2" }],
        gated: ["lock_timeout"],
        active_tail: false,
      },
    }),
  });
  const chip = screen.getByTestId("data-health-chip");
  expect(chip.textContent).toContain("header.dataPartial");
  fireEvent.click(chip);
  expect(onToggleDataHealth).toHaveBeenCalledTimes(1);
});

test("opens the data health popover when dataHealthOpen", async () => {
  renderHeader({ dataHealthOpen: true });
  await waitFor(() => expect(screen.getByRole("dialog")).toBeDefined());
});

test("counts incidents by the server level verdict, not confidence", () => {
  const onOpenIncidents = vi.fn();
  renderHeader({
    onOpenIncidents,
    incidents: makeIncidentsResponse({
      incidents: [
        makeIncident({
          incident_key: "i1",
          level: "critical",
          findings: [makeIncidentFinding({ confidence: "low" })],
        }),
        makeIncident({
          incident_key: "i2",
          level: "warning",
          findings: [makeIncidentFinding({ confidence: "high" })],
        }),
        makeIncident({
          incident_key: "i3",
          level: "critical",
          findings: [makeIncidentFinding({ confidence: "medium" })],
        }),
        makeIncident({ incident_key: "i4", level: "info", findings: [] }),
      ],
    }),
  });
  const crit = screen.getByTestId("incidents-critical");
  const warn = screen.getByTestId("incidents-warning");
  expect(crit.textContent).toContain("2");
  expect(warn.textContent).toContain("1");
  fireEvent.click(crit);
  fireEvent.click(warn);
  expect(onOpenIncidents).toHaveBeenCalledTimes(2);
});

test("no incident chips when all incidents are info-level", () => {
  renderHeader({
    incidents: makeIncidentsResponse({
      incidents: [makeIncident({ level: "info" })],
    }),
  });
  expect(screen.queryByTestId("incidents-critical")).toBeNull();
  expect(screen.queryByTestId("incidents-warning")).toBeNull();
});

test("no incident chips when there are no incidents with findings", () => {
  renderHeader({ incidents: makeIncidentsResponse() });
  expect(screen.queryByTestId("incidents-critical")).toBeNull();
  expect(screen.queryByTestId("incidents-warning")).toBeNull();
});

test("clock ticks every second", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-07-31T10:00:00Z"));
  renderHeader();
  const before = screen.getByTestId("clock").textContent;
  act(() => {
    vi.setSystemTime(new Date("2026-07-31T10:00:05Z"));
    vi.advanceTimersByTime(5000);
  });
  const after = screen.getByTestId("clock").textContent;
  expect(before).not.toBe(after);
});

test("copy link writes the canonical share URL and shows a toast for 1.7s", () => {
  vi.useFakeTimers();
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  renderHeader();
  fireEvent.click(screen.getByRole("button", { name: /copyLink/ }));
  expect(writeText).toHaveBeenCalledWith(
    "https://pgkronika.local/#view=activity&at=1722400000000000",
  );
  expect(screen.getByTestId("toast").textContent).toContain("linkCopied");
  act(() => {
    vi.advanceTimersByTime(1700);
  });
  expect(screen.queryByTestId("toast")).toBeNull();
});

test("instance chip tooltip shows host, pg version and cpu from context", async () => {
  renderHeader({
    context: makeContextResponse({
      instance: {
        hostname: "prod-1",
        pg_version_num: 170003,
        pg_system_identifier: "7300000000000000000",
        role: "primary",
      },
      host: { logical_cpu_count: 16 },
    }),
  });
  fireEvent.mouseEnter(screen.getByTestId("instance-chip"));
  await waitFor(() => expect(screen.getByRole("tooltip")).toBeDefined());
  const tip = screen.getByRole("tooltip").textContent ?? "";
  expect(tip).toContain("prod-1");
  expect(tip).toContain("17.3");
  expect(tip).toContain("16");
});
