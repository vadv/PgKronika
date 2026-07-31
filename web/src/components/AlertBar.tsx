import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";

export interface AlertBarProps {
  live: boolean;
  summary: ViewSummaryResponse | undefined;
}

export function AlertBar(props: AlertBarProps) {
  const { t } = useTranslation();
  const q = props.summary?.quality;
  // The active tail (current, still-open snapshot window) is the normal LIVE
  // state, not a defect — warning on it would make the banner permanent.
  // Alert only on what the operator can act on: gaps or degradation NOT
  // explained by the open tail.
  const stale =
    props.live &&
    q !== undefined &&
    (q.gaps.length > 0 || (q.status !== "complete" && !q.active_tail));
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
    </div>
  );
}
