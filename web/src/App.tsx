import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { useCatalog } from "./api/catalog";
import { useUiContext } from "./api/context";
import { useIncidents } from "./api/incidents";
import { useSummary } from "./api/summary";
import { AlertBar } from "./components/AlertBar";
import { DockOverlay } from "./components/DockOverlay";
import { FocusBar } from "./components/FocusBar";
import { Header } from "./components/Header";
import { HeatmapStrip } from "./components/HeatmapStrip";
import { Spine } from "./components/Spine";
import { StatusBar } from "./components/StatusBar";
import { PageHeader } from "./components/PageHeader";
import { TabBar } from "./components/TabBar";
import { TableView } from "./components/TableView";
import { Toolbar } from "./components/Toolbar";
import { parseHash, toHash, type UiState } from "./state/url";

const queryClient = new QueryClient();

/** Incidents are always queried over the trailing 24 h (µs). */
const INCIDENTS_WINDOW_US = 86_400_000_000n;
/** Shift+Arrow jump length (1 h, µs). */
const HOUR_US = 3_600_000_000n;
/** Arrow keys step through 1/48 of the active span. */
const STEP_DIVISOR = 48n;
const MOBILE_QUERY = "(max-width: 760px)";
/** LIVE cursor tick: the shared `at` advances on this interval, not per
 * render — query keys stay stable between ticks and all panels read the
 * same snapshot time. */
const LIVE_TICK_MS = 15_000;

function useMobile(): boolean {
  return useSyncExternalStore(
    (onChange) => {
      // jsdom has no matchMedia; non-browser targets stay on desktop layout.
      if (typeof window.matchMedia !== "function") return () => {};
      const mql = window.matchMedia(MOBILE_QUERY);
      mql.addEventListener("change", onChange);
      return () => mql.removeEventListener("change", onChange);
    },
    () =>
      typeof window.matchMedia === "function" &&
      window.matchMedia(MOBILE_QUERY).matches,
  );
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.tagName === "SELECT"
  );
}

