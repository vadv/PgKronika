export interface UiState {
  source: string;
  view: string;
  at: string | null;
}

export function parseHash(hash: string): UiState {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  return {
    source: params.get("source") ?? "local",
    view: params.get("view") ?? "activity",
    at: params.get("at"),
  };
}

export function toHash(state: UiState): string {
  const params = new URLSearchParams();
  params.set("source", state.source);
  params.set("view", state.view);
  if (state.at !== null) params.set("at", state.at);
  return `#${params.toString()}`;
}
