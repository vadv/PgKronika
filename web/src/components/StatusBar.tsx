import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";
import { formatDurationUs, formatTimestampUs } from "../design/format";
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
  const replay = props.state.at !== null;
  const cursor = props.state.at ?? props.summary?.at_us ?? null;
  const selected = props.state.entity === null ? 0 : 1;
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
      <span data-status-field="mode">
        {t(replay ? "statusbar.mode.replay" : "statusbar.mode.live")}
      </span>
      <span aria-hidden="true">·</span>
      <span
        data-status-field="cursor"
        title={cursor ?? undefined}
        style={{ fontFamily: cursor === null ? undefined : "var(--mono-font)" }}
      >
        {cursor === null
          ? t("statusbar.cursor.latest")
          : formatTimestampUs(cursor)}
      </span>
      <span aria-hidden="true">·</span>
      <span data-status-field="range">
        {t("statusbar.range")}: {formatDurationUs(props.state.span * 1_000_000)}
      </span>
      <span aria-hidden="true">·</span>
      <span data-status-field="baseline">
        {t(
          props.state.baseline === null
            ? "statusbar.baseline.none"
            : "statusbar.baseline.present",
        )}
      </span>
      <span data-status-field="selection">
        {t("statusbar.selection")}: {selected}
      </span>
      <span style={{ flex: 1 }} />
      <span
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
        }}
        className="statusbar-hints"
      >
        {t("statusbar.hints")}
      </span>
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