function Shell() {
  const { t } = useTranslation();
  const [state, setState] = useState(() => parseHash(location.hash));
  const [dataHealthOpen, setDataHealthOpen] = useState(false);
  const [matched, setMatched] = useState<number | null>(null);
  const [metricByView, setMetricByView] = useState<Record<string, string>>({});
  const mobile = useMobile();

  const patch = (p: Partial<UiState>) => {
    setState((prev) => {
      const next = { ...prev, ...p };
      location.hash = toHash(next);
      return next;
    });
  };

  // Back/Forward: adopt the hash — unless it is the one `patch` just wrote.
  useEffect(() => {
    const onHashChange = () => {
      setState((prev) =>
        location.hash === toHash(prev) ? prev : parseHash(location.hash),
      );
    };
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  // int64 µs cursors travel as decimal strings; all math goes through BigInt.
  // LIVE `at` is pinned to a tick: recomputing Date.now() per render would
  // give summary and heatmap different times and storm refetches on any
  // unrelated setState.
  const [liveAt, setLiveAt] = useState(() => String(Date.now() * 1000));
  useEffect(() => {
    const id = setInterval(
      () => setLiveAt(String(Date.now() * 1000)),
      LIVE_TICK_MS,
    );
    return () => clearInterval(id);
  }, []);
  const at = state.at ?? liveAt;
  const incidentsRange = {
    from: (BigInt(at) - INCIDENTS_WINDOW_US).toString(),
    to: at,
  };
  // Heatmap/spine window follows the zoom span (capped at the 24 h API bound).
  const heatmapRange = {
    from: (BigInt(at) - BigInt(state.span) * 1_000_000n).toString(),
    to: at,
  };

  const catalog = useCatalog();
  const summary = useSummary(at);
  const context = useUiContext(at);
  const incidents = useIncidents(incidentsRange);

  const views = catalog.data?.views ?? [];
  const activeView = views.find((v) => v.code === state.view);
  const focusedIncident = state.focus
    ? incidents.data?.incidents.find((i) => i.incident_key === state.focus)
    : undefined;

  // View-scoped URL params: preset and sort only exist within one view's
  // catalog. Switching the view without validating them leaves a 400-ошибку
  // ("ошибка загрузки") in the frame — drop what the next view does not have.
  const selectView = (code: string) => {
    const next = views.find((v) => v.code === code);
    const keepPreset =
      next !== undefined &&
      state.preset !== null &&
      next.presets.some((p) => p.code === state.preset);
    const keepSort =
      next !== undefined &&
      state.sort !== null &&
      next.columns.some((c) => c.code === state.sort);
    patch({
      view: code,
      ...(keepPreset || state.preset === null ? {} : { preset: null }),
      ...(keepSort || state.sort === null ? {} : { sort: null, order: null }),
    });
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // A component that already handled the key (slider, button) owns it.
      if (e.defaultPrevented) return;
      if (isEditableTarget(e.target)) return;
      // Enter/Space on a focused button belong to the button, not to
      // global shortcuts.
      const onButton =
        e.target instanceof HTMLElement && e.target.tagName === "BUTTON";
      if (e.key >= "1" && e.key <= "9") {
        const view = views[Number(e.key) - 1];
        if (view !== undefined && view.availability === "available") {
          selectView(view.code);
        }
        return;
      }
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        if (onButton) return;
        e.preventDefault();
        const step = e.shiftKey
          ? HOUR_US
          : (BigInt(state.span) * 1_000_000n) / STEP_DIVISOR;
        const delta = e.key === "ArrowLeft" ? -step : step;
        patch({ at: (BigInt(at) + delta).toString() });
        return;
      }
      if (e.key === " ") {
        if (onButton) return;
        e.preventDefault();
        patch({ at: state.at === null ? at : null });
        return;
      }
      if (e.key === "Enter") {
        if (onButton) return;
        if (state.entity !== null) patch({ dock: "row" });
        return;
      }
      if (e.key === "Escape") {
        if (dataHealthOpen) setDataHealthOpen(false);
        else if (state.dock !== null) patch({ dock: null });
        else if (state.focus !== null) patch({ focus: null });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  const tableReady = activeView !== undefined;
  const heatmapReady = tableReady && activeView.availability === "available";

  return (
    <div
      data-testid="app-shell"
      style={{
        background: "var(--bg)",
        color: "var(--fg)",
        minHeight: "100dvh",
        display: "flex",
        flexDirection: "column",
      }}
    >
      <Header
        state={state}
        context={context.data}
        incidents={incidents.data}
        // Share always carries the absolute cursor time: a LIVE link must
        // reproduce this exact screen for the recipient.
        shareUrl={`${location.origin}${location.pathname}${toHash({ ...state, at })}`}
        dataHealthOpen={dataHealthOpen}
        onToggleDataHealth={() => setDataHealthOpen((open) => !open)}
        onOpenIncidents={() => patch({ dock: "incidents" })}
      />
      <AlertBar live={state.at === null} summary={summary.data} />
      {mobile ? (
        <main
          data-testid="mobile-triage"
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-2)",
            padding: "var(--space-2) var(--space-3)",
            fontFamily: "var(--ui-font)",
          }}
        >
          <span style={{ color: "var(--fg-dim)", fontSize: "var(--text-sm)" }}>
            {t("app.mobileTriage")}
          </span>
          {/* Mobile triage shows the incidents inline — the chip-only path
              read as an empty, broken page. */}
          {incidents.isLoading && (
            <span style={{ color: "var(--fg-dim)" }}>
              {t("dock.incidents.loading")}
            </span>
          )}
          {incidents.isError && (
            <span role="alert" style={{ color: "var(--sev-warn-fg)" }}>
              {t("dock.incidents.error")}
            </span>
          )}
          {incidents.isSuccess &&
            (incidents.data?.incidents.length ?? 0) === 0 && (
              <span style={{ color: "var(--fg-dim)" }}>
                {t("dock.incidents.empty")}
              </span>
            )}
          {(incidents.data?.incidents ?? []).map((incident) => (
            <button
              key={incident.incident_key}
              type="button"
              data-incident={incident.incident_key}
              onClick={() =>
                patch({ dock: "incidents", focus: incident.incident_key })
              }
              style={{
                display: "block",
                width: "100%",
                textAlign: "start",
                background: "var(--bg-raised)",
                border: "1px solid var(--border)",
                borderInlineStart: `4px solid ${
                  incident.level === "critical"
                    ? "var(--sev-crit)"
                    : incident.level === "warning"
                      ? "var(--sev-warn)"
                      : "var(--border)"
                }`,
                borderRadius: "var(--radius-sm)",
                padding: "8px 10px",
                color: "var(--fg)",
                cursor: "pointer",
              }}
            >
              <span
                style={{
                  display: "block",
                  fontFamily: "var(--ui-font)",
                  fontSize: "var(--text-sm)",
                  overflowWrap: "anywhere",
                }}
                title={incident.incident_key}
              >
                {t(`incident.summary.${incident.summary_code}`, {
                  defaultValue: incident.summary_code,
                })}
              </span>
              <span
                style={{
                  display: "block",
                  color: "var(--fg-dim)",
                  fontSize: "var(--text-xs)",
                }}
              >
                {t("dock.incidents.counts", {
                  members: incident.members.length,
                  findings: incident.findings.length,
                })}
              </span>
            </button>
          ))}
        </main>
      ) : (
        <main
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-2)",
            padding: "var(--space-2) var(--space-3)",
          }}
        >
          {catalog.isSuccess && (
            <TabBar
              views={views}
              active={state.view}
              onSelect={selectView}
              summaries={
                new Map((summary.data?.views ?? []).map((v) => [v.view, v]))
              }
            />
          )}
          <Spine
            at={state.at}
            span={state.span}
            baseline={state.baseline}
            onSelectAt={(nextAt) => patch({ at: nextAt })}
            onSelectSpan={(span) => patch({ span })}
            onSelectBaseline={(baseline) => patch({ baseline })}
          />
          {focusedIncident !== undefined && (
            <FocusBar
              incident={focusedIncident}
              onExit={() => patch({ focus: null })}
            />
          )}
          {tableReady && (
            <PageHeader
              view={activeView}
              summary={summary.data?.views.find((v) => v.view === state.view)}
              matched={matched}
            />
          )}
          {heatmapReady && (
            <HeatmapStrip
              view={activeView}
              metric={
                metricByView[activeView.code] ?? activeView.canonical_metric
              }
              from={heatmapRange.from}
              to={heatmapRange.to}
              onMetricChange={(m) =>
                setMetricByView((prev) => ({ ...prev, [activeView.code]: m }))
              }
              onSelectEntity={(entity) => patch({ entity, dock: "row" })}
            />
          )}
          {tableReady && (
            <Toolbar
              view={activeView}
              preset={state.preset}
              q={state.q}
              matched={matched}
              onSelectPreset={(preset) => patch({ preset })}
              onFilter={(q) => patch({ q })}
            />
          )}
          {tableReady && (
            <TableView
              view={activeView}
              at={at}
              span={state.span}
              preset={state.preset}
              q={state.q}
              sort={state.sort}
              order={state.order}
              entity={state.entity}
              onSort={(sort, order) => patch({ sort, order })}
              onSelectRow={(entity) => patch({ entity, dock: "row" })}
              onMatched={setMatched}
            />
          )}
        </main>
      )}
      <div style={{ flex: 1 }} />
      <StatusBar state={state} summary={summary.data} />
      <DockOverlay
        state={state}
        view={activeView}
        at={at}
        onClose={() => patch({ dock: null })}
        onSelectIncident={(focus) => patch({ focus })}
        onPatch={patch}
      />
    </div>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <Shell />
    </QueryClientProvider>
  );
}
