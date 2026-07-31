import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { TabBadge } from "./TabBadge";

export function TabBar(props: {
  views: ViewSpec[];
  active: string;
  onSelect: (code: string) => void;
  summaries: Map<string, ViewSummaryItem>;
}) {
  const { t } = useTranslation();
  return (
    <div
      role="tablist"
      style={{
        display: "flex",
        gap: "2px",
        borderBottom: "1px solid var(--border)",
        paddingInline: "var(--space-2)",
      }}
    >
      {props.views.map((v) => {
        const gated = v.availability !== "available";
        const summary = props.summaries.get(v.code);
        const active = props.active === v.code;
        return (
          <button
            key={v.code}
            role="tab"
            aria-selected={active}
            aria-disabled={gated}
            style={{
              display: "inline-flex",
              alignItems: "baseline",
              gap: "4px",
              fontFamily: "var(--ui-font)",
              fontSize: "var(--text-md)",
              padding: "4px 8px",
              color: gated
                ? "var(--fg-dim)"
                : active
                  ? "var(--accent-strong)"
                  : "var(--fg)",
              fontWeight: active ? 600 : 400,
              background: "none",
              border: "none",
              borderBottom: active
                ? "2px solid var(--accent)"
                : "2px solid transparent",
              cursor: gated ? "default" : "pointer",
              transition: "color var(--transition-fast)",
            }}
            onClick={() => !gated && props.onSelect(v.code)}
          >
            {t(`tabs.${v.code}`)}
            {!gated && summary && (
              <TabBadge
                population={summary.population}
                status={summary.status}
                notable={summary.notable}
              />
            )}
          </button>
        );
      })}
    </div>
  );
}
