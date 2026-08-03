import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import type { DataState } from "../design/ui";
import { overlay } from "../design/ui";
import { SemanticBadge } from "./SemanticBadge";

export interface ProvenanceRecord {
  definition?: ReactNode;
  value?: ReactNode;
  unit?: ReactNode;
  window?: ReactNode;
  snapshot?: ReactNode;
  aggregation?: ReactNode;
  formula?: ReactNode;
  baseline?: ReactNode;
  source?: ReactNode;
  producer?: ReactNode;
  coverage?: ReactNode;
  reset?: ReactNode;
  sampling?: ReactNode;
  verdictRule?: ReactNode;
  revision?: ReactNode;
  state?: DataState;
  reason?: ReactNode;
}

export interface ProvenancePopoverProps {
  triggerLabel: string;
  record: ProvenanceRecord;
  renderTrigger?: (state: { open: boolean }) => ReactNode;
}

type FieldName = keyof ProvenanceRecord;
const FIELD_ORDER: readonly FieldName[] = [
  "definition",
  "value",
  "unit",
  "window",
  "snapshot",
  "aggregation",
  "formula",
  "baseline",
  "source",
  "producer",
  "coverage",
  "reset",
  "sampling",
  "verdictRule",
  "revision",
  "state",
  "reason",
];

const VIEWPORT_MARGIN_PX = 8;
const POPOVER_GAP_PX = 6;

function present(value: ReactNode): boolean {
  return value !== null && value !== undefined && value !== "";
}

export function ProvenancePopover(props: ProvenancePopoverProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const id = useId();

  const close = useCallback(() => {
    setOpen(false);
    triggerRef.current?.focus();
  }, []);

  const place = useCallback(() => {
    const trigger = triggerRef.current;
    const popover = popoverRef.current;
    if (trigger === null || popover === null) return;
    const anchor = trigger.getBoundingClientRect();
    const box = popover.getBoundingClientRect();
    const maxLeft = Math.max(
      VIEWPORT_MARGIN_PX,
      window.innerWidth - VIEWPORT_MARGIN_PX - box.width,
    );
    const left = Math.min(maxLeft, Math.max(VIEWPORT_MARGIN_PX, anchor.left));
    const below = anchor.bottom + POPOVER_GAP_PX;
    const desiredTop =
      below + box.height <= window.innerHeight - VIEWPORT_MARGIN_PX
        ? below
        : anchor.top - POPOVER_GAP_PX - box.height;
    const maxTop = Math.max(
      VIEWPORT_MARGIN_PX,
      window.innerHeight - VIEWPORT_MARGIN_PX - box.height,
    );
    const top = Math.min(maxTop, Math.max(VIEWPORT_MARGIN_PX, desiredTop));
    setPosition({ left, top });
  }, []);

  useLayoutEffect(() => {
    if (open) place();
  }, [open, place]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (
        triggerRef.current?.contains(target) === true ||
        popoverRef.current?.contains(target) === true
      ) {
        return;
      }
      close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      close();
    };
    const onViewportChange = () => place();
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("resize", onViewportChange);
    window.addEventListener("scroll", onViewportChange, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", onViewportChange);
      window.removeEventListener("scroll", onViewportChange, true);
    };
  }, [close, open, place]);

  const fields = FIELD_ORDER.filter((field) => present(props.record[field]));
  const dialog = open ? (
    <div
      ref={popoverRef}
      id={id}
      role="dialog"
      aria-label={t("provenance.title")}
      data-testid="provenance-popover"
      style={{
        ...overlay,
        position: "fixed",
        zIndex: 80,
        left: `${position.left}px`,
        top: `${position.top}px`,
        width: "min(420px, calc(100vw - 16px))",
        maxHeight: "min(520px, calc(100vh - 16px))",
        overflowY: "auto",
        padding: "10px 12px",
        color: "var(--fg)",
        fontFamily: "var(--ui-font)",
        fontSize: "var(--text-sm)",
      }}
    >
      <div
        style={{
          marginBlockEnd: "8px",
          color: "var(--fg-strong)",
          fontWeight: 600,
        }}
      >
        {t("provenance.title")}
      </div>
      <dl
        style={{
          display: "grid",
          gridTemplateColumns: "minmax(96px, auto) minmax(0, 1fr)",
          gap: "var(--space-1) var(--space-3)",
          margin: 0,
        }}
      >
        {fields.map((field) => {
          const value = props.record[field];
          const formula = field === "formula";
          return (
            <div
              key={field}
              data-testid="provenance-row"
              data-field={field}
              style={{ display: "contents" }}
            >
              <dt style={{ color: "var(--fg-dim)" }}>
                {t(`provenance.field.${field}`)}
              </dt>
              <dd
                data-testid={formula ? "provenance-formula" : undefined}
                style={{
                  minWidth: 0,
                  margin: 0,
                  fontFamily:
                    formula || field === "window" || field === "snapshot"
                      ? "var(--mono-font)"
                      : undefined,
                  overflowWrap: formula ? "anywhere" : "break-word",
                  whiteSpace: "normal",
                }}
              >
                {field === "state" ? (
                  <SemanticBadge
                    state={value as DataState}
                    reason={
                      typeof props.record.reason === "string"
                        ? props.record.reason
                        : undefined
                    }
                  />
                ) : (
                  value
                )}
              </dd>
            </div>
          );
        })}
      </dl>
    </div>
  ) : null;

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        aria-label={props.triggerLabel}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          event.stopPropagation();
          setOpen((value) => !value);
        }}
        style={{
          display: "inline-flex",
          alignItems: "center",
          minWidth: 0,
          height: "20px",
          padding: "1px 6px",
          color: "var(--fg-dim)",
          background: "transparent",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-sm)",
          fontFamily: "var(--ui-font)",
          fontSize: "var(--text-xs)",
          lineHeight: 1,
          cursor: "pointer",
          whiteSpace: "nowrap",
        }}
      >
        {props.renderTrigger?.({ open }) ?? t("provenance.trigger")}
      </button>
      {dialog !== null ? createPortal(dialog, document.body) : null}
    </>
  );
}
