import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";
import type { UiState } from "../state/url";

export interface StatusBarProps {
  state: UiState;
  summary: ViewSummaryResponse | undefined;
}

export function StatusBar(props: StatusBarProps) {
  const { t } = useTranslation();
  const notable = props.summary?.views.filter((v) => v.notable).length ?? 0;
  return (
    <div
      style={{
        height: "100%",
        display: "flex",
        alignItems: "center",
        gap: "8px",
        padding: "4px 12px",
        background: "var(--bg)",
        borderTop: "1px solid var(--border)",
        color: "var(--fg-dim)",
        fontFamily: "var(--ui-font)",
        fontSize: "var(--text-xs)",
        overflow: "hidden",
        whiteSpace: "nowrap",
      }}
    >
      <span
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
        }}
        className="statusbar-hints"
      >
        {t("statusbar.hints")}
      </span>
      <span style={{ flex: 1 }} />
      {notable > 0 && (
        <span
          data-testid="notable-count"
          style={{
            color: "var(--sev-warn-fg)",
            background: "var(--sev-warn-bg)",
            borderRadius: "var(--radius-sm)",
            padding: "0 6px",
            fontFamily: "var(--mono-font)",
          }}
        >
          {t("statusbar.notable")}: {notable}
        </span>
      )}
    </div>
  );
}
