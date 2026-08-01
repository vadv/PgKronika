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

function ViewButton(props: {
  view: ViewSpec;
  active: boolean;
  collapsed: boolean;
  summary: ViewSummaryItem | undefined;
  label: string;
  tip: React.ReactNode;
  onSelect: () => void;
}) {
  const gated = props.view.availability !== "available";
  return (
    <Tooltip content={props.tip}>
      <button
        type="button"
        role="tab"
        aria-selected={props.active}
        aria-disabled={gated}
        aria-label={props.label}
        onClick={props.onSelect}
        style={{
          display: "flex",
          alignItems: "center",
          gap: "8px",
          width: "100%",
          padding: props.collapsed ? "5px 4px" : "5px 8px",
          justifyContent: props.collapsed ? "center" : "flex-start",
          fontFamily: "var(--ui-font)",
          fontSize: props.collapsed ? "var(--text-xs)" : "var(--text-md)",
          textAlign: "start",
          color: gated
            ? "var(--fg-dim)"
            : props.active
              ? "var(--fg-strong)"
              : "var(--fg)",
          fontWeight: props.active ? 600 : 400,
          background: props.active ? "var(--active-bg)" : "transparent",
          border: "none",
          borderRadius: "var(--radius-sm)",
          boxShadow: props.active ? "inset 2px 0 0 var(--accent)" : "none",
          cursor: gated ? "default" : "pointer",
          transition: "background var(--transition-fast)",
        }}
      >
        <AvailabilityDot availability={props.view.availability} />
        {props.collapsed ? (
          <span style={{ fontFamily: "var(--mono-font)" }}>
            {props.view.code.slice(0, 2)}
          </span>
        ) : (
          <>
            <span
              style={{
                flex: 1,
                overflow: "hidden",
                textOverflow: "ellipsis",
              }}
            >
              {props.label}
            </span>
            {props.summary?.population != null && (
              <span
                style={{
                  fontFamily: "var(--mono-font)",
                  fontSize: "var(--text-xs)",
                  color: "var(--fg-dim)",
                }}
              >
                {props.summary.population}
              </span>
            )}
            {props.summary?.notable === true && (
              <span
                style={{
                  width: "7px",
                  height: "7px",
                  borderRadius: "2px",
                  background:
                    props.summary.notable_level === "critical"
                      ? "var(--sev-crit)"
                      : "var(--sev-warn)",
                  flexShrink: 0,
                }}
                title={`${props.summary.notable_level} ×${props.summary.notable_count}`}
              />
            )}
          </>
        )}
      </button>
    </Tooltip>
  );
}

export function Sidebar(props: {
  views: ViewSpec[];
  active: string;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onSelect: (code: string) => void;
  summaries: Map<string, ViewSummaryItem>;
}) {
  const { t } = useTranslation();
  const collapsed = props.collapsed;
  return (
    <nav
      aria-label={t("sidebar.nav")}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "2px",
        padding: collapsed ? "var(--space-1)" : "var(--space-2)",
        background: "var(--bg-raised)",
        borderRight: "1px solid var(--border)",
        minWidth: collapsed ? "44px" : "190px",
        maxWidth: collapsed ? "44px" : "190px",
        overflowY: "auto",
        transition: "min-width var(--transition-fast)",
      }}
    >
      <button
        type="button"
        onClick={props.onToggleCollapsed}
        aria-label={t(collapsed ? "sidebar.expand" : "sidebar.collapse")}
        title={t(collapsed ? "sidebar.expand" : "sidebar.collapse")}
        style={{
          alignSelf: collapsed ? "center" : "flex-end",
          background: "none",
          border: "none",
          color: "var(--fg-dim)",
          cursor: "pointer",
          padding: "2px 4px",
          fontFamily: "var(--ui-font)",
          fontSize: "var(--text-sm)",
        }}
      >
        {collapsed ? "»" : "«"}
      </button>
      {!collapsed && (
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
      )}
      {props.views.map((v) => {
        const active = props.active === v.code;
        const summary = props.summaries.get(v.code);
        const tip = (
          <span style={{ display: "grid", gap: "2px" }}>
            <span style={{ fontFamily: "var(--mono-font)" }}>
              {t(`tabs.${v.code}`)}
            </span>
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
          <ViewButton
            key={v.code}
            view={v}
            active={active}
            collapsed={collapsed}
            summary={summary}
            label={t(`tabs.${v.code}`)}
            tip={tip}
            onSelect={() =>
              v.availability === "available" && props.onSelect(v.code)
            }
          />
        );
      })}
    </nav>
  );
}
