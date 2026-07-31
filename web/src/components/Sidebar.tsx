import { useTranslation } from "react-i18next";
import type { ViewSpec, ViewSummaryItem } from "../api/types";
import { TipRow, Tooltip } from "./Tooltip";

/** Small availability dot: present even when the view is gated (honest). */
function AvailabilityDot(props: { availability: string }) {
  const color =
    props.availability === "available"
      ? "var(--sev-ok)"
      : props.availability === "gated"
        ? "var(--sev-warn)"
        : "var(--fg-dim)";
  return (
    <span
      aria-hidden="true"
      style={{
        width: "7px",
        height: "7px",
        borderRadius: "50%",
        background: color,
        flexShrink: 0,
      }}
    />
  );
}

export function Sidebar(props: {
  views: ViewSpec[];
  active: string;
  onSelect: (code: string) => void;
  summaries: Map<string, ViewSummaryItem>;
}) {
  const { t } = useTranslation();
  return (
    <nav
      aria-label={t("sidebar.nav")}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "2px",
        padding: "var(--space-2)",
        background: "var(--bg-raised)",
        borderRight: "1px solid var(--border)",
        minWidth: "190px",
        maxWidth: "190px",
        overflowY: "auto",
      }}
    >
      <span
        style={{
          padding: "2px 8px 6px",
          fontFamily: "var(--ui-font)",
          fontSize: "var(--text-xs)",
          fontWeight: 600,
          textTransform: "uppercase",
          letterSpacing: "var(--tracking-caps)",
          color: "var(--fg-dim)",
        }}
      >
        {t("sidebar.views")}
      </span>
      {props.views.map((v) => {
        const active = props.active === v.code;
        const gated = v.availability !== "available";
        const summary = props.summaries.get(v.code);
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
            {summary?.notable === true && (
              <TipRow
                label="notable"
                value={`${summary.notable_level} ×${summary.notable_count}`}
              />
            )}
          </span>
        );
        return (
          <Tooltip key={v.code} content={tip}>
            <button
              type="button"
              role="tab"
              aria-selected={active}
              aria-disabled={gated}
              onClick={() => !gated && props.onSelect(v.code)}
              style={{
                display: "flex",
                alignItems: "center",
                gap: "8px",
                width: "100%",
                padding: "5px 8px",
                fontFamily: "var(--ui-font)",
                fontSize: "var(--text-md)",
                textAlign: "start",
                color: gated
                  ? "var(--fg-dim)"
                  : active
                    ? "var(--fg-strong)"
                    : "var(--fg)",
                fontWeight: active ? 600 : 400,
                background: active ? "var(--active-bg)" : "transparent",
                border: "none",
                borderRadius: "var(--radius-sm)",
                boxShadow: active ? "inset 2px 0 0 var(--accent)" : "none",
                cursor: gated ? "default" : "pointer",
                transition: "background var(--transition-fast)",
              }}
            >
              <AvailabilityDot availability={v.availability} />
              <span
                style={{
                  flex: 1,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
              >
                {t(`tabs.${v.code}`)}
              </span>
              {summary?.population != null && (
                <span
                  style={{
                    fontFamily: "var(--mono-font)",
                    fontSize: "var(--text-xs)",
                    color: "var(--fg-dim)",
                  }}
                >
                  {summary.population}
                </span>
              )}
              {summary?.notable === true && (
                <span
                  style={{
                    width: "7px",
                    height: "7px",
                    borderRadius: "2px",
                    background:
                      summary.notable_level === "critical"
                        ? "var(--sev-crit)"
                        : "var(--sev-warn)",
                    flexShrink: 0,
                  }}
                  title={`${summary.notable_level} ×${summary.notable_count}`}
                />
              )}
            </button>
          </Tooltip>
        );
      })}
    </nav>
  );
}
