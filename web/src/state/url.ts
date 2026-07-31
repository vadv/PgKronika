export type DockKind = "incidents" | "row";

export interface UiState {
  view: string;
  /** Cursor timestamp (int64 µs, decimal string); null = LIVE. */
  at: string | null;
  /** Window length in seconds (900 / 3600 / 21600 / 86400). */
  span: number;
  /** Baseline cursor for the heatmap Δ-mode (int64 µs string). */
  baseline: string | null;
  /** Active column preset code of the view. */
  preset: string | null;
  /** Server-side filter query. */
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

export function parseHash(hash: string): UiState {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  const span = Number(params.get("span"));
  const order = params.get("order");
  const dock = params.get("dock");
  return {
    view: params.get("view") ?? "activity",
    at: params.get("at"),
    span: SPANS.includes(span as (typeof SPANS)[number]) ? span : DEFAULT_SPAN,
    baseline: params.get("baseline"),
    preset: params.get("preset"),
    q: params.get("q"),
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
  if (state.at !== null) params.set("at", state.at);
  if (state.span !== DEFAULT_SPAN) params.set("span", String(state.span));
  if (state.baseline !== null) params.set("baseline", state.baseline);
  if (state.preset !== null) params.set("preset", state.preset);
  if (state.q !== null) params.set("q", state.q);
  if (state.sort !== null) params.set("sort", state.sort);
  if (state.order !== null) params.set("order", state.order);
  if (state.focus !== null) params.set("focus", state.focus);
  if (state.dock !== null) params.set("dock", state.dock);
  if (state.entity !== null) params.set("entity", state.entity);
  return `#${params.toString()}`;
}
