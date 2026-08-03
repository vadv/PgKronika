import { useInfiniteQuery } from "@tanstack/react-query";
import { useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { apiGet } from "../api/client";
import { metricDesc, metricLabel } from "../api/codes";
import { useHeatmap } from "../api/heatmap";
import type {
  FrameColumnDto,
  FrameResponse,
  FrameRowDto,
  FrameValue,
  ViewSpec,
} from "../api/types";
import { formatByUnit } from "../design/format";
import { TableView } from "./TableView";

export interface ActivityWorkspaceProps {
  view: ViewSpec;
  at: string;
  span: number;
  from: string;
  to: string;
  metric: string;
  baselineUs: string | null;
  preset: string | null;
  q: string | null;
  sort: string | null;
  order: "asc" | "desc" | null;
  entity: string | null;
  matched: number | null;
  mobile: boolean;
  onMetricChange: (metric: string) => void;
  onSort: (sort: string | null, order: "asc" | "desc" | null) => void;
  onSelectRow: (entity: string) => void;
  onOpenEntity: (view: string, entity: string) => void;
  onMatched?: (matched: number) => void;
}

function cell(
  columns: FrameColumnDto[],
  row: FrameRowDto,
  code: string,
): FrameValue | null {
  const index = columns.findIndex((column) => column.code === code);
  return index < 0 ? null : (row.cells[index] ?? null);
}

function evidenceText(value: FrameValue | null): string {
  if (value === null) return "—";
  if (typeof value === "boolean") return value ? "true" : "false";
  return String(value);
}

function pidAtLeast(value: FrameValue | null, minimum: number): number | null {
  const parsed =
    typeof value === "number"
      ? value
      : typeof value === "string" && /^\d+$/.test(value.trim())
        ? Number(value)
        : Number.NaN;
  return Number.isSafeInteger(parsed) && parsed >= minimum ? parsed : null;
}

function waiterPid(value: FrameValue | null): number | null {
  return pidAtLeast(value, 1);
}

function blockerPids(value: FrameValue | null): number[] {
  if (typeof value === "number") {
    const pid = pidAtLeast(value, 0);
    return pid === null ? [] : [pid];
  }
  if (typeof value !== "string") return [];
  return value
    .split(",")
    .map((candidate) => pidAtLeast(candidate.trim(), 0))
    .filter((pid): pid is number => pid !== null);
}

interface LockEdge {
  key: string;
  entity: string;
  pid: number;
  blockedBy: number;
  target: string;
  waitAge: FrameValue | null;
}

function expandLockEdges(pages: FrameResponse[]): LockEdge[] {
  const expanded: LockEdge[] = [];
  const seen = new Set<string>();
  for (const page of pages) {
    for (const row of page.rows) {
      const pid = waiterPid(cell(page.columns, row, "pid"));
      if (pid === null) continue;
      const target = evidenceText(cell(page.columns, row, "target"));
      const waitAge = cell(page.columns, row, "wait_age_us");
      for (const blockedBy of blockerPids(
        cell(page.columns, row, "blocked_by"),
      )) {
        const key = `${pid}:${blockedBy}`;
        if (seen.has(key)) continue;
        seen.add(key);
        expanded.push({
          key,
          entity: row.entity,
          pid,
          blockedBy,
          target,
          waitAge,
        });
        if (expanded.length === 3) return expanded;
      }
    }
  }
  return expanded;
}

function ActivityLockEvidence(props: {
  at: string;
  span: number;
  onOpenEntity: (view: string, entity: string) => void;
}) {
  const { t } = useTranslation();
  const frame = useInfiniteQuery({
    queryKey: ["activity-lock-evidence", props.at, props.span],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      apiGet("/v1/frame/{view}", {
        params: {
          path: { view: "locks" },
          query: {
            at: Number(props.at),
            span: `${props.span}s`,
            preset: "tree",
            limit: 16,
            ...(pageParam !== null ? { cursor: pageParam } : {}),
          },
        },
      }),
    getNextPageParam: (lastPage) => lastPage.page.next ?? undefined,
  });
  const edges = useMemo(
    () => expandLockEdges(frame.data?.pages ?? []),
    [frame.data?.pages],
  );
  const { fetchNextPage, hasNextPage, isFetchingNextPage } = frame;
  const lockError = frame.isError || frame.isFetchNextPageError;

  useEffect(() => {
    if (edges.length >= 3 || !hasNextPage || isFetchingNextPage || lockError) {
      return;
    }
    void fetchNextPage();
  }, [edges.length, fetchNextPage, hasNextPage, isFetchingNextPage, lockError]);

  const isSearchingForEdges =
    edges.length === 0 &&
    !lockError &&
    (frame.isPending || hasNextPage || isFetchingNextPage);
  const retryLockEvidence = () => {
    if (frame.isFetchNextPageError) {
      void fetchNextPage();
      return;
    }
    void frame.refetch();
  };

  return (
    <section
      className="activity-lock-evidence"
      data-testid="activity-lock-evidence"
      aria-label={t("activity.matrix.locks.title")}
    >
      <div className="activity-lock-evidence__context">
        <strong>{t("activity.matrix.locks.title")}</strong>
      </div>
      <div className="activity-lock-evidence__edges">
        {isSearchingForEdges && (
          <span className="activity-lock-evidence__state">
            {t("table.loading")}
          </span>
        )}
        {lockError && (
          <button
            type="button"
            className="activity-lock-evidence__state activity-lock-evidence__state--error activity-lock-evidence__retry"
            onClick={retryLockEvidence}
          >
            {t("table.error")} · {t("table.retry")}
          </button>
        )}
        {frame.data !== undefined &&
          edges.length === 0 &&
          !hasNextPage &&
          !isFetchingNextPage &&
          !lockError && (
            <span className="activity-lock-evidence__state">
              {t("activity.locks.empty")}
            </span>
          )}
        {edges.map((edge) => {
          return (
            <button
              key={edge.key}
              type="button"
              className="activity-lock-evidence__edge"
              onClick={() => props.onOpenEntity("locks", edge.entity)}
              aria-label={`${edge.pid} → ${edge.blockedBy} · ${edge.target}`}
            >
              <span className="activity-lock-evidence__pids">
                {edge.pid} → {edge.blockedBy}
              </span>
              <span className="activity-lock-evidence__target">
                {edge.target}
              </span>
              <span className="activity-lock-evidence__age">
                {typeof edge.waitAge === "number"
                  ? formatByUnit(edge.waitAge, "us")
                  : evidenceText(edge.waitAge)}
              </span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

export function ActivityWorkspace(props: ActivityWorkspaceProps) {
  const { t } = useTranslation();
  const buckets = props.mobile ? 48 : 96;
  const temporalLens = props.preset !== "overview";
  const heatmap = useHeatmap({
    view: "activity",
    metric: props.metric,
    from: props.from,
    to: props.to,
    buckets,
    top: 64,
    enabled: temporalLens,
  });
  const metrics = props.view.metrics.filter(
    (metric) => metric.availability === "available",
  );
  const metricText = metricLabel(t, props.view.code, props.metric);

  return (
    <section
      className="activity-workspace"
      data-testid="activity-workspace"
      data-lens={props.preset ?? "overview"}
    >
      <div className="activity-workspace__evidence">
        <div
          className="activity-workspace__snapshot"
          data-testid="activity-point-evidence"
        >
          <strong>{t("activity.snapshotBadge")}</strong>
          {!temporalLens && (
            <span className="activity-workspace__snapshot-hint">
              {t("activity.pointEvidence")}
            </span>
          )}
        </div>
        <div
          className="activity-workspace__metrics"
          role="group"
          aria-label={t("activity.matrix.metricGroup")}
        >
          {metrics.map((metric) => (
            <button
              key={metric.code}
              type="button"
              className="activity-workspace__metric"
              aria-pressed={props.metric === metric.code}
              title={metricDesc(t, props.view.code, metric.code) ?? undefined}
              onClick={() => props.onMetricChange(metric.code)}
            >
              {metricLabel(t, props.view.code, metric.code)}
            </button>
          ))}
        </div>
        <span
          className="activity-workspace__process-link"
          data-testid="activity-process-link"
          title={t("relation.kind.pid.desc")}
        >
          {t("relation.activityProcess.pid")}
        </span>
        <span
          className="activity-workspace__population"
          data-testid="activity-population"
          data-matched={props.matched ?? undefined}
        >
          {t("activity.matrix.backends", { count: props.matched ?? "—" })}
        </span>
      </div>
      {props.preset === "waits_locks" && (
        <ActivityLockEvidence
          at={props.at}
          span={props.span}
          onOpenEntity={props.onOpenEntity}
        />
      )}
      <TableView
        view={props.view}
        at={props.at}
        span={props.span}
        preset={props.preset}
        q={props.q}
        sort={props.sort}
        order={props.order}
        entity={props.entity}
        onSort={props.onSort}
        onSelectRow={props.onSelectRow}
        onMatched={props.onMatched}
        activitySnapshot={!temporalLens}
        timeMatrix={
          temporalLens
            ? {
                kind: "activity",
                evidenceMode:
                  props.metric === "active_fraction"
                    ? "point_samples"
                    : "interval_estimates",
                data: heatmap.data,
                pending: heatmap.isPending,
                error: heatmap.isError,
                metricLabel: metricText,
                cursorUs: props.at,
                baselineUs: props.baselineUs,
                onRetry: () => void heatmap.refetch(),
              }
            : null
        }
      />
    </section>
  );
}
