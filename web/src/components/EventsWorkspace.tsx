import { useTranslation } from "react-i18next";
import { eventKindDesc, eventKindLabel } from "../api/codes";
import { useTimelineEvents } from "../api/timeline";
import type { EventFact, ViewSpec } from "../api/types";
import { formatNumber, formatTimestampUs } from "../design/format";
import type { TimeRange } from "../state/timeGeometry";
import {
  EventsSignalPanel,
  eventAt,
  eventMatchesPreset,
  eventMatchesQuery,
  investigationView,
} from "./EventsSignalPanel";
import { HeatmapStrip } from "./HeatmapStrip";
import { eventGlyph, type GlyphTone } from "./spineHealth";
import "./EventsWorkspace.css";

const EVENT_RANGE_LIMIT = 200;

export interface EventFamilySummary {
  code: string;
  count: number;
  facts: number;
  tone: GlyphTone;
}

const TONE_RANK: Record<GlyphTone, number> = {
  dim: 0,
  info: 1,
  warn: 2,
  crit: 3,
};

export function groupEventFamilies(events: EventFact[]): EventFamilySummary[] {
  const groups = new Map<string, EventFamilySummary>();
  for (const event of events) {
    const tone = eventGlyph(event).tone;
    const current = groups.get(event.event_kind);
    if (current === undefined) {
      groups.set(event.event_kind, {
        code: event.event_kind,
        count: Math.max(1, event.occurrence_count),
        facts: 1,
        tone,
      });
      continue;
    }
    current.count += Math.max(1, event.occurrence_count);
    current.facts += 1;
    if (TONE_RANK[tone] > TONE_RANK[current.tone]) current.tone = tone;
  }
  return [...groups.values()].sort(
    (left, right) =>
      right.count - left.count || left.code.localeCompare(right.code),
  );
}

function lossSummary(events: EventFact[]): number {
  return events.reduce(
    (total, event) =>
      total + Math.max(0, event.loss?.lost_count_lower_bound ?? 0),
    0,
  );
}

function qualityLabel(event: EventFact): string {
  return `${event.identity_quality}/${event.evidence_quality}`;
}

export interface EventsWorkspaceProps {
  view: ViewSpec;
  from: string;
  to: string;
  metric: string;
  preset: string | null;
  q: string | null;
  selectedRange: TimeRange;
  cursorUs: string;
  hoverUs: string | null;
  brushDraft: TimeRange | null;
  baselineUs: string | null;
  onMetricChange: (metric: string) => void;
  onSelectEntity: (entity: string) => void;
  onInvestigate: (view: string, atUs: string, eventInstance: string) => void;
}

