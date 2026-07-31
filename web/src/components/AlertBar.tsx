import { useTranslation } from "react-i18next";
import type { ViewSummaryResponse } from "../api/types";

export interface AlertBarProps {
  live: boolean;
  summary: ViewSummaryResponse | undefined;
}

export function AlertBar(props: AlertBarProps) {
  const { t } = useTranslation();
  const stale =
    props.live &&
    props.summary !== undefined &&
    (props.summary.quality.status !== "complete" ||
      props.summary.quality.gaps.length > 0);
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
