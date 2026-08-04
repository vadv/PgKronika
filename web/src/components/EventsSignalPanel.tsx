import { useTranslation } from "react-i18next";
import { eventKindLabel } from "../api/codes";
import type { EventFact } from "../api/types";
import { useTimelineEvents } from "../api/timeline";
import { formatNumber, formatTimestampUs } from "../design/format";
import { button as uiButton } from "../design/ui";

interface EventsSignalPanelProps {
  from: string;
  to: string;
  preset: string | null;
  q?: string | null;
  onInvestigate: (view: string, atUs: string, eventInstance: string) => void;
}

const MAX_SIGNAL_LANES = 5;
const EVENT_REQUEST_LIMIT = 50;

export function eventMatchesPreset(
  event: EventFact,
  preset: string | null,
): boolean {
  const code = event.event_kind;
  switch (preset) {
    case "errors":
      return code.includes("error");
    case "checkpoints":
      return code.startsWith("pg.checkpoint.");
    case "vacuum":
      return code.startsWith("pg.maintenance.");
    case "slow":
      return code.startsWith("pg.query.slow_");
    case "collector_health":
      return code.startsWith("collector.");
    default:
      return true;
  }
}

function eventField(event: EventFact, field: string): string | null {
  switch (field) {
    case "category_code":
    case "event_kind":
      return event.event_kind;
    case "severity_code":
      return "severity" in event.payload
        ? String(event.payload.severity)
        : event.notable_class;
    case "entity_kind":
      return event.entity?.kind ?? "";
    case "identity_quality":
      return event.identity_quality;
    case "evidence_quality":
      return event.evidence_quality;
    default:
      return null;
  }
}

type EventGlobAtom =
  { kind: "literal"; value: string } | { kind: "star" } | { kind: "one" };

function splitEventTerms(raw: string): string[] | null {
  const terms: string[] = [];
  let current = "";
  let quoted = false;
  let escaped = false;
  const flush = () => {
    if (current !== "" && current !== "&&") terms.push(current);
    current = "";
  };
  for (const character of raw.trim()) {
    if (/\s/u.test(character) && !quoted) {
      flush();
      continue;
    }
    current += character;
    if (escaped) {
      escaped = false;
    } else if (quoted && character === "\\") {
      escaped = true;
    } else if (character === '"') {
      quoted = !quoted;
    }
  }
  if (quoted || escaped) return null;
  flush();
  return terms;
}

function splitEventField(term: string): [string | null, string] | null {
  let quoted = false;
  let escaped = false;
  let separator = -1;
  for (let index = 0; index < term.length; index += 1) {
    const character = term[index];
    if (escaped) {
      escaped = false;
    } else if (quoted && character === "\\") {
      escaped = true;
    } else if (character === '"') {
      quoted = !quoted;
    } else if (character === "=" && !quoted) {
      if (separator >= 0) return null;
      separator = index;
    }
  }
  if (quoted || escaped) return null;
  if (separator < 0) return [null, term];
  const field = term.slice(0, separator).trim().toLowerCase();
  const glob = term.slice(separator + 1).trim();
  return field === "" || glob === "" ? null : [field, glob];
}

function parseEventGlob(
  raw: string,
  substring: boolean,
): EventGlobAtom[] | null {
  const quoted = raw.startsWith('"') && raw.endsWith('"');
  if ((raw.startsWith('"') || raw.endsWith('"')) && !quoted) return null;
  const value = quoted ? raw.slice(1, -1) : raw;
  if (value === "") return null;
  const atoms: EventGlobAtom[] = [];
  let escaped = false;
  for (const character of value) {
    if (escaped) {
      if (!['"', "\\", "*", "?"].includes(character)) return null;
      atoms.push({ kind: "literal", value: character.toLowerCase() });
      escaped = false;
    } else if (quoted && character === "\\") {
      escaped = true;
    } else if (character === "*") {
      atoms.push({ kind: "star" });
    } else if (character === "?") {
      atoms.push({ kind: "one" });
    } else if (character === '"' || (!quoted && character === "\\")) {
      return null;
    } else {
      for (const folded of character.toLowerCase()) {
        atoms.push({ kind: "literal", value: folded });
      }
    }
  }
  if (escaped || atoms.length === 0) return null;
  return substring && !atoms.some((atom) => atom.kind !== "literal")
    ? [{ kind: "star" }, ...atoms, { kind: "star" }]
    : atoms;
}

function eventGlobMatches(atoms: EventGlobAtom[], observed: string): boolean {
  const value = [...observed.toLowerCase()];
  let previous = Array.from(
    { length: value.length + 1 },
    (_, index) => index === 0,
  );
  for (const atom of atoms) {
    const current = Array.from({ length: value.length + 1 }, () => false);
    if (atom.kind === "star") {
      current[0] = previous[0] ?? false;
      for (let index = 1; index <= value.length; index += 1) {
        current[index] =
          (previous[index] ?? false) || (current[index - 1] ?? false);
      }
    } else if (atom.kind === "one") {
      for (let index = 1; index <= value.length; index += 1) {
        current[index] = previous[index - 1] ?? false;
      }
    } else {
      for (let index = 1; index <= value.length; index += 1) {
        current[index] =
          (previous[index - 1] ?? false) && value[index - 1] === atom.value;
      }
    }
    previous = current;
  }
  return previous[value.length] ?? false;
}

