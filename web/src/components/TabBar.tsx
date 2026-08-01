import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { TabBadge } from "./TabBadge";
import { TipRow, Tooltip } from "./Tooltip";

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
        const tip = (
          <span style={{ display: "grid", gap: "2px" }}>
            <span style={{ fontFamily: "var(--mono-font)" }}>{v.code}</span>
            <TipRow label="availability" value={v.availability} />
            {summary?.snapshot_ts_us != null && (
              <TipRow
                label="population"
                value={summary.population ?? "—"}
                mono
              />
            )}
            {summary !== undefined && (
              <TipRow label="status" value={summary.status} />
            )}
            {summary?.notable === true && (
              <TipRow
                label="notable"
                value={`${summary.notable_level} ×${summary.notable_count}`}
              />
            )}
            {v.availability !== "available" && (
              <TipRow
                label="reason"
                value={t(`availability.${v.availability}`)}
              />
            )}
          </span>
        );
        const dotColor =
          v.availability === "available"
            ? "var(--sev-ok)"
            : v.availability === "gated"
              ? "var(--sev-warn)"
              : "var(--fg-dim)";
        return (
          <Tooltip key={v.code} content={tip}>
            <button
              role="tab"
              aria-selected={active}
              aria-disabled={gated}
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: "5px",
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
              <span
                aria-hidden="true"
                style={{
                  width: "7px",
                  height: "7px",
                  borderRadius: "50%",
                  background: dotColor,
                  alignSelf: "center",
                }}
              />
              {t(`tabs.${v.code}`)}
              {!gated && summary && (
                <TabBadge
                  population={summary.population}
                  status={summary.status}
                  notable={summary.notable}
                />
              )}
            </button>
          </Tooltip>
        );
      })}
    </div>
  );
}
