import { useTranslation } from "react-i18next";
import type { ReactNode } from "react";
import { useDataQuality } from "../api/dataQuality";
import { useStorage } from "../api/storage";

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

/** FreshnessDto carries int64 µs as decimal strings. */
function formatAge(us: string | null): string {
  if (us === null) return "—";
  return `${Math.round(Number(us) / 1_000_000)}s`;
}

function formatTs(us: string | null): string {
  if (us === null) return "—";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "short",
    timeStyle: "medium",
  }).format(new Date(Number(us) / 1000));
}

function formatBytes(n: number): string {
  if (n <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log2(n) / 10));
  const v = n / 2 ** (10 * i);
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function freshnessColor(state: string): string {
  if (state === "fresh") return "var(--sev-ok)";
  if (state === "stale") return "var(--sev-warn)";
  return "var(--fg-dim)";
}

export interface DataHealthPopoverProps {
  /** Window start (int64 µs, decimal string). */
  from: string;
  /** Window end (int64 µs, decimal string). */
  to: string;
}

export function DataHealthPopover(props: DataHealthPopoverProps) {
  const { t } = useTranslation();
  const quality = useDataQuality({ from: props.from, to: props.to });
  const storage = useStorage();
  const dq = quality.data;
  const st = storage.data;

  // Capabilities with a non-available status are the skipped lenses.
  const skipped = (dq?.capabilities ?? []).filter(
    (c) => c.status !== "available",
  );

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
        maxWidth: "min(92vw, 480px)",
        marginTop: "6px",
        padding: "10px 12px",
        background: "var(--bg-overlay)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-lg)",
        boxShadow: "var(--shadow-pop)",
        color: "var(--fg)",
        fontFamily: "var(--mono-font)",
        fontSize: "var(--text-sm)",
        display: "flex",
        flexDirection: "column",
        gap: "6px",
      }}
    >
      {!dq ? (
        <Row label={t("popover.loading")}>
          <span style={{ color: "var(--sev-warn-fg)" }}>
            {quality.isError ? t("popover.error") : "…"}
          </span>
        </Row>
      ) : (
        <>
          <Row label={t("popover.freshness")}>
            <Square color={freshnessColor(dq.freshness.state)} />
            <span>
              {dq.freshness.state} · {t("popover.age")}{" "}
              {formatAge(dq.freshness.age_us)} · {t("popover.dataThrough")}{" "}
              {formatTs(dq.freshness.data_through_us)}
            </span>
          </Row>
          <Row label={t("popover.coverage")}>
            <span>
              {dq.coverage.observed_snapshots}/
              {dq.coverage.expected_snapshots ?? "—"} · {t("popover.complete")}:{" "}
              {dq.coverage.complete_snapshots}
            </span>
          </Row>
          <Row label={t("popover.gaps")}>
            {dq.gaps.length === 0 ? (
              <span>{t("popover.none")}</span>
            ) : (
              <span>
                {dq.gaps
                  .map(
                    (g) =>
                      `${formatTs(g.from_us)}→${formatTs(g.to_us)} · ${g.reason}`,
                  )
                  .join(", ")}
              </span>
            )}
          </Row>
          <Row label={t("popover.lensesSkipped")}>
            {skipped.length === 0 ? (
              <span>{t("popover.none")}</span>
            ) : (
              <span>
                {skipped.map((c) => `${c.code}: ${c.reason ?? "—"}`).join(", ")}
              </span>
            )}
          </Row>
        </>
      )}
      {!st ? (
        <Row label={t("popover.storage")}>
          <span>
            {storage.isError ? t("popover.error") : t("popover.loading")}
          </span>
        </Row>
      ) : (
        <>
          <Row label={t("popover.storage")}>
            <span>
              {formatBytes(
                st.filesystem.total_bytes - st.filesystem.available_bytes,
              )}{" "}
              / {formatBytes(st.filesystem.total_bytes)}
            </span>
          </Row>
          <Row label={t("popover.retention")}>
            <span title={st.retention.reason ?? undefined}>
              {st.retention.status} ·{" "}
              {st.retention.effective_limit_bytes === null
                ? "—"
                : formatBytes(st.retention.effective_limit_bytes)}
            </span>
          </Row>
          <Row label={t("popover.fullIn")}>
            <span
              title={
                st.forecast.full_in_days === null
                  ? (st.forecast.full_in_days_reason ?? undefined)
                  : undefined
              }
            >
              {st.forecast.full_in_days ?? "—"}
            </span>
          </Row>
          <Row label={t("popover.integrity")}>
            <Square color="var(--fg)" />
            <span>
              {t("popover.readable")} {st.integrity.readable_segments} ·{" "}
              {t("popover.quarantined")} {st.integrity.quarantined_entries} ·{" "}
              {t("popover.orphans")} {st.integrity.orphan_overviews}
            </span>
          </Row>
        </>
      )}
    </div>
  );
}
