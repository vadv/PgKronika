import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import type { HeatmapRow } from "../api/types";
import { formatByUnit } from "../design/format";
import { heatColor } from "./heatmapColor";

export function bucketPosition(
  timeUs: string,
  fromUs: string,
  toUs: string,
  bucketCount: number,
): number | null {
  try {
    const time = BigInt(timeUs);
    const from = BigInt(fromUs);
    const to = BigInt(toUs);
    if (bucketCount <= 0 || to <= from || time < from || time > to) return null;
    return Math.min(
      bucketCount - 1,
      Number(((time - from) * BigInt(bucketCount)) / (to - from)),
    );
  } catch {
    return null;
  }
}

function markerPosition(
  timeUs: string | null,
  fromUs: string,
  toUs: string,
): string | null {
  if (timeUs === null) return null;
  try {
    const time = BigInt(timeUs);
    const from = BigInt(fromUs);
    const to = BigInt(toUs);
    if (to <= from || time < from || time > to) return null;
    const perMille = Number(((time - from) * 100_000n) / (to - from)) / 1000;
    return `${perMille}%`;
  } catch {
    return null;
  }
}

function bucketWindow(
  fromUs: string,
  toUs: string,
  bucketCount: number,
  index: number,
): string {
  const from = BigInt(fromUs);
  const width = (BigInt(toUs) - from) / BigInt(bucketCount);
  const start = new Date(Number(from + width * BigInt(index)) / 1000);
  const end = new Date(Number(from + width * BigInt(index + 1)) / 1000);
  const formatter = new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  return `${formatter.format(start)}–${formatter.format(end)}`;
}

export function TemporalBucketRow(props: {
  row: HeatmapRow | null;
  bucketCount: number;
  gridFromUs: string;
  gridToUs: string;
  cursorUs: string | null;
  baselineUs: string | null;
  metricLabel: string;
  max?: number;
  mode?: "range" | "point_samples";
}) {
  const { t } = useTranslation();
  const mode = props.mode ?? "range";
  const values = Array.from(
    { length: props.bucketCount },
    (_, index) => props.row?.values[index] ?? null,
  );
  const localMax = Math.max(
    0,
    ...values.filter((value): value is number => value !== null),
  );
  const max = props.max ?? localMax;
  const cursor = markerPosition(
    props.cursorUs,
    props.gridFromUs,
    props.gridToUs,
  );
  const baseline = markerPosition(
    props.baselineUs,
    props.gridFromUs,
    props.gridToUs,
  );

  return (
    <div
      className={`temporal-bucket-row temporal-bucket-row--${mode}`}
      data-testid={
        mode === "point_samples" ? "activity-sample-row" : "temporal-row"
      }
      data-mode={mode}
      data-evidence={props.row === null ? "unavailable" : "available"}
      aria-label={
        props.row === null
          ? t(
              mode === "point_samples"
                ? "activity.matrix.seriesUnavailable"
                : "statements.matrix.seriesUnavailable",
            )
          : `${props.row.label} · ${props.metricLabel}`
      }
      style={
        {
          "--time-matrix-buckets": props.bucketCount,
        } as CSSProperties
      }
    >
      {values.map((value, index) => {
        const time = bucketWindow(
          props.gridFromUs,
          props.gridToUs,
          props.bucketCount,
          index,
        );
        return (
          <span
            key={index}
            className="temporal-bucket-row__bucket"
            data-testid="time-matrix-bucket"
            data-empty={value === null ? "true" : undefined}
            data-observed={
              mode === "point_samples" && value !== null ? "true" : undefined
            }
            title={t(
              mode === "point_samples"
                ? "activity.matrix.bucketValue"
                : "statements.matrix.bucketValue",
              {
                time,
                metric: props.metricLabel,
                value:
                  value === null
                    ? t("spine.missing")
                    : formatByUnit(value, props.row?.unit),
              },
            )}
            style={{
              background: heatColor(
                value === null ? null : max > 0 ? value / max : 0,
              ),
            }}
          />
        );
      })}
      {baseline !== null && (
        <span
          className="temporal-bucket-row__baseline"
          data-testid="time-matrix-baseline"
          style={{ left: baseline }}
        />
      )}
      {cursor !== null && (
        <span
          className="temporal-bucket-row__cursor"
          data-testid="time-matrix-cursor"
          style={{ left: cursor }}
        />
      )}
    </div>
  );
}
