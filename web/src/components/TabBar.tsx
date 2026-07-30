import { useTranslation } from "react-i18next";
import type { ViewSpec } from "../api/types";

export function TabBar(props: {
  views: ViewSpec[];
  active: string;
  onSelect: (code: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <div role="tablist" style={{ display: "flex", gap: "var(--gap, 4px)" }}>
      {props.views.map((v) => {
        const gated = v.availability !== "available";
        return (
          <button
            key={v.code}
            role="tab"
            aria-selected={props.active === v.code}
            aria-disabled={gated}
            style={{
              fontFamily: "var(--mono-font)",
              color: gated
                ? "var(--fg-dim)"
                : props.active === v.code
                  ? "var(--accent)"
                  : "var(--fg)",
              background: "none",
              border: "none",
              borderBottom:
                props.active === v.code
                  ? "2px solid var(--accent)"
                  : "2px solid transparent",
              cursor: gated ? "default" : "pointer",
            }}
            onClick={() => !gated && props.onSelect(v.code)}
          >
            {t(`tabs.${v.code}`)}
          </button>
        );
      })}
    </div>
  );
}
