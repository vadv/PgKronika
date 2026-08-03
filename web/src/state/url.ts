export type DockKind = "incidents" | "row";

export interface UiState {
  view: string;
  /** Cursor timestamp (int64 µs, decimal string); null = LIVE. */
  at: string | null;
  /** Window length in integer seconds (1..86400); prepared controls are SPANS. */
  span: number;
  /** Baseline cursor for the heatmap Δ-mode (int64 µs string). */
  baseline: string | null;
  /** Active column preset code of the view. */
  preset: string | null;
  /** Server-side filter query (transient: never serialized into the hash). */
  q: string | null;
  /** Sort column code and direction (server-side). */
  sort: string | null;
  order: "asc" | "desc" | null;
  /** Focused incident key (from /v1/incidents). */
  focus: string | null;
  /** Open dock panel. */
  dock: DockKind | null;
  /** Selected row entity id (row dock). */
  entity: string | null;
}

export const DEFAULT_SPAN = 3600;
export const SPANS = [900, 3600, 21600, 86400] as const;
export const MIN_SPAN = 1;
export const MAX_SPAN = 86_400;

/** Signed decimal int64 — the only accepted wire form for µs timestamps. */
const DECIMAL_US = /^-?\d+$/;
const DECIMAL_SECONDS = /^\d+$/;
export const INT64_MIN = -(1n << 63n);
export const INT64_MAX = (1n << 63n) - 1n;

export function isTimestampUs(raw: string): boolean {
  // A signed int64 needs at most 20 characters including the minus sign.
  // Reject before BigInt parsing so a hostile hash cannot force work linear
  // in an otherwise unbounded decimal string.
  if (raw.length === 0 || raw.length > 20) return false;
  if (!DECIMAL_US.test(raw)) return false;
  const timestamp = BigInt(raw);
  return timestamp >= INT64_MIN && timestamp <= INT64_MAX;
}

export function isValidSpan(span: number): boolean {
  return Number.isInteger(span) && span >= MIN_SPAN && span <= MAX_SPAN;
}

/** Timestamp from the hash, or null when absent/invalid — an invalid value
 * must fall back to the live default, never reach `BigInt()` in render. */
function parseTimestampUs(raw: string | null): string | null {
  return raw !== null && isTimestampUs(raw) ? raw : null;
}

function parseSpan(raw: string | null): number {
  if (raw === null || !DECIMAL_SECONDS.test(raw)) return DEFAULT_SPAN;
  const span = Number(raw);
  return isValidSpan(span) ? span : DEFAULT_SPAN;
}

export function parseHash(hash: string): UiState {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  const order = params.get("order");
  const dock = params.get("dock");
  return {
    view: params.get("view") ?? "activity",
    at: parseTimestampUs(params.get("at")),
    span: parseSpan(params.get("span")),
    baseline: parseTimestampUs(params.get("baseline")),
    preset: params.get("preset"),
    // `q` is transient: never parsed from a foreign link, never serialized.
    q: null,
    sort: params.get("sort"),
    order: order === "asc" || order === "desc" ? order : null,
    focus: params.get("focus"),
    dock: dock === "incidents" || dock === "row" ? dock : null,
    entity: params.get("entity"),
  };
}

export function toHash(state: UiState): string {
  const params = new URLSearchParams();
  params.set("view", state.view);
  if (state.at !== null && isTimestampUs(state.at)) params.set("at", state.at);
  if (state.span !== DEFAULT_SPAN && isValidSpan(state.span)) {
    params.set("span", String(state.span));
  }
  if (state.baseline !== null && isTimestampUs(state.baseline)) {
    params.set("baseline", state.baseline);
  }
  if (state.preset !== null) params.set("preset", state.preset);
  // `q` is transient on purpose: share URLs must not carry a free-text
  // filter, so it is neither parsed from the hash nor written back.
  if (state.sort !== null) params.set("sort", state.sort);
  if (state.order !== null) params.set("order", state.order);
  if (state.focus !== null) params.set("focus", state.focus);
  if (state.dock !== null) params.set("dock", state.dock);
  if (state.entity !== null) params.set("entity", state.entity);
  return `#${params.toString()}`;
}
