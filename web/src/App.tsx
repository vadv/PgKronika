import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { useTranslation } from "react-i18next";
import { useCatalog } from "./api/catalog";
import { apiRetryDelay, isWarmingUp, retryApiRequest } from "./api/client";
import { useUiContext } from "./api/context";
import { useIncidents } from "./api/incidents";
import { useSummary } from "./api/summary";
import type { ViewSpec } from "./api/types";
import { AlertBar } from "./components/AlertBar";
import { DockOverlay } from "./components/DockOverlay";
import { FocusBar } from "./components/FocusBar";
import { ForensicSearch } from "./components/ForensicSearch";
import { Header } from "./components/Header";
import { HeatmapStrip } from "./components/HeatmapStrip";
import { PrimaryNavigation } from "./components/PrimaryNavigation";
import { ShellLayout } from "./components/ShellLayout";
import { HealthLine } from "./components/HealthLine";
import { StatusBar } from "./components/StatusBar";
import { PageHeader } from "./components/PageHeader";
import { TableView } from "./components/TableView";
import { Toolbar, type PreparedLens } from "./components/Toolbar";
import { WorkloadEvidencePanel } from "./components/WorkloadEvidencePanel";
import {
  availableDestinations,
  buildNavigationGroups,
} from "./navigation/model";
import { TimeGeometryProvider, useTimeGeometry } from "./state/timeGeometry";
import { toHash } from "./state/url";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: retryApiRequest,
      retryDelay: apiRetryDelay,
    },
  },
});

/** Incidents are always queried over the trailing 24 h (µs). */
const INCIDENTS_WINDOW_US = 86_400_000_000n;
/** Shift+Arrow jump length (1 h, µs). */
const HOUR_US = 3_600_000_000n;
/** Arrow keys step through 1/48 of the active span. */
const STEP_DIVISOR = 48n;
const MOBILE_QUERY = "(max-width: 760px)";

const STATEMENT_LENSES = [
  ["workload", "time"],
  ["latency", "latency"],
  ["buffers", "io"],
  ["wal", "wal"],
  ["temp", "temp"],
  ["planning", "planning"],
] as const;
const ACTIVITY_LENSES = [
  ["overview", "overview"],
  ["waitsLocks", "waits_locks"],
  ["duration", "duration"],
  ["cpu", "cpu"],
  ["diskIo", "disk_io"],
  ["replication", "replication"],
  ["sampling", "sampling"],
] as const;
const PLAN_LENSES = [
  ["planExecution", "time"],
  ["planBuffers", "io"],
  ["planRows", "rows"],
  ["planChanges", "change_timeline"],
] as const;

