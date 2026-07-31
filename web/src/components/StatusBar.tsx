import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";
import type { UiState } from "../state/url";

export interface StatusBarProps {
  state: UiState;
  summary: ViewSummaryResponse | undefined;
}

export function StatusBar(props: StatusBarProps) {
  const { t } = useTranslation();
  const notable =
    props.summary?.views.filter((v) => v.notable).length ?? 0;
  return (
    <footer
      style={{
        display: "flex",
        alignItems: "center",
        gap: "8px",
        padding: "2px 8px",
        background: "var(--bg)",
        borderTop: "1px solid var(--border)",
        color: "var(--fg-dim)",
        fontFamily: "var(--mono-font)",
        fontSize: "11px",
      }}
    >
      <span>{t("statusbar.hints")}</span>
      <span style={{ flex: 1 }} />
      {notable > 0 && (
        <span data-testid="notable-count" style={{ color: "var(--sev-warn)" }}>
          {t("statusbar.notable")}: {notable}
        </span>
      )}
    </footer>
  );
}
