import { act, cleanup, render } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  TimeGeometryProvider,
  useTimeGeometry,
  type TimeGeometryValue,
} from "./timeGeometry";

let observed: TimeGeometryValue | null = null;

function Probe() {
  observed = useTimeGeometry();
  return null;
}

function value(): TimeGeometryValue {
  if (observed === null) throw new Error("time geometry was not observed");
  return observed;
}

function renderProvider(children: ReactNode = <Probe />) {
  return render(<TimeGeometryProvider>{children}</TimeGeometryProvider>);
}

beforeEach(() => {
  observed = null;
  history.replaceState(null, "", location.pathname);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

test("derives exact BigInt endpoints for replay and arbitrary spans", () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=1722400000000001&span=37`,
  );
  renderProvider();

  expect(value().range).toEqual({
    fromUs: "1722399963000001",
    toUs: "1722400000000001",
  });
  expect(value().isLive).toBe(false);
  expect(value().preparedSpans).toEqual([900, 3600, 21600, 86400]);
});

test("pins one LIVE tick for all consumers across unrelated renders", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-08-03T12:00:00.123Z"));
  const seen: Record<string, string> = {};

  function Consumer({ name }: { name: string }) {
    const geometry = useTimeGeometry();
    seen[name] = geometry.range.toUs;
    return null;
  }

  function Unrelated() {
    const [count, setCount] = useState(0);
    return <button onClick={() => setCount((n) => n + 1)}>{count}</button>;
  }

  const rendered = renderProvider(
    <>
      <Probe />
      <Consumer name="health" />
      <Consumer name="heatmap" />
      <Unrelated />
    </>,
  );

  expect(seen).toEqual({
    health: "1785758400123000",
    heatmap: "1785758400123000",
  });
  act(() => rendered.getByRole("button").click());
  expect(seen).toEqual({
    health: "1785758400123000",
    heatmap: "1785758400123000",
  });

  act(() => {
    vi.advanceTimersByTime(15_000);
  });
  expect(seen).toEqual({
    health: "1785758415123000",
    heatmap: "1785758415123000",
  });
});

test("commits a brush as one replay range and preserves the baseline", () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&baseline=1722390000000000`,
  );
  renderProvider();

  act(() => {
    value().commitRange({
      fromUs: "1722399940000000",
      toUs: "1722400000000000",
    });
  });

  expect(value().state.at).toBe("1722400000000000");
  expect(value().state.span).toBe(60);
  expect(value().state.baseline).toBe("1722390000000000");
  expect(value().range).toEqual({
    fromUs: "1722399940000000",
    toUs: "1722400000000000",
  });
  expect(location.hash).toContain("at=1722400000000000");
  expect(location.hash).toContain("span=60");
  expect(location.hash).toContain("baseline=1722390000000000");
});

test("normalizes a sub-second brush to one second", () => {
  renderProvider();

  act(() => {
    value().commitRange({ fromUs: "2000000", toUs: "2500000" });
  });

  expect(value().state.at).toBe("2500000");
  expect(value().state.span).toBe(1);
  expect(value().range).toEqual({ fromUs: "1500000", toUs: "2500000" });
});

test("rejects invalid cursor, span, and brushes over 24 hours", () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}#view=activity&at=100000000&span=10`,
  );
  renderProvider();
  const before = value().state;

  act(() => {
    value().setCursor("not-decimal");
    value().setSpan(0);
    value().setSpan(-1);
    value().setSpan(86_401);
    value().commitRange({ fromUs: "0", toUs: "86400000001" });
  });

  expect(value().state).toEqual(before);
});

test("keeps hover and brush drafts ephemeral", () => {
  renderProvider();
  const hashBefore = location.hash;

  act(() => {
    value().setHover("1722400000000000");
    value().setBrushDraft({
      fromUs: "1722399990000000",
      toUs: "1722400000000000",
    });
  });

  expect(value().hoverUs).toBe("1722400000000000");
  expect(value().brushDraft).toEqual({
    fromUs: "1722399990000000",
    toUs: "1722400000000000",
  });
  expect(location.hash).toBe(hashBefore);
});

test("adopts hash changes from back and forward navigation", () => {
  renderProvider();

  act(() => {
    history.replaceState(
      null,
      "",
      `${location.pathname}#view=statements&at=1722400000000000&span=47`,
    );
    window.dispatchEvent(new Event("hashchange"));
  });

  expect(value().state.view).toBe("statements");
  expect(value().range).toEqual({
    fromUs: "1722399953000000",
    toUs: "1722400000000000",
  });
});

test("toggleLive pins replay and then returns to the shared LIVE clock", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-08-03T12:00:00.000Z"));
  renderProvider();

  act(() => value().toggleLive());
  expect(value().state.at).toBe("1785758400000000");
  expect(value().isLive).toBe(false);

  vi.setSystemTime(new Date("2026-08-03T12:01:00.000Z"));
  act(() => value().toggleLive());
  expect(value().state.at).toBeNull();
  expect(value().range.toUs).toBe("1785758460000000");
});