export function eventMatchesQuery(
  event: EventFact,
  query: string | null,
): boolean {
  const typed = query?.trim();
  if (!typed) return true;
  if (new TextEncoder().encode(typed).length > 256) return false;
  const terms = splitEventTerms(typed);
  if (terms === null || terms.length === 0 || terms.length > 16) return false;
  return terms.every((term) => {
    const split = splitEventField(term);
    if (split === null) return false;
    const [field, rawGlob] = split;
    const glob = parseEventGlob(rawGlob, field === null);
    if (glob === null) return false;
    if (field !== null) {
      const observed = eventField(event, field);
      return observed !== null && eventGlobMatches(glob, observed);
    }
    return [
      event.event_kind,
      event.entity?.kind ?? "",
      event.entity?.id ?? "",
    ].some((value) => eventGlobMatches(glob, value));
  });
}

export function investigationView(event: EventFact): string {
  switch (event.entity?.kind) {
    case "host":
    case "filesystem":
    case "cgroup":
      return "processes";
    case "postmaster":
    case "replication_sender":
    case "replication_slot":
      return "activity";
    case "database":
      return "tables";
    default:
      return "events";
  }
}

export function eventAt(event: EventFact): string {
  return String(event.occurred_at_us ?? event.sort_ts_us);
}

function signalPosition(event: EventFact, from: string, to: string): number {
  const start = BigInt(from);
  const end = BigInt(to);
  const span = end - start;
  if (span <= 0n) return 100;
  const at = BigInt(eventAt(event));
  const basisPoints = Number(((at - start) * 10_000n) / span);
  return Math.min(100, Math.max(0, basisPoints / 100));
}

export function EventsSignalPanel(props: EventsSignalPanelProps) {
  const { t } = useTranslation();
  const events = useTimelineEvents({
    from: props.from,
    to: props.to,
    limit: EVENT_REQUEST_LIMIT,
  });
  const matching = [...(events.data?.events ?? [])]
    .filter((event) =>
      props.q?.trim()
        ? eventMatchesQuery(event, props.q)
        : eventMatchesPreset(event, props.preset),
    )
    .sort((left, right) => right.sort_ts_us - left.sort_ts_us);
  const visible = matching.slice(0, MAX_SIGNAL_LANES);

  return (
    <aside
      data-testid="events-signal-panel"
      data-view="events"
      className="event-signals"
    >
      <header className="event-signals__header">
        <strong>{t("eventsSignals.title")}</strong>
        <span
          data-testid="event-signals-summary"
          role="status"
          aria-live="polite"
        >
          {events.data === undefined
            ? t("table.loading")
            : t("eventsSignals.summary", { count: matching.length })}
        </span>
      </header>
      {events.isPending ? (
        <div style={{ color: "var(--fg-dim)" }}>{t("table.loading")}</div>
      ) : events.isError ? (
        <div
          role="alert"
          style={{
            display: "flex",
            alignItems: "center",
            gap: "var(--space-2)",
            color: "var(--sev-warn-fg)",
          }}
        >
          <span>{t("eventsSignals.error")}</span>
          <button
            type="button"
            disabled={events.isFetching}
            onClick={() => void events.refetch()}
            style={{ ...uiButton, marginInlineStart: "auto" }}
          >
            {t("eventsSignals.retry")}
          </button>
        </div>
      ) : visible.length === 0 ? (
        <div style={{ color: "var(--fg-dim)" }}>{t("eventsSignals.empty")}</div>
      ) : (
        <div data-testid="event-signal-lanes" className="event-signals__lanes">
          {visible.map((event) => {
            const target = investigationView(event);
            const label = eventKindLabel(t, event.event_kind);
            const object = event.entity?.kind ?? t("eventsWorkspace.cluster");
            const targetLabel = t(
              target === "processes"
                ? "navigation.destination.os"
                : `tabs.${target}`,
              { defaultValue: target === "processes" ? "OS" : target },
            );
            return (
              <button
                key={event.event_instance_id}
                type="button"
                data-testid="event-signal-lane"
                data-event-instance={event.event_instance_id}
                aria-label={t("eventsSignals.investigate", {
                  event: label,
                  target: targetLabel,
                })}
                title={t("eventsSignals.tooltip", {
                  event: label,
                  object,
                  count: event.supporting_evidence.length,
                })}
                onClick={() =>
                  props.onInvestigate(
                    target,
                    eventAt(event),
                    event.event_instance_id,
                  )
                }
                className="event-signal-lane"
              >
                <time
                  dateTime={new Date(
                    Number(eventAt(event)) / 1000,
                  ).toISOString()}
                >
                  {formatTimestampUs(eventAt(event)).split(", ").at(-1)}
                </time>
                <span className="event-signal-lane__identity">
                  <strong>{label}</strong>
                  <small>{object}</small>
                </span>
                <span
                  className="event-signal-lane__timeline"
                  aria-hidden="true"
                >
                  <i
                    style={{
                      left: `${signalPosition(event, props.from, props.to)}%`,
                    }}
                  />
                </span>
                <strong className="event-signal-lane__count">
                  ×{formatNumber(Math.max(1, event.occurrence_count))}
                </strong>
                <span className="event-signal-lane__target">
                  → {targetLabel}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </aside>
  );
}
