import { useTranslation } from "react-i18next";
import type { ReactNode } from "react";
import type { SummaryQuality, ViewSummaryItem } from "../api/types";

type Collection = NonNullable<ViewSummaryItem["collection"]>;

function Square(props: { color: string }) {
  return (
    <span
      aria-hidden="true"
      style={{
        display: "inline-block",
        width: "10px",
        height: "10px",
        background: props.color,
        flexShrink: 0,
      }}
    />
  );
}

function Row(props: { label: string; children: ReactNode }) {
  return (
    <div style={{ display: "flex", gap: "6px", alignItems: "baseline" }}>
      <span style={{ color: "var(--fg-dim)", minWidth: "140px" }}>
        {props.label}
      </span>
      <span
        style={{
          display: "inline-flex",
          gap: "6px",
          alignItems: "center",
          flexWrap: "wrap",
        }}
      >
        {props.children}
      </span>
    </div>
  );
}

function ratio(c: Collection): number {
  if (c.source_total === null || c.source_total <= 0) return 1;
  return c.collected / c.source_total;
}

export interface DataHealthPopoverProps {
  quality: SummaryQuality;
  views: ViewSummaryItem[];
}

export function DataHealthPopover(props: DataHealthPopoverProps) {
  const { t } = useTranslation();
  const { quality } = props;

  const fresh =
    quality.status === "complete" && !quality.active_tail
      ? "var(--sev-ok)"
      : "var(--sev-warn)";

  const skipped: { label: string; codes: string[] }[] = [
    { label: t("popover.gated"), codes: quality.gated },
    { label: t("popover.resourceLimited"), codes: quality.resource_limited },
    {
      label: t("popover.unavailableRevision"),
      codes: quality.unavailable_revision,
    },
  ].filter((s) => s.codes.length > 0);

  const collected = props.views.filter(
    (v): v is ViewSummaryItem & { collection: Collection } =>
      v.collection !== null,
  );
  const totalCollected = collected.reduce(
    (sum, v) => sum + v.collection.collected,
    0,
  );
  const totalSource = collected.reduce(
    (sum, v) => sum + (v.collection.source_total ?? 0),
    0,
  );
  const worst = [...collected]
    .sort((a, b) => ratio(a.collection) - ratio(b.collection))
    .slice(0, 3);

  return (
    <div
      role="dialog"
      aria-label={t("header.data")}
      style={{
        position: "absolute",
        top: "100%",
        left: 0,
        zIndex: 10,
        minWidth: "360px",
        padding: "8px",
        background: "var(--bg-raised)",
        border: "1px solid var(--border)",
        color: "var(--fg)",
        fontFamily: "var(--mono-font)",
        fontSize: "12px",
        display: "flex",
        flexDirection: "column",
        gap: "4px",
      }}
    >
      <Row label={t("popover.freshness")}>
        <Square color={fresh} />
        <span>
          {quality.status}
          {quality.active_tail ? ` · ${t("popover.activeTail")}` : ""}
        </span>
      </Row>
      <Row label={t("popover.coverage")}>
        <span>
          {quality.snapshots} {t("popover.snapshots")}
        </span>
      </Row>
      <Row label={t("popover.gaps")}>
        {quality.gaps.length === 0 ? (
          <span>{t("popover.none")}</span>
        ) : (
          <span>{quality.gaps.join(", ")}</span>
        )}
      </Row>
      {skipped.map((s) => (
        <Row key={s.label} label={s.label}>
          <Square color="var(--sev-warn)" />
          <span>{s.codes.join(", ")}</span>
        </Row>
      ))}
      {collected.length > 0 && (
        <Row label={t("popover.views")}>
          <span>
            {totalCollected}/{totalSource}
          </span>
        </Row>
      )}
      {worst.map((v) => (
        <Row key={v.view} label={`${t("popover.worst")}: ${v.view}`}>
          <span>
            {v.collection.collected}/{v.collection.source_total ?? "—"}
          </span>
        </Row>
      ))}
    </div>
  );
}
