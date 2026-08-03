import { useTranslation } from "react-i18next";
import type { DataState, SemanticKind } from "../design/ui";

export interface SemanticBadgeProps {
  kind?: SemanticKind;
  state?: DataState;
  /** Exact server or collector reason. When absent, the localized state
   * explanation is shown; a reason is never fabricated. */
  reason?: string | null;
}

const STATE_COLOR: Record<DataState, string> = {
  null: "var(--fg-dim)",
  partial: "var(--sev-warn-fg)",
  gated: "var(--sev-warn-fg)",
  reset: "var(--accent-strong)",
  gap: "var(--fg-dim)",
  unsupported: "var(--fg-dim)",
  top_n: "var(--sev-warn-fg)",
};

const STATE_BACKGROUND: Record<DataState, string> = {
  null: "var(--state-muted-bg)",
  partial: "var(--state-caution-bg)",
  gated: "var(--state-caution-bg)",
  reset: "var(--accent-bg)",
  gap: "var(--state-muted-bg)",
  unsupported: "var(--state-muted-bg)",
  top_n: "var(--state-caution-bg)",
};

export function SemanticBadge(props: SemanticBadgeProps) {
  const { t } = useTranslation();

  if (props.state !== undefined) {
    const label = t(`semantic.state.${props.state}.label`);
    const reason =
      props.reason ?? t(`semantic.state.${props.state}.explanation`);
    const value = props.state === "null" ? "—" : label;
    return (
      <span
        data-state={props.state}
        aria-label={`${label}: ${reason}`}
        title={`${label}: ${reason}`}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: "4px",
          minWidth: 0,
          color: STATE_COLOR[props.state],
          background: STATE_BACKGROUND[props.state],
          borderRadius: "var(--radius-sm)",
          padding: "1px 4px",
          fontFamily: "var(--ui-font)",
          fontSize: "var(--text-xs)",
          lineHeight: 1.25,
          whiteSpace: "normal",
        }}
      >
        <span
          style={{
            flex: "none",
            fontFamily: props.state === "null" ? "var(--mono-font)" : undefined,
            fontWeight: 600,
          }}
        >
          {value}
        </span>
        <span style={{ color: "inherit", overflowWrap: "anywhere" }}>
          {reason}
        </span>
      </span>
    );
  }

  if (props.kind === undefined) return null;
  const label = t(`semantic.kind.${props.kind}.label`);
  const explanation = t(`semantic.kind.${props.kind}.explanation`);
  return (
    <span
      data-state="complete"
      aria-label={`${props.kind}: ${label}`}
      title={`${label}: ${explanation}`}
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        minWidth: props.kind === "EST" ? "28px" : "20px",
        height: "18px",
        paddingInline: "4px",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-sm)",
        color: "var(--fg-dim)",
        background: "transparent",
        fontFamily: "var(--mono-font)",
        fontSize: "var(--text-xs)",
        fontWeight: 600,
        lineHeight: 1,
        whiteSpace: "nowrap",
      }}
    >
      {props.kind}
    </span>
  );
}
