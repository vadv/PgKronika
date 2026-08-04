import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import {
  availableDestinations,
  destinationForView,
  type NavigationDestination,
  type NavigationDestinationId,
  type NavigationGroup,
} from "../navigation/model";
import { SPANS } from "../state/url";

export interface PrimaryNavigationProps {
  groups: readonly NavigationGroup[];
  activeView: string;
  isLive: boolean;
  span: number;
  onSelect: (viewCode: string) => void;
  onToggleLive: () => void;
  onSelectSpan: (span: number) => void;
}

function availabilityColor(destination: NavigationDestination): string {
  if (destination.availability === "available") return "var(--fg-dim)";
  if (destination.availability === "gated") return "var(--sev-warn)";
  return "var(--border-strong)";
}

export function PrimaryNavigation(props: PrimaryNavigationProps) {
  const { t } = useTranslation();
  const available = useMemo(
    () => availableDestinations(props.groups),
    [props.groups],
  );
  const selected = destinationForView(props.groups, props.activeView);
  const initialFocus =
    selected?.availability === "available" ? selected : available[0];
  const [focusState, setFocusState] = useState<{
    activeView: string;
    id: NavigationDestinationId | null;
  }>({ activeView: props.activeView, id: initialFocus?.id ?? null });
  const refs = useRef(new Map<NavigationDestinationId, HTMLButtonElement>());
  const focusStateIsCurrent = focusState.activeView === props.activeView;
  const focusDestination = available.find(({ id }) => id === focusState.id);
  const focusedId =
    focusStateIsCurrent && focusDestination !== undefined
      ? focusDestination.id
      : (initialFocus?.id ?? null);

  const moveFocus = (
    event: KeyboardEvent<HTMLButtonElement>,
    current: NavigationDestination,
  ) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
      return;
    }
    event.preventDefault();
    if (available.length === 0) return;
    const currentIndex = available.findIndex(({ id }) => id === current.id);
    let nextIndex: number;
    if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = available.length - 1;
    else if (event.key === "ArrowRight")
      nextIndex = (Math.max(0, currentIndex) + 1) % available.length;
    else nextIndex = (currentIndex <= 0 ? available.length : currentIndex) - 1;
    const next = available[nextIndex];
    if (next === undefined) return;
    setFocusState({ activeView: props.activeView, id: next.id });
    refs.current.get(next.id)?.focus();
  };

  return (
    <div
      data-testid="primary-navigation"
      style={{
        height: "32px",
        minWidth: 0,
        display: "flex",
        alignItems: "stretch",
        gap: "8px",
        paddingInline: "var(--space-4)",
        overflow: "hidden",
        background: "var(--bg)",
        color: "var(--fg)",
        fontFamily: "var(--ui-font)",
      }}
    >
      <div
        role="tablist"
        aria-label={t("navigation.destinations")}
        aria-orientation="horizontal"
        style={{
          minWidth: 0,
          display: "flex",
          alignItems: "stretch",
          gap: "8px",
          overflowX: "auto",
          overflowY: "hidden",
          scrollbarWidth: "none",
        }}
      >
        {props.groups.map((group, groupIndex) => (
          <span
            key={group.id}
            role="group"
            aria-label={t(group.labelKey)}
            style={{
              display: "inline-flex",
              alignItems: "stretch",
              paddingInlineStart: groupIndex > 0 ? "var(--space-3)" : 0,
              marginInlineStart: groupIndex > 0 ? "var(--space-1)" : 0,
              borderInlineStart:
                groupIndex > 0 ? "1px solid var(--border)" : "none",
            }}
          >
            <span
              aria-hidden="true"
              style={{
                display: "inline-flex",
                alignItems: "center",
                alignSelf: "center",
                gap: "var(--space-1)",
                marginInline: "var(--space-1)",
                color: "var(--fg-dim)",
                fontSize: "var(--text-xs)",
                fontWeight: 700,
                letterSpacing: "var(--tracking-caps)",
                textTransform: "uppercase",
                whiteSpace: "nowrap",
              }}
            >
              <span
                data-testid={`navigation-group-ordinal-${group.id}`}
                style={{
                  color: "var(--fg-dim)",
                  fontFamily: "var(--mono-font)",
                  fontSize: "var(--text-xs)",
                  fontWeight: 500,
                }}
              >
                {groupIndex + 1}
              </span>
              {t(group.labelKey)}
            </span>
            {group.destinations.map((destination) => {
              const active = selected?.id === destination.id;
              const enabled = destination.availability === "available";
              const availability = t(
                `availability.${destination.availability}`,
                { defaultValue: destination.availability },
              );
              return (
                <button
                  key={destination.id}
                  ref={(node) => {
                    if (node === null) refs.current.delete(destination.id);
                    else refs.current.set(destination.id, node);
                  }}
                  type="button"
                  role="tab"
                  aria-selected={active}
                  aria-disabled={!enabled}
                  aria-label={`${t(destination.labelKey)} · ${availability}`}
                  tabIndex={enabled && focusedId === destination.id ? 0 : -1}
                  onFocus={() =>
                    enabled &&
                    setFocusState({
                      activeView: props.activeView,
                      id: destination.id,
                    })
                  }
                  onKeyDown={(event) => moveFocus(event, destination)}
                  onClick={() =>
                    enabled && props.onSelect(destination.viewCode)
                  }
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: "4px",
                    height: "32px",
                    padding: "0 6px",
                    color: enabled
                      ? active
                        ? "var(--accent-strong)"
                        : "var(--fg)"
                      : "var(--fg-dim)",
                    fontSize: "var(--text-sm)",
                    fontWeight: active ? 600 : 400,
                    whiteSpace: "nowrap",
                    background: "transparent",
                    border: "none",
                    borderBottom: active
                      ? "2px solid var(--accent)"
                      : "2px solid transparent",
                    cursor: enabled ? "pointer" : "default",
                  }}
                >
                  <span
                    aria-hidden="true"
                    style={{
                      width: "4px",
                      height: "4px",
                      borderRadius: "50%",
                      background: availabilityColor(destination),
                    }}
                  />
                  {t(destination.labelKey)}
                </button>
              );
            })}
          </span>
        ))}
      </div>

      <span style={{ flex: 1 }} />
      <button
        type="button"
        aria-pressed={!props.isLive}
        onClick={props.onToggleLive}
        style={{
          alignSelf: "center",
          height: "24px",
          padding: "0 8px",
          color: props.isLive ? "var(--sev-ok-fg)" : "var(--sev-warn-fg)",
          background: props.isLive ? "var(--sev-ok-bg)" : "var(--sev-warn-bg)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-sm)",
          fontSize: "var(--text-xs)",
          fontWeight: 600,
          cursor: "pointer",
          whiteSpace: "nowrap",
        }}
      >
        {props.isLive ? t("spine.live") : t("spine.replay")}
      </button>
      <div
        role="group"
        aria-label={t("spine.zoom")}
        style={{
          alignSelf: "center",
          display: "inline-flex",
          height: "24px",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-sm)",
          overflow: "hidden",
          flexShrink: 0,
        }}
      >
        {SPANS.map((preparedSpan) => (
          <button
            key={preparedSpan}
            type="button"
            aria-pressed={props.span === preparedSpan}
            onClick={() => props.onSelectSpan(preparedSpan)}
            style={{
              padding: "0 8px",
              color:
                props.span === preparedSpan
                  ? "var(--accent-strong)"
                  : "var(--fg-dim)",
              background:
                props.span === preparedSpan
                  ? "var(--active-bg)"
                  : "transparent",
              border: "none",
              borderInlineStart:
                preparedSpan === SPANS[0] ? "none" : "1px solid var(--border)",
              fontSize: "var(--text-xs)",
              cursor: "pointer",
            }}
          >
            {t(`spine.span.${preparedSpan}`)}
          </button>
        ))}
      </div>
    </div>
  );
}
