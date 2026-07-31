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
  // - the active tail (still-open snapshot window) is the normal LIVE state;
  // - purely capability states (gated inputs, e.g. a missing optional
  //   extension) are availability facts, already shown in the tab bar and
  //   the data-health chip — they are not a data emergency.
  const degradation: string[] = [];
  if (q !== undefined) {
    if (q.gaps.length > 0)
      degradation.push(t("alertbar.reasons.gaps", { count: q.gaps.length }));
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
  const degradedLive =
    q !== undefined &&
    q.status !== "complete" &&
    !q.active_tail &&
    q.gated.length === 0;
  const stale =
    props.live && q !== undefined && (degradation.length > 0 || degradedLive);
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
