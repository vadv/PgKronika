import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { ViewSpec } from "../api/types";

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
        gap: "4px",
        alignItems: "center",
        padding: "4px 8px",
        borderBottom: "1px solid var(--border)",
        fontFamily: "var(--mono-font)",
      }}
    >
      {props.view.presets.map((p) => {
        const active = props.preset === p.code;
        return (
          <button
            key={p.code}
            type="button"
            aria-pressed={active}
            onClick={() => props.onSelectPreset(active ? null : p.code)}
            style={{
              fontFamily: "var(--mono-font)",
              color: active ? "var(--accent)" : "var(--fg)",
              background: "none",
              border: "none",
              borderBottom: active
                ? "2px solid var(--accent)"
                : "2px solid transparent",
              cursor: "pointer",
            }}
          >
            {p.code}
          </button>
        );
      })}
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
        style={{
          fontFamily: "var(--mono-font)",
          color: "var(--fg)",
          background: "var(--bg-raised)",
          border: "1px solid var(--border)",
          padding: "2px 6px",
          marginInlineStart: "8px",
        }}
      />
      {props.matched !== null && (
        <span style={{ marginInlineStart: "auto", color: "var(--fg-dim)" }}>
          {t("toolbar.rows", { count: props.matched })}
        </span>
      )}
    </div>
  );
}
