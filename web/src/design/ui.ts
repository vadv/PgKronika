import type { CSSProperties } from "react";

/** Metric meaning is separate from its unit. These compact codes are shared
 * by OS, PostgreSQL and derived evidence surfaces. */
export const SEMANTIC_KINDS = ["G", "ΔC", "R", "S", "E", "EST"] as const;
export type SemanticKind = (typeof SEMANTIC_KINDS)[number];

/** Successful responses can still carry an incomplete or unavailable value.
 * Keep these states typed so callers cannot silently collapse one into zero. */
export const DATA_STATES = [
  "null",
  "partial",
  "gated",
  "reset",
  "gap",
  "unsupported",
  "top_n",
] as const;
export type DataState = (typeof DATA_STATES)[number];

/** Shared visual primitives for the v2 chrome — single source so every
 * panel, chip and button speaks the same design language. */

/** Heatmap row-label column width: long lock/relation labels must not
 * truncate while there is room to give them. */
export const HEATMAP_LABEL_COL_PX = 220;

export const font = {
  sans: "var(--ui-font)",
  mono: "var(--mono-font)",
} as const;

export const text = {
  xs: "var(--text-xs)",
  sm: "var(--text-sm)",
  md: "var(--text-md)",
  lg: "var(--text-lg)",
} as const;

export const panel: CSSProperties = {
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-md)",
};

export const sectionTitle: CSSProperties = {
  fontFamily: font.sans,
  fontSize: text.xs,
  fontWeight: 600,
  textTransform: "uppercase",
  letterSpacing: "var(--tracking-caps)",
  color: "var(--fg-dim)",
};

export const chip: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "6px",
  fontFamily: font.sans,
  fontSize: text.sm,
  lineHeight: "18px",
  color: "var(--fg)",
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-sm)",
  padding: "2px 8px",
  whiteSpace: "nowrap",
};

export const chipInteractive: CSSProperties = {
  ...chip,
  cursor: "pointer",
  transition: `border-color var(--transition-fast), background var(--transition-fast)`,
};

export const button: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "6px",
  fontFamily: font.sans,
  fontSize: text.sm,
  lineHeight: "18px",
  color: "var(--fg)",
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-sm)",
  padding: "2px 8px",
  cursor: "pointer",
  transition: `border-color var(--transition-fast), background var(--transition-fast)`,
};

export const buttonAccent: CSSProperties = {
  ...button,
  color: "var(--accent-strong)",
  borderColor: "var(--accent)",
  background: "var(--accent-bg)",
};

export const segmentedGroup: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  background: "var(--bg-raised)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-sm)",
  overflow: "hidden",
};

export const segmentedItem = (active: boolean): CSSProperties => ({
  fontFamily: font.sans,
  fontSize: text.sm,
  lineHeight: "18px",
  padding: "2px 8px",
  border: "none",
  background: active ? "var(--active-bg)" : "transparent",
  color: active ? "var(--accent-strong)" : "var(--fg-dim)",
  cursor: "pointer",
  transition: `color var(--transition-fast), background var(--transition-fast)`,
});

export const input: CSSProperties = {
  fontFamily: font.mono,
  fontSize: text.sm,
  color: "var(--fg)",
  background: "var(--bg)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-sm)",
  padding: "2px 8px",
  outline: "none",
};

export const overlay: CSSProperties = {
  background: "var(--bg-overlay)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-lg)",
  boxShadow: "var(--shadow-pop)",
};

export const skeleton: CSSProperties = {
  display: "inline-block",
  background: "var(--skeleton)",
  borderRadius: "var(--radius-sm)",
  animation: "pgk-pulse 1.4s ease-in-out infinite",
};

export type VerdictLevel = "ok" | "warning" | "critical";

export const verdictTint = (
  level: VerdictLevel,
): { background: string; color: string; borderColor: string } => {
  switch (level) {
    case "ok":
      return {
        background: "var(--sev-ok-bg)",
        color: "var(--sev-ok-fg)",
        borderColor: "var(--sev-ok)",
      };
    case "warning":
      return {
        background: "var(--sev-warn-bg)",
        color: "var(--sev-warn-fg)",
        borderColor: "var(--sev-warn)",
      };
    case "critical":
      return {
        background: "var(--sev-crit-bg)",
        color: "var(--sev-crit-fg)",
        borderColor: "var(--sev-crit)",
      };
  }
};
