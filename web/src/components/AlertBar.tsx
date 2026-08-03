import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";

export interface AlertBarProps {
  live: boolean;
  summary: ViewSummaryResponse | undefined;
}

export function AlertBar(props: AlertBarProps) {
  const { t } = useTranslation();
  const q = props.summary?.quality;
  // The banner is reserved for what the operator can act on, with the reason
  // spelled out — a permanent warning trains people to ignore it:
  // - ordinary gaps, partial snapshots and the active tail are expected
  //   recording facts and remain in explicit diagnostics;
  // - only actionable incompatibility/resource limits interrupt the screen.
  const degradation: string[] = [];
  if (q !== undefined) {
    if (q.unavailable_revision.length > 0)
      degradation.push(
        t("alertbar.reasons.unavailable_revision", {
          count: q.unavailable_revision.length,
        }),
      );
    if (q.resource_limited.length > 0)
      degradation.push(
        t("alertbar.reasons.resource_limited", {
          count: q.resource_limited.length,
        }),
      );
  }
  const stale = props.live && degradation.length > 0;
  if (!stale) return null;
  return (
    <div
      role="alert"
      style={{
        padding: "4px 8px",
        background: "var(--bg-raised)",
        borderBottom: "1px solid var(--sev-warn)",
        color: "var(--sev-warn)",
        fontFamily: "var(--ui-font)",
        fontSize: "12px",
      }}
    >
      {t("alertbar.stale")}
      {degradation.length > 0 ? ` — ${degradation.join(" · ")}` : ""}
    </div>
  );
}
