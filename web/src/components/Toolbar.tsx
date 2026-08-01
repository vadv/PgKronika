import { useState } from "react";
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
}

export function Toolbar(props: ToolbarProps) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(props.q ?? "");
  // The filter can also change from outside (URL state); adopt it then.
  const [prevQ, setPrevQ] = useState(props.q);
  if (prevQ !== props.q) {
    setPrevQ(props.q);
    setDraft(props.q ?? "");
  }

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
      <span style={sectionTitle}>{t("toolbar.presets")}</span>
      <div
        role="group"
        aria-label={t("toolbar.presets")}
        style={segmentedGroup}
      >
        {props.view.presets.map((p) => {
          const active = props.preset === p.code;
          return (
            <button
              key={p.code}
              type="button"
              aria-pressed={active}
              title={p.code}
              onClick={() => props.onSelectPreset(active ? null : p.code)}
              style={segmentedItem(active)}
            >
              {t(`preset.${p.code}`, { defaultValue: p.code })}
            </button>
          );
        })}
      </div>
      <input
        type="search"
        aria-label={t("toolbar.filter")}
        placeholder={t("toolbar.filter")}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            props.onFilter(draft.trim() || null);
          }
        }}
        style={{ ...input, minWidth: "220px", marginInlineStart: "4px" }}
      />
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
