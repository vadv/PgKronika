import {
  cloneElement,
  isValidElement,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
} from "react";

/**
 * Hand-rolled hover/focus tooltip: quiet surface, technological inside.
 * Native `title` stays for trivial cases; this is for structured hints
 * (metric meaning, evidence, reasons) that must render fast and token-true.
 * No portals: the tip is positioned inside a relative wrapper, so it
 * composes inside overflow panels; flips above when near the bottom edge
 * and clamps horizontally to the viewport (a dock shrinks the usable width).
 */

const TIP_DELAY_MS = 250;
const VIEWPORT_MARGIN_PX = 8;

export interface TooltipProps {
  /** Structured hint content (keep it short: 1–3 lines + optional mono rows). */
  content: ReactNode;
  children: ReactElement;
  /** Prefer opening above the anchor (e.g. bottom-row tables). */
  preferAbove?: boolean;
}

export function Tooltip(props: TooltipProps) {
  const [open, setOpen] = useState(false);
  const [above, setAbove] = useState(props.preferAbove === true);
  const [shiftX, setShiftX] = useState(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const tipRef = useRef<HTMLSpanElement>(null);
  const id = useId();

  const show = () => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      const rect = wrapRef.current?.getBoundingClientRect();
      if (rect !== undefined && props.preferAbove !== true) {
        // Flip above when there is not enough room below (<= 96px tip).
        setAbove(window.innerHeight - rect.bottom < 110);
      }
      setShiftX(0);
      setOpen(true);
    }, TIP_DELAY_MS);
  };
  const hide = () => {
    if (timer.current !== null) clearTimeout(timer.current);
    setOpen(false);
  };

  // Clamp after render: the tip must never stick out past the viewport edge,
  // no matter how close the anchor sits to it (or how wide a formula is).
  useLayoutEffect(() => {
    if (!open) return;
    const tip = tipRef.current;
    if (tip === null) return;
    const rect = tip.getBoundingClientRect();
    let shift = 0;
    if (rect.right > window.innerWidth - VIEWPORT_MARGIN_PX) {
      shift = window.innerWidth - VIEWPORT_MARGIN_PX - rect.right;
    }
    if (rect.left + shift < VIEWPORT_MARGIN_PX) {
      shift = VIEWPORT_MARGIN_PX - rect.left;
    }
    if (shift !== 0) setShiftX(shift);
  }, [open]);

  const child = isValidElement(props.children)
    ? cloneElement(props.children, {
        "aria-describedby": open ? id : undefined,
        onMouseEnter: show,
        onMouseLeave: hide,
        onFocus: show,
        onBlur: hide,
      } as Record<string, unknown>)
    : props.children;

  const interactive =
    isValidElement(props.children) &&
    (props.children.type === "button" || props.children.type === "a");

  const tipStyle: CSSProperties = {
    position: "absolute",
    zIndex: 30,
    insetInlineStart: 0,
    transform: shiftX === 0 ? undefined : `translateX(${shiftX}px)`,
    ...(above
      ? { bottom: "100%", marginBlockEnd: "6px" }
      : { top: "100%", marginBlockStart: "6px" }),
    maxWidth: "min(340px, 90vw)",
    padding: "6px 8px",
    background: "var(--bg-overlay)",
    border: "1px solid var(--border)",
    borderRadius: "var(--radius-md)",
    boxShadow: "var(--shadow-pop)",
    color: "var(--fg)",
    fontFamily: "var(--ui-font)",
    fontSize: "var(--text-sm)",
    lineHeight: 1.4,
    textAlign: "start",
    whiteSpace: "normal",
    pointerEvents: "none",
  };

  return (
    <span
      ref={wrapRef}
      style={{
        position: "relative",
        display: "inline-flex",
        cursor: interactive ? undefined : "help",
      }}
    >
      {child}
      {open && (
        <span role="tooltip" id={id} ref={tipRef} style={tipStyle}>
          {props.content}
        </span>
      )}
    </span>
  );
}

/** Compact label: value rows used inside structured tooltips. */
export function TipRow(props: {
  label: string;
  value: ReactNode;
  mono?: boolean;
}) {
  return (
    <span
      style={{ display: "flex", gap: "12px", justifyContent: "space-between" }}
    >
      <span style={{ color: "var(--fg-dim)", flex: "none" }}>
        {props.label}
      </span>
      <span
        style={{
          fontFamily: props.mono === true ? "var(--mono-font)" : undefined,
          textAlign: "end",
          maxWidth: "240px",
          overflowWrap: "break-word",
        }}
      >
        {props.value}
      </span>
    </span>
  );
}

/** Full-width mono block for formulas: wraps anywhere instead of sticking
 * out of the box (operators and parens are fine break points). */
export function TipFormula(props: { label: string; value: string }) {
  return (
    <span style={{ display: "grid", gap: "2px" }}>
      <span style={{ color: "var(--fg-dim)" }}>{props.label}</span>
      <span
        style={{
          fontFamily: "var(--mono-font)",
          overflowWrap: "anywhere",
          whiteSpace: "normal",
        }}
      >
        {props.value}
      </span>
    </span>
  );
}