function catalogPreparedLens(
  view: ViewSpec | undefined,
  code: string,
  preset: string,
): PreparedLens {
  if (view === undefined) {
    return { code, preset, availability: "gated", reason: "view_missing" };
  }
  const presetSpec = view.presets.find(
    (candidate) => candidate.code === preset,
  );
  if (presetSpec === undefined) {
    return {
      code,
      preset,
      availability: "gated",
      reason: "preset_not_published",
    };
  }
  const requiredColumns = presetSpec.columns
    .map((columnCode) =>
      view.columns.find((column) => column.code === columnCode),
    )
    .filter((column) => column !== undefined);
  const unavailable =
    requiredColumns.find((column) => column.availability === "not_collected") ??
    requiredColumns.find((column) => column.availability !== "available");
  if (unavailable === undefined) {
    return { code, preset, availability: "available" };
  }
  return {
    code,
    preset,
    availability:
      unavailable.availability === "not_collected" ? "not_collected" : "gated",
    reason: unavailable.unavailable_reason ?? unavailable.availability,
  };
}

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
  const {
    state,
    range,
    cursorUs: at,
    hoverUs,
    brushDraft,
    patchUiState: patch,
    setCursor,
    setSpan,
    toggleLive,
  } = useTimeGeometry();
  const [dataHealthOpen, setDataHealthOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [matched, setMatched] = useState<number | null>(null);
  const [metricByView, setMetricByView] = useState<Record<string, string>>({});
  const mobile = useMobile();

  const incidentsRange = {
    from: (BigInt(at) - INCIDENTS_WINDOW_US).toString(),
    to: at,
  };
  const heatmapRange = {
    from: range.fromUs,
    to: range.toUs,
  };

  const catalog = useCatalog();
  const summary = useSummary(at);
  const context = useUiContext(at);
  const incidents = useIncidents(incidentsRange);

  const views = catalog.data?.views ?? [];
  const navigationGroups = buildNavigationGroups(views);
  const shortcutDestinations = availableDestinations(navigationGroups);
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

  const onTableSort = useCallback(
    (sort: string | null, order: "asc" | "desc" | null) =>
      patch({ sort, order }),
    [patch],
  );
  const onSelectRow = useCallback(
    (entity: string) => patch({ entity, dock: "row" }),
    [patch],
  );

  const shortcutRef = useRef<(event: KeyboardEvent) => void>(() => {});
  shortcutRef.current = (e: KeyboardEvent) => {
    // A component that already handled the key (slider, button) owns it.
    if (e.defaultPrevented) return;
    if (isEditableTarget(e.target)) return;
    // Enter/Space on a focused button belong to the button, not to
    // global shortcuts.
    const onButton =
      e.target instanceof HTMLElement && e.target.tagName === "BUTTON";
    if (e.key === "/") {
      e.preventDefault();
      setSearchOpen(true);
      return;
    }
    if (e.key >= "1" && e.key <= "9") {
      if (mobile) return;
      const destination = shortcutDestinations[Number(e.key) - 1];
      if (destination !== undefined) selectView(destination.viewCode);
      return;
    }
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      if (onButton) return;
      e.preventDefault();
      const step = e.shiftKey
        ? HOUR_US
        : (BigInt(state.span) * 1_000_000n) / STEP_DIVISOR;
      const delta = e.key === "ArrowLeft" ? -step : step;
      setCursor((BigInt(at) + delta).toString());
      return;
    }
    if (e.key === " ") {
      if (onButton) return;
      e.preventDefault();
      toggleLive();
      return;
    }
    if (e.key === "Enter") {
      if (onButton) return;
      if (state.entity !== null) patch({ dock: "row" });
      return;
    }
    if (e.key === "Escape") {
      if (searchOpen) setSearchOpen(false);
      else if (dataHealthOpen) setDataHealthOpen(false);
      else if (state.dock !== null) patch({ dock: null });
      else if (state.focus !== null) patch({ focus: null });
    }
  };

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => shortcutRef.current(event);
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const tableReady = activeView !== undefined;
  const heatmapReady = tableReady && activeView.availability === "available";
  const statementsScreen = activeView?.code === "statements";
  const activityScreen = activeView?.code === "activity";
  const plansScreen = activeView?.code === "plans";
  const evidencePanelScreen = activityScreen || plansScreen;
  const denseHeatmapScreen = activityScreen || statementsScreen || plansScreen;
  const effectivePreset = statementsScreen
    ? (state.preset ?? "time")
    : activityScreen
      ? (state.preset ?? "overview")
      : plansScreen
        ? (state.preset ?? "time")
        : state.preset;
  const preparedLenses: PreparedLens[] | undefined = statementsScreen
    ? [
        ...STATEMENT_LENSES.map(([code, preset]) =>
          catalogPreparedLens(activeView, code, preset),
        ),
        {
          code: "regression",
          preset: null,
          availability: "gated",
          reason: t("statements.lens.regression.reason"),
        },
        {
          code: "observedSamples",
          preset: null,
          availability: "gated",
          reason: t("statements.lens.observedSamples.reason"),
        },
      ]
    : activityScreen
      ? [
          ...ACTIVITY_LENSES.map(([code, preset]) =>
            catalogPreparedLens(activeView, code, preset),
          ),
          {
            code: "memory",
            preset: null,
            availability: "not_collected" as const,
            reason: t("activity.lens.memory.reason"),
          },
          {
            code: "xidHorizon",
            preset: null,
            availability: "gated" as const,
            reason: t("activity.lens.xidHorizon.reason"),
          },
        ]
      : plansScreen
        ? [
            ...PLAN_LENSES.map(([code, preset]) =>
              catalogPreparedLens(activeView, code, preset),
            ),
            {
              code: "planCompare",
              preset: null,
              availability: "gated" as const,
              reason: t("plans.lens.compare.reason"),
            },
          ]
        : undefined;
  const evidenceNote = statementsScreen
    ? t("statements.evidenceNote")
    : activityScreen
      ? t("activity.evidenceNote")
      : plansScreen
        ? t("plans.evidenceNote")
        : undefined;
  const filterHint = statementsScreen
    ? t("statements.filterHint")
    : activityScreen
      ? t("activity.filterHint")
      : plansScreen
        ? t("plans.filterHint")
        : undefined;
  // A preset owns its default ranking. Carrying an explicit sort from the
  // previous lens can leave the matrix invisibly ranked by a hidden column.
  const selectPreset = (preset: string | null) =>
    patch({ preset, sort: null, order: null });

  const globalContext = (
    <Header
      embedded
      mobile={mobile}
      range={range}
      context={context.data}
      incidents={incidents.data}
      // Share always carries the absolute cursor time: a LIVE link must
      // reproduce this exact screen for the recipient.
      shareUrl={`${location.origin}${location.pathname}${toHash({ ...state, at: range.toUs })}`}
      dataHealthOpen={dataHealthOpen}
      onToggleDataHealth={() => setDataHealthOpen((open) => !open)}
      onOpenIncidents={() => patch({ dock: "incidents" })}
      onOpenSearch={() => setSearchOpen(true)}
    />
  );
  const primaryNavigation = !mobile ? (
    <PrimaryNavigation
      groups={navigationGroups}
      activeView={state.view}
      isLive={state.at === null}
      span={state.span}
      onSelect={selectView}
      onToggleLive={toggleLive}
      onSelectSpan={setSpan}
    />
  ) : null;

  return (
    <ShellLayout
      mobile={mobile}
      globalContext={globalContext}
      primaryNavigation={primaryNavigation}
      primaryNavigationLabel={t("navigation.primary")}
      status={<StatusBar embedded state={state} summary={summary.data} />}
      overlay={
        <>
          <DockOverlay
            state={state}
            view={activeView}
            at={at}
            mobile={mobile}
            onClose={() => patch({ dock: null })}
            onSelectIncident={(focus) => patch({ focus })}
            onPatch={patch}
          />
          <ForensicSearch
            open={searchOpen}
            views={views}
            at={at}
            span={state.span}
            onClose={() => setSearchOpen(false)}
            onSelect={(view, entity) => {
              patch({
                view,
                entity,
                dock: "row",
                q: null,
                preset: null,
                sort: null,
                order: null,
              });
              setSearchOpen(false);
            }}
          />
        </>
      }
    >
      <AlertBar live={state.at === null} summary={summary.data} />
      {mobile ? (
        <div
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
          {incidents.isPending && (
            <span style={{ color: "var(--fg-dim)" }}>
              {incidents.failureCount > 0 &&
              isWarmingUp(incidents.failureReason)
                ? t("loading.warming")
                : t("dock.incidents.loading")}
            </span>
          )}
          {incidents.isError && (
            <span role="alert" style={{ color: "var(--sev-warn-fg)" }}>
              {isWarmingUp(incidents.error)
                ? t("error.warming")
                : t("dock.incidents.error")}
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
          {statementsScreen && heatmapReady && (
            <section
              data-testid="mobile-statements-workspace"
              style={{
                display: "flex",
                flexDirection: "column",
                gap: "var(--space-2)",
                minWidth: 0,
              }}
            >
              <HealthLine />
              <PageHeader
                view={activeView}
                summary={summary.data?.views.find(
                  (view) => view.view === state.view,
                )}
                matched={matched}
                live={state.at === null}
                onOpenIncidents={() => patch({ dock: "incidents" })}
              />
              <Toolbar
                view={activeView}
                preset={effectivePreset}
                q={state.q}
                matched={matched}
                onSelectPreset={selectPreset}
                onFilter={(q) => patch({ q })}
                lenses={preparedLenses}
                contextNote={evidenceNote}
                filterHint={filterHint}
              />
              <div style={{ minWidth: 0, overflowX: "auto" }}>
                <div style={{ minWidth: "1040px" }}>
                  <HeatmapStrip
                    view={activeView}
                    metric={
                      metricByView[activeView.code] ??
                      activeView.canonical_metric
                    }
                    from={heatmapRange.from}
                    to={heatmapRange.to}
                    selectedRange={range}
                    cursorUs={at}
                    hoverUs={hoverUs}
                    brushDraft={brushDraft}
                    baselineUs={state.baseline}
                    buckets={48}
                    onMetricChange={(metric) =>
                      setMetricByView((previous) => ({
                        ...previous,
                        [activeView.code]: metric,
                      }))
                    }
                    onSelectEntity={(entity) => patch({ entity, dock: "row" })}
                  />
                </div>
              </div>
              <div
                style={{
                  display: "flex",
                  flexDirection: "column",
                  height: "70dvh",
                  minHeight: "520px",
                  minWidth: 0,
                }}
              >
                <TableView
                  view={activeView}
                  at={at}
                  span={state.span}
                  preset={effectivePreset}
                  q={state.q}
                  sort={state.sort}
                  order={state.order}
                  entity={state.entity}
                  onSort={onTableSort}
                  onSelectRow={onSelectRow}
                  onMatched={setMatched}
                />
              </div>
            </section>
          )}
        </div>
      ) : (
        <div
          data-testid="desktop-forensic-content"
          data-layout-boundary="analytical-content"
          style={{
            display: "flex",
            flexDirection: "column",
            flex: "1 1 auto",
            minHeight: 0,
            overflow: "hidden",
            gap: "var(--space-2)",
            padding: "var(--space-2) var(--space-3)",
          }}
        >
          <HealthLine />
          {(state.view === "locks" || state.view === "processes") && (
            <aside
              data-testid="contextual-deep-link"
              role="status"
              style={{
                padding: "6px 10px",
                color: "var(--fg-dim)",
                background: "var(--bg-raised)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius-sm)",
                fontFamily: "var(--ui-font)",
                fontSize: "var(--text-sm)",
              }}
            >
              {t(`navigation.deepLink.${state.view}`)}
            </aside>
          )}
          {focusedIncident !== undefined && (
            <FocusBar
              incident={focusedIncident}
              onExit={() => patch({ focus: null })}
            />
          )}
          {tableReady && (
            <section
              data-shell-region="screen-context"
              style={{
                display: "flex",
                flex: "0 0 72px",
                flexDirection: "column",
                justifyContent: "center",
                gap: "var(--space-1)",
                height: "72px",
                minHeight: "72px",
                minWidth: 0,
                overflow: "hidden",
                padding: "var(--space-1) 2px",
              }}
            >
              <PageHeader
                view={activeView}
                summary={summary.data?.views.find((v) => v.view === state.view)}
                matched={matched}
                live={state.at === null}
                onOpenIncidents={() => patch({ dock: "incidents" })}
              />
              {heatmapReady ? (
                <Toolbar
                  view={activeView}
                  preset={effectivePreset}
                  q={state.q}
                  matched={matched}
                  onSelectPreset={selectPreset}
                  onFilter={(q) => patch({ q })}
                  lenses={preparedLenses}
                  contextNote={evidenceNote}
                  filterHint={filterHint}
                />
              ) : (
                <div
                  role="status"
                  style={{
                    minWidth: 0,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    padding: "4px 8px",
                    color: "var(--fg-dim)",
                    background: "var(--bg-raised)",
                    border: "1px solid var(--border)",
                    borderRadius: "var(--radius-sm)",
                    fontFamily: "var(--ui-font)",
                    fontSize: "var(--text-sm)",
                  }}
                >
                  {t("view.gated")} ·{" "}
                  {t("view.gatedHint", {
                    reason: t(`availability.${activeView.availability}`, {
                      defaultValue: activeView.availability,
                    }),
                  })}
                </div>
              )}
            </section>
          )}
          {heatmapReady && (
            <div
              data-shell-region="analytical-center"
              data-testid={
                evidencePanelScreen ? "workload-analytical-center" : undefined
              }
              style={{
                display: "grid",
                gridTemplateColumns: evidencePanelScreen
                  ? "minmax(0, 1fr) minmax(320px, 0.32fr)"
                  : "minmax(0, 1fr)",
                gap: "var(--space-2)",
                flex: "0 0 156px",
                height: "156px",
                minHeight: 0,
                overflow: "hidden",
              }}
            >
              <div style={{ minWidth: 0, overflow: "hidden" }}>
                <HeatmapStrip
                  view={activeView}
                  metric={
                    metricByView[activeView.code] ?? activeView.canonical_metric
                  }
                  from={heatmapRange.from}
                  to={heatmapRange.to}
                  selectedRange={range}
                  cursorUs={at}
                  hoverUs={hoverUs}
                  brushDraft={brushDraft}
                  baselineUs={state.baseline}
                  buckets={denseHeatmapScreen ? 96 : undefined}
                  onMetricChange={(m) =>
                    setMetricByView((prev) => ({
                      ...prev,
                      [activeView.code]: m,
                    }))
                  }
                  onSelectEntity={(entity) => patch({ entity, dock: "row" })}
                />
              </div>
              {evidencePanelScreen && (
                <WorkloadEvidencePanel
                  view={activeView}
                  preset={effectivePreset}
                  at={at}
                  span={state.span}
                  onOpenEntity={(view, entity) =>
                    patch({
                      view,
                      entity,
                      dock: "row",
                      ...(view === state.view
                        ? {}
                        : { preset: null, sort: null, order: null }),
                    })
                  }
                />
              )}
            </div>
          )}
          {heatmapReady && (
            <TableView
              view={activeView}
              at={at}
              span={state.span}
              preset={effectivePreset}
              q={state.q}
              sort={state.sort}
              order={state.order}
              entity={state.entity}
              onSort={onTableSort}
              onSelectRow={onSelectRow}
              onMatched={setMatched}
            />
          )}
        </div>
      )}
    </ShellLayout>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <TimeGeometryProvider>
        <Shell />
      </TimeGeometryProvider>
    </QueryClientProvider>
  );
}
