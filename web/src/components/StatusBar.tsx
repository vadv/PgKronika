import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";
import type { UiState } from "../state/url";

export interface StatusBarProps {
  /** Render content-only when ShellLayout already owns the footer landmark. */
  embedded?: boolean;
  state: UiState;
  summary: ViewSummaryResponse | undefined;
}

export function StatusBar(props: StatusBarProps) {
  const { t } = useTranslation();
  const notable = props.summary?.views.filter((v) => v.notable).length ?? 0;
  const Root = props.embedded === true ? "div" : "footer";
  return (
    <Root
      data-testid="statusbar-content"
      style={{
        height: props.embedded === true ? "100%" : undefined,
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
    </Root>
  );
}
