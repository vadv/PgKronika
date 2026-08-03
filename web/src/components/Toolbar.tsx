import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ViewSpec } from "../api/types";
import {
  input,
  sectionTitle,
  segmentedGroup,
  segmentedItem,
} from "../design/ui";

export interface ToolbarProps {
  view: ViewSpec;
  preset: string | null;
  q: string | null;
  matched: number | null;
  onSelectPreset: (preset: string | null) => void;
  onFilter: (q: string | null) => void;
  lenses?: PreparedLens[];
  contextNote?: string;
  filterHint?: string;
}

export interface PreparedLens {
  code: string;
  preset: string | null;
  availability: "available" | "gated" | "not_collected";
  reason?: string;
}

export function Toolbar(props: ToolbarProps) {
  const { t } = useTranslation();
  const filterHintId = useId();
  const [draft, setDraft] = useState(props.q ?? "");
  const [focusedLensReason, setFocusedLensReason] = useState<string | null>(
    null,
  );
  // The filter can also change from outside (URL state); adopt it then.
  const [prevQ, setPrevQ] = useState(props.q);
  if (prevQ !== props.q) {
    setPrevQ(props.q);
    setDraft(props.q ?? "");
  }
  const lenses: PreparedLens[] =
    props.lenses ??
    props.view.presets.map((preset) => ({
      code: preset.code,
      preset: preset.code,
      availability: "available",
    }));
  const groupLabel = t(
    props.lenses === undefined ? "toolbar.presets" : "toolbar.lenses",
  );

  return (
    <div
      style={{
        display: "flex",
        gap: "8px",
        alignItems: "center",
        flexWrap: "wrap",
        padding: "4px 2px",
        fontFamily: "var(--ui-font)",
      }}
    >
      <span style={sectionTitle}>{groupLabel}</span>
      <div role="group" aria-label={groupLabel} style={segmentedGroup}>
        {lenses.map((lens) => {
          const active = props.preset === lens.preset;
          const available = lens.availability === "available";
          const title = available
            ? lens.code
            : `${t(`availability.${lens.availability}`, {
                defaultValue: lens.availability,
              })}: ${lens.reason ?? "—"}`;
          return (
            <button
              key={lens.code}
              type="button"
              aria-pressed={active}
              aria-disabled={!available}
              aria-label={`${t(`lens.${lens.code}`, {
                defaultValue: t(`preset.${lens.code}`, {
                  defaultValue: lens.code,
                }),
              })}${available ? "" : ` · ${title}`}`}
              title={title}
              onFocus={() => {
                if (!available) setFocusedLensReason(title);
              }}
              onBlur={() => setFocusedLensReason(null)}
              onClick={() => {
                if (!available) return;
                if (!active) props.onSelectPreset(lens.preset);
                else if (props.lenses === undefined) props.onSelectPreset(null);
              }}
              style={{
                ...segmentedItem(active),
                ...(available
                  ? undefined
                  : {
                      color: "var(--fg-dim)",
                      cursor: "not-allowed",
                      opacity: 0.58,
                      textDecoration: "underline dotted",
                      textUnderlineOffset: "4px",
                    }),
              }}
            >
              {t(`lens.${lens.code}`, {
                defaultValue: t(`preset.${lens.code}`, {
                  defaultValue: lens.code,
                }),
              })}
            </button>
          );
        })}
      </div>
      <input
        type="search"
        name="view-filter"
        autoComplete="off"
        spellCheck={false}
        aria-label={t("toolbar.filter")}
        aria-describedby={props.filterHint ? filterHintId : undefined}
        placeholder={t("toolbar.filter")}
        title={props.filterHint}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            props.onFilter(draft.trim() || null);
          }
        }}
        style={{ ...input, minWidth: "220px", marginInlineStart: "4px" }}
      />
      {props.filterHint !== undefined && (
        <span
          id={filterHintId}
          style={{
            position: "absolute",
            width: "1px",
            height: "1px",
            padding: 0,
            margin: "-1px",
            overflow: "hidden",
            clip: "rect(0, 0, 0, 0)",
            whiteSpace: "nowrap",
            border: 0,
          }}
        >
          {props.filterHint}
        </span>
      )}
      {(focusedLensReason ?? props.contextNote) !== undefined && (
        <span
          role={focusedLensReason === null ? "note" : "status"}
          aria-live={focusedLensReason === null ? undefined : "polite"}
          title={focusedLensReason ?? props.contextNote}
          style={{
            minWidth: 0,
            maxWidth: "320px",
            overflow: "hidden",
            color: "var(--fg-dim)",
            fontSize: "var(--text-xs)",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {focusedLensReason ?? props.contextNote}
        </span>
      )}
      {props.matched !== null && (
        <span
          style={{
            marginInlineStart: "auto",
            color: "var(--fg-dim)",
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-sm)",
          }}
        >
          {t("toolbar.rows", { count: props.matched })}
        </span>
      )}
    </div>
  );
}
