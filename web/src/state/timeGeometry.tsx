import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  isTimestampUs,
  isValidSpan,
  parseHash,
  SPANS,
  toHash,
  type UiState,
} from "./url";

export interface TimeRange {
  fromUs: string;
  toUs: string;
}

export interface TimeGeometryValue {
  state: UiState;
  /** Canonical selected window shared by every query and visual. */
  range: TimeRange;
  /** Canonical range end: replay cursor or the provider's pinned LIVE tick. */
  cursorUs: string;
  isLive: boolean;
  preparedSpans: typeof SPANS;
  hoverUs: string | null;
  brushDraft: TimeRange | null;
  patchUiState: (patch: Partial<UiState>) => void;
  setCursor: (cursorUs: string) => void;
  setSpan: (spanSeconds: number) => void;
  commitRange: (range: TimeRange) => void;
  setHover: (hoverUs: string | null) => void;
  setBrushDraft: (range: TimeRange | null) => void;
  setBaseline: (baselineUs: string | null) => void;
  toggleLive: () => void;
}

export interface TimeGeometryProviderProps {
  children: ReactNode;
}

const LIVE_TICK_MS = 15_000;
const US_PER_SECOND = 1_000_000n;
const MAX_RANGE_US = 86_400n * US_PER_SECOND;

const TimeGeometryContext = createContext<TimeGeometryValue | null>(null);

function nowUs(): string {
  return (BigInt(Date.now()) * 1_000n).toString();
}

function normalizeDraft(range: TimeRange): TimeRange | null {
  if (!isTimestampUs(range.fromUs) || !isTimestampUs(range.toUs)) return null;
  const from = BigInt(range.fromUs);
  const to = BigInt(range.toUs);
  return from <= to
    ? { fromUs: from.toString(), toUs: to.toString() }
    : { fromUs: to.toString(), toUs: from.toString() };
}

function sanitizePatch(previous: UiState, patch: Partial<UiState>): UiState {
  const merged = { ...previous, ...patch };
  return {
    ...merged,
    at:
      merged.at === null || isTimestampUs(merged.at) ? merged.at : previous.at,
    span: isValidSpan(merged.span) ? merged.span : previous.span,
    baseline:
      merged.baseline === null || isTimestampUs(merged.baseline)
        ? merged.baseline
        : previous.baseline,
  };
}

export function TimeGeometryProvider({ children }: TimeGeometryProviderProps) {
  const [state, setState] = useState(() => parseHash(location.hash));
  const stateRef = useRef(state);
  const [liveTickUs, setLiveTickUs] = useState(nowUs);
  const liveTickRef = useRef(liveTickUs);
  const [hoverUs, setHoverState] = useState<string | null>(null);
  const [brushDraft, setBrushDraftState] = useState<TimeRange | null>(null);

  stateRef.current = state;
  liveTickRef.current = liveTickUs;

  const replaceState = useCallback((next: UiState) => {
    stateRef.current = next;
    setState(next);
    const hash = toHash(next);
    if (location.hash !== hash) location.hash = hash;
  }, []);

  const patchUiState = useCallback(
    (patch: Partial<UiState>) => {
      const next = sanitizePatch(stateRef.current, patch);
      replaceState(next);
    },
    [replaceState],
  );

  useEffect(() => {
    const onHashChange = () => {
      if (location.hash === toHash(stateRef.current)) return;
      const next = parseHash(location.hash);
      stateRef.current = next;
      setState(next);
      setHoverState(null);
      setBrushDraftState(null);
    };
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  useEffect(() => {
    if (state.at !== null) return;
    const id = window.setInterval(() => {
      const next = nowUs();
      liveTickRef.current = next;
      setLiveTickUs(next);
    }, LIVE_TICK_MS);
    return () => window.clearInterval(id);
  }, [state.at]);

  const setCursor = useCallback(
    (cursorUs: string) => {
      if (!isTimestampUs(cursorUs)) return;
      setBrushDraftState(null);
      patchUiState({ at: cursorUs });
    },
    [patchUiState],
  );

  const setSpan = useCallback(
    (spanSeconds: number) => {
      if (!isValidSpan(spanSeconds)) return;
      patchUiState({ span: spanSeconds });
    },
    [patchUiState],
  );

  const commitRange = useCallback(
    (draft: TimeRange) => {
      const normalized = normalizeDraft(draft);
      if (normalized === null) return;
      const from = BigInt(normalized.fromUs);
      const to = BigInt(normalized.toUs);
      const durationUs = to - from;
      if (durationUs > MAX_RANGE_US) return;
      const roundedSeconds =
        durationUs === 0n
          ? 1n
          : (durationUs + US_PER_SECOND - 1n) / US_PER_SECOND;
      const span = Number(roundedSeconds);
      if (!isValidSpan(span)) return;
      setBrushDraftState(null);
      patchUiState({ at: normalized.toUs, span });
    },
    [patchUiState],
  );

  const setHover = useCallback((nextHoverUs: string | null) => {
    if (nextHoverUs !== null && !isTimestampUs(nextHoverUs)) return;
    setHoverState(nextHoverUs);
  }, []);

  const setBrushDraft = useCallback((draft: TimeRange | null) => {
    if (draft === null) {
      setBrushDraftState(null);
      return;
    }
    const normalized = normalizeDraft(draft);
    if (normalized !== null) setBrushDraftState(normalized);
  }, []);

  const setBaseline = useCallback(
    (baselineUs: string | null) => {
      if (baselineUs !== null && !isTimestampUs(baselineUs)) return;
      patchUiState({ baseline: baselineUs });
    },
    [patchUiState],
  );

  const toggleLive = useCallback(() => {
    if (stateRef.current.at === null) {
      patchUiState({ at: liveTickRef.current });
      return;
    }
    const nextLiveTick = nowUs();
    liveTickRef.current = nextLiveTick;
    setLiveTickUs(nextLiveTick);
    patchUiState({ at: null });
  }, [patchUiState]);

  const cursorUs = state.at ?? liveTickUs;
  const range = useMemo<TimeRange>(() => {
    const to = BigInt(cursorUs);
    const from = to - BigInt(state.span) * US_PER_SECOND;
    return { fromUs: from.toString(), toUs: to.toString() };
  }, [cursorUs, state.span]);

  const value = useMemo<TimeGeometryValue>(
    () => ({
      state,
      range,
      cursorUs,
      isLive: state.at === null,
      preparedSpans: SPANS,
      hoverUs,
      brushDraft,
      patchUiState,
      setCursor,
      setSpan,
      commitRange,
      setHover,
      setBrushDraft,
      setBaseline,
      toggleLive,
    }),
    [
      brushDraft,
      commitRange,
      cursorUs,
      hoverUs,
      patchUiState,
      range,
      setBaseline,
      setBrushDraft,
      setCursor,
      setHover,
      setSpan,
      state,
      toggleLive,
    ],
  );

  return (
    <TimeGeometryContext.Provider value={value}>
      {children}
    </TimeGeometryContext.Provider>
  );
}

export function useTimeGeometry(): TimeGeometryValue {
  const value = useContext(TimeGeometryContext);
  if (value === null) {
    throw new Error("useTimeGeometry must be used within TimeGeometryProvider");
  }
  return value;
}
