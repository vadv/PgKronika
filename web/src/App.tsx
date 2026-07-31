import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCatalog } from "./api/catalog";
import { useSummary } from "./api/summary";
import { HeatmapStrip } from "./components/HeatmapStrip";
import { TabBar } from "./components/TabBar";
import { parseHash, toHash } from "./state/url";

const queryClient = new QueryClient();

function Shell() {
  const { t } = useTranslation();
  const [state, setState] = useState(() => parseHash(location.hash));
  // Fixed 24h window, computed once per mount (µs).
  const [range] = useState(() => {
    const to = Date.now() * 1000;
    return { from: String(to - 86_400_000_000), to: String(to) };
  });
  const [metricByView, setMetricByView] = useState<Record<string, string>>({});
  const catalog = useCatalog();
  const summary = useSummary(state.at ?? range.to);

  const patch = (p: Partial<typeof state>) => {
    const next = { ...state, ...p };
    setState(next);
    location.hash = toHash(next);
  };

  const activeView = catalog.data?.views.find((v) => v.code === state.view);

  return (
    <div
      data-testid="app-shell"
      style={{
        background: "var(--bg)",
        color: "var(--fg)",
        minHeight: "100dvh",
      }}
    >
      <header style={{ fontFamily: "var(--mono-font)", padding: "4px 8px" }}>
        {t("app.title")}
      </header>
      {catalog.isSuccess && (
        <TabBar
          views={catalog.data.views}
          active={state.view}
          onSelect={(view) => patch({ view })}
          summaries={
            new Map((summary.data?.views ?? []).map((v) => [v.view, v]))
          }
        />
      )}
      {activeView && activeView.availability === "available" && (
        <HeatmapStrip
          view={activeView}
          metric={metricByView[activeView.code] ?? activeView.canonical_metric}
          from={range.from}
          to={range.to}
          onMetricChange={(m) =>
            setMetricByView((prev) => ({ ...prev, [activeView.code]: m }))
          }
          onSelectEntity={() => {}}
        />
      )}
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
