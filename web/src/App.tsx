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
  const at = state.at ?? String(Date.now() * 1000);
  const incidentsRange = {
    from: (BigInt(at) - INCIDENTS_WINDOW_US).toString(),
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

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target)) return;
      if (e.key >= "1" && e.key <= "9") {
        const view = views[Number(e.key) - 1];
        if (view !== undefined && view.availability === "available") {
          patch({ view: view.code });
        }
        return;
      }
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        const step = e.shiftKey
          ? HOUR_US
          : (BigInt(state.span) * 1_000_000n) / STEP_DIVISOR;
        const delta = e.key === "ArrowLeft" ? -step : step;
        patch({ at: (BigInt(at) + delta).toString() });
        return;
      }
      if (e.key === " ") {
        e.preventDefault();
        patch({ at: state.at === null ? String(Date.now() * 1000) : null });
        return;
      }
      if (e.key === "Enter") {
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
        dataHealthOpen={dataHealthOpen}
        onToggleDataHealth={() => setDataHealthOpen((open) => !open)}
        onOpenIncidents={() => patch({ dock: "incidents" })}
      />
      <AlertBar live={state.at === null} summary={summary.data} />
      {mobile ? (
        <div
          data-testid="mobile-triage"
          style={{
            padding: "8px",
            color: "var(--fg-dim)",
            fontFamily: "var(--ui-font)",
          }}
        >
          {t("app.mobileTriage")}
        </div>
      ) : (
        <>
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
          {catalog.isSuccess && (
            <TabBar
              views={views}
              active={state.view}
              onSelect={(view) => patch({ view })}
              summaries={
                new Map((summary.data?.views ?? []).map((v) => [v.view, v]))
              }
            />
          )}
          {heatmapReady && (
            <HeatmapStrip
              view={activeView}
              metric={
                metricByView[activeView.code] ?? activeView.canonical_metric
              }
              from={incidentsRange.from}
              to={incidentsRange.to}
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
        </>
      )}
      <div style={{ flex: 1 }} />
      <StatusBar state={state} summary={summary.data} />
      <DockOverlay
        state={state}
        view={activeView}
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
