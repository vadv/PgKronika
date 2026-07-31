import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCatalog } from "./api/catalog";
import { TabBar } from "./components/TabBar";
import { parseHash, toHash } from "./state/url";

const queryClient = new QueryClient();

function Shell() {
  const { t } = useTranslation();
  const [state, setState] = useState(() => parseHash(location.hash));
  const catalog = useCatalog();

  const patch = (p: Partial<typeof state>) => {
    const next = { ...state, ...p };
    setState(next);
    location.hash = toHash(next);
  };

  return (
    <div data-testid="app-shell" style={{ background: "var(--bg)", color: "var(--fg)", minHeight: "100dvh" }}>
      <header style={{ fontFamily: "var(--mono-font)", padding: "4px 8px" }}>
        {t("app.title")} · {state.source}
      </header>
      {catalog.isSuccess && (
        <TabBar views={catalog.data.views} active={state.view} onSelect={(view) => patch({ view })} />
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