export function EventsWorkspace(props: EventsWorkspaceProps) {
  const { t } = useTranslation();
  const query = useTimelineEvents({
    from: props.from,
    to: props.to,
    limit: EVENT_RANGE_LIMIT,
  });
  const events = [...(query.data?.events ?? [])]
    .filter((event) =>
      props.q?.trim()
        ? eventMatchesQuery(event, props.q)
        : eventMatchesPreset(event, props.preset),
    )
    .sort((left, right) => right.sort_ts_us - left.sort_ts_us);
  const families = groupEventFamilies(events);
  const occurrences = events.reduce(
    (total, event) => total + Math.max(1, event.occurrence_count),
    0,
  );
  const maxFamilyCount = Math.max(1, ...families.map((family) => family.count));
  const lost = lossSummary(events);

  return (
    <section className="events-workspace" data-testid="events-workspace">
      <div className="events-workspace__overview">
        <div className="events-workspace__heatmap">
          <HeatmapStrip
            view={props.view}
            metric={props.metric}
            from={props.from}
            to={props.to}
            selectedRange={props.selectedRange}
            cursorUs={props.cursorUs}
            hoverUs={props.hoverUs}
            brushDraft={props.brushDraft}
            baselineUs={props.baselineUs}
            buckets={96}
            onMetricChange={props.onMetricChange}
            onSelectEntity={props.onSelectEntity}
          />
        </div>
        <EventsSignalPanel
          from={props.from}
          to={props.to}
          preset={props.preset}
          q={props.q}
          onInvestigate={props.onInvestigate}
        />
      </div>

      <div className="events-workspace__body">
        <section
          className="events-range"
          aria-label={t("eventsWorkspace.rangeTitle")}
        >
          <header className="events-range__header">
            <div>
              <strong>{t("eventsWorkspace.rangeTitle")}</strong>
              <span>{t("eventsWorkspace.rangeHint")}</span>
            </div>
            <div className="events-range__totals" role="status">
              <strong>{formatNumber(occurrences)}</strong>
              <span>{t("eventsWorkspace.occurrences")}</span>
              <strong>{formatNumber(events.length)}</strong>
              <span>{t("eventsWorkspace.facts")}</span>
            </div>
          </header>
          <div className="events-range__columns" aria-hidden="true">
            <span>{t("eventsWorkspace.time")}</span>
            <span>{t("eventsWorkspace.event")}</span>
            <span>{t("eventsWorkspace.entity")}</span>
            <span>{t("eventsWorkspace.quality")}</span>
          </div>
          <div className="events-range__rows" data-testid="event-range-rows">
            {query.isPending ? (
              <span className="events-range__state" aria-busy="true">
                {t("table.loading")}
              </span>
            ) : query.isError ? (
              <div className="events-range__state" role="alert">
                <span>{t("eventsSignals.error")}</span>
                <button type="button" onClick={() => void query.refetch()}>
                  {t("eventsSignals.retry")}
                </button>
              </div>
            ) : events.length === 0 ? (
              <span className="events-range__state">
                {t("eventsSignals.empty")}
              </span>
            ) : (
              events.map((event) => {
                const tone = eventGlyph(event).tone;
                const target = investigationView(event);
                const description = eventKindDesc(t, event.event_kind);
                return (
                  <button
                    key={event.event_instance_id}
                    type="button"
                    className="event-range-row"
                    data-testid="event-range-row"
                    data-tone={tone}
                    data-event-instance={event.event_instance_id}
                    title={description ?? event.event_kind}
                    onClick={() =>
                      props.onInvestigate(
                        target,
                        eventAt(event),
                        event.event_instance_id,
                      )
                    }
                  >
                    <time
                      dateTime={new Date(
                        Number(eventAt(event)) / 1000,
                      ).toISOString()}
                    >
                      {formatTimestampUs(eventAt(event)).split(", ").at(-1)}
                    </time>
                    <span className="event-range-row__event">
                      <span>{eventKindLabel(t, event.event_kind)}</span>
                      <small>{event.event_kind}</small>
                    </span>
                    <span className="event-range-row__entity">
                      {event.entity?.kind ?? t("eventsWorkspace.cluster")}
                    </span>
                    <span className="event-range-row__quality">
                      {qualityLabel(event)}
                      {event.occurrence_count > 1
                        ? ` · ×${formatNumber(event.occurrence_count)}`
                        : ""}
                    </span>
                  </button>
                );
              })
            )}
          </div>
        </section>

        <aside
          className="event-families"
          aria-label={t("eventsWorkspace.families")}
        >
          <header>
            <strong>{t("eventsWorkspace.families")}</strong>
            <span>{formatNumber(families.length)}</span>
          </header>
          <div className="event-families__list">
            {families.map((family) => (
              <div
                className="event-family"
                data-tone={family.tone}
                key={family.code}
              >
                <div className="event-family__label">
                  <span>{eventKindLabel(t, family.code)}</span>
                  <strong>{formatNumber(family.count)}</strong>
                </div>
                <div className="event-family__bar" aria-hidden="true">
                  <span
                    style={{
                      width: `${(family.count / maxFamilyCount) * 100}%`,
                    }}
                  />
                </div>
                <small>{family.code}</small>
              </div>
            ))}
          </div>
          <footer className="event-families__quality">
            <div>
              <span>{t("eventsWorkspace.completeness")}</span>
              <strong>{query.data?.completeness ?? "—"}</strong>
            </div>
            <div>
              <span>{t("eventsWorkspace.retention")}</span>
              <strong>{query.data?.retained_exactness ?? "—"}</strong>
            </div>
            <div>
              <span>{t("eventsWorkspace.omitted")}</span>
              <strong>
                {formatNumber(query.data?.omitted_by_response_filter ?? 0)}
              </strong>
            </div>
            <div>
              <span>{t("eventsWorkspace.lost")}</span>
              <strong>{lost > 0 ? `≥${formatNumber(lost)}` : "0"}</strong>
            </div>
          </footer>
        </aside>
      </div>
    </section>
  );
}
