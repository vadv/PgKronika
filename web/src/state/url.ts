export interface UiState {
  view: string;
  at: string | null;
}

export function parseHash(hash: string): UiState {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  return {
    view: params.get("view") ?? "activity",
    at: params.get("at"),
  };
}

export function toHash(state: UiState): string {
  const params = new URLSearchParams();
  params.set("view", state.view);
  if (state.at !== null) params.set("at", state.at);
  return `#${params.toString()}`;
}
