import type { ViewSpec } from "../api/types";

const MAX_QUERY_BYTES = 256;
const MAX_TERMS = 16;

type SearchKey =
  | "pid"
  | "queryid"
  | "planid"
  | "relation"
  | "index"
  | "oid"
  | "wait"
  | "event"
  | "severity"
  | "database"
  | "user"
  | "application"
  | "cgroup"
  | "device";

interface SearchTerm {
  key: SearchKey | null;
  value: string;
}

export interface ForensicSearchGroup {
  view: ViewSpec;
  /** Exact q sent to this view's bounded server frame. */
  q: string;
  /** Human-readable reason derived from admitted structured terms. */
  reason: string;
}

export type ForensicSearchError =
  | { code: "invalid_query" }
  | { code: "query_too_long"; limit: number }
  | { code: "too_many_terms"; limit: number }
  | { code: "unsupported_key"; key: string };

export interface ForensicSearchPlan {
  groups: ForensicSearchGroup[];
  error: ForensicSearchError | null;
}

/** Palette aliases over public, non-lazy frame columns. Empty mappings are
 * prepared search concepts whose evidence is not projected yet. */
const SEARCH_COLUMNS: Record<SearchKey, Record<string, string>> = {
  pid: {
    activity: "pid",
    processes: "pid",
    locks: "pid",
    vacuum: "pid",
  },
  queryid: { statements: "queryid", plans: "queryid" },
  planid: { plans: "planid" },
  relation: {
    tables: "relation",
    indexes: "table",
    vacuum: "relation",
  },
  index: { indexes: "index" },
  oid: {},
  wait: { activity: "wait_event", locks: "lock" },
  event: { events: "type" },
  severity: { events: "severity" },
  database: { activity: "database", statements: "database" },
  user: { activity: "user", statements: "user" },
  application: { activity: "application" },
  cgroup: { processes: "cgroup" },
  device: {},
};

const KEY_ALIASES: Record<string, SearchKey> = {
  pid: "pid",
  queryid: "queryid",
  planid: "planid",
  rel: "relation",
  relation: "relation",
  index: "index",
  oid: "oid",
  wait: "wait",
  event: "event",
  severity: "severity",
  db: "database",
  database: "database",
  user: "user",
  app: "application",
  application: "application",
  cgroup: "cgroup",
  device: "device",
};

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).length;
}

function tokenize(raw: string): string[] | null {
  const terms: string[] = [];
  let current = "";
  let quoted = false;
  let escaped = false;
  const flush = () => {
    if (current !== "") terms.push(current);
    current = "";
  };
  for (const character of raw.trim()) {
    if (escaped) {
      if (character !== '"' && character !== "\\") return null;
      current += character;
      escaped = false;
    } else if (character === "\\" && quoted) {
      escaped = true;
    } else if (character === '"') {
      quoted = !quoted;
    } else if (/\s/u.test(character) && !quoted) {
      flush();
    } else {
      current += character;
    }
  }
  if (quoted || escaped) return null;
  flush();
  return terms;
}

function parseTerms(raw: string): SearchTerm[] | ForensicSearchError {
  if (utf8Bytes(raw) > MAX_QUERY_BYTES) {
    return { code: "query_too_long", limit: MAX_QUERY_BYTES };
  }
  const tokens = tokenize(raw);
  if (tokens === null) return { code: "invalid_query" };
  if (tokens.length > MAX_TERMS) {
    return { code: "too_many_terms", limit: MAX_TERMS };
  }
  const terms: SearchTerm[] = [];
  for (const token of tokens) {
    const colon = token.indexOf(":");
    if (colon <= 0) {
      if (token === "") return { code: "invalid_query" };
      terms.push({ key: null, value: token });
      continue;
    }
    const rawKey = token.slice(0, colon).toLocaleLowerCase("en-US");
    const value = token.slice(colon + 1);
    if (value === "") return { code: "invalid_query" };
    const key = KEY_ALIASES[rawKey];
    if (key === undefined) return { code: "unsupported_key", key: rawKey };
    terms.push({ key, value });
  }
  return terms;
}

/** Encode one user value in the server frame glob grammar. Wildcards remain
 * intentional. Any char the server reads outside quotes — whitespace (term
 * split), `=` (field split), `"`/`\` (quote grammar) — forces a quoted,
 * escaped literal so the value can never break out of its own term. */
function encodeGlob(value: string, substring: boolean): string {
  const bounded = `${substring ? "*" : ""}${value}${substring ? "*" : ""}`;
  if (!/[\s="\\]/u.test(bounded)) return bounded;
  const escaped = bounded.replaceAll("\\", "\\\\").replaceAll('"', '\\"');
  return `"${escaped}"`;
}

function termReason(term: SearchTerm): string {
  return term.key === null
    ? `materialized fields contain ${term.value}`
    : `${term.key} = ${term.value}`;
}

function searchableColumn(view: ViewSpec, code: string): boolean {
  return view.columns.some(
    (column) =>
      column.code === code &&
      !column.lazy &&
      column.availability === "available",
  );
}

/** Compile palette syntax into one honest frame query per compatible view.
 * This is catalog admission, not a client-side result filter. */
export function compileForensicSearch(
  raw: string,
  views: ViewSpec[],
): ForensicSearchPlan {
  const parsed = parseTerms(raw);
  if (!Array.isArray(parsed)) return { groups: [], error: parsed };
  if (parsed.length === 0) return { groups: [], error: null };

  for (const term of parsed) {
    if (
      term.key !== null &&
      Object.keys(SEARCH_COLUMNS[term.key]).length === 0
    ) {
      return { groups: [], error: { code: "unsupported_key", key: term.key } };
    }
  }

  const groups: ForensicSearchGroup[] = [];
  for (const view of views) {
    if (view.availability !== "available") continue;
    const queryTerms: string[] = [];
    let compatible = true;
    for (const term of parsed) {
      if (term.key === null) {
        queryTerms.push(encodeGlob(term.value, true));
        continue;
      }
      const column = SEARCH_COLUMNS[term.key][view.code];
      if (column === undefined || !searchableColumn(view, column)) {
        compatible = false;
        break;
      }
      queryTerms.push(`${column}=${encodeGlob(term.value, false)}`);
    }
    if (!compatible) continue;
    const q = queryTerms.join(" ");
    if (utf8Bytes(q) > MAX_QUERY_BYTES) {
      return {
        groups: [],
        error: { code: "query_too_long", limit: MAX_QUERY_BYTES },
      };
    }
    groups.push({
      view,
      q,
      reason: parsed.map(termReason).join(" · "),
    });
  }

  if (groups.length === 0) {
    const key = parsed.find((term) => term.key !== null)?.key;
    if (key !== undefined && key !== null) {
      return { groups: [], error: { code: "unsupported_key", key } };
    }
  }
  return { groups, error: null };
}
