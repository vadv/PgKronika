/** Shared value formatters: one place for units, tokens and timestamps so
 * every surface (table, heatmap, dock, popovers) renders the same way.
 * Locale-sensitive parts go through Intl; unit symbols (µs, KiB) are
 * symbols, not localized text. */

const numberFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});

export function formatNumber(value: number): string {
  return numberFormat.format(value);
}

/** 3 significant digits max, trailing zeros trimmed: 1.07, 12.5, 412. */
function trim(value: number): string {
  const abs = Math.abs(value);
  const rounded =
    abs < 10
      ? Number(value.toFixed(2))
      : abs < 100
        ? Number(value.toFixed(1))
        : Math.round(value);
  return numberFormat.format(rounded);
}

/** Compact high-volume counters for dense matrices; exact values stay in the
 * cell tooltip. Stable technical suffixes avoid locale-dependent width. */
export function formatCompactNumber(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000_000) return `${trim(value / 1_000_000_000_000)}T`;
  if (abs >= 1_000_000_000) return `${trim(value / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${trim(value / 1_000_000)}M`;
  if (abs >= 1_000) return `${trim(value / 1_000)}k`;
  return formatNumber(value);
}

export function formatBytes(value: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let scaled = value;
  let unit = 0;
  while (Math.abs(scaled) >= 1024 && unit < units.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  return `${numberFormat.format(Number(scaled.toFixed(1)))} ${units[unit]}`;
}

/** Microseconds → the unit a human reads fastest: 842 µs, 12.5 ms, 1.07 s.
 * Hours stretch to 48 h ("26 h" beats "1.08 d" for long vacuums and
 * statement runtimes); days take over only past that. */
export function formatDurationUs(value: number): string {
  const abs = Math.abs(value);
  if (abs < 1_000) return `${trim(value)} µs`;
  if (abs < 1_000_000) return `${trim(value / 1_000)} ms`;
  const seconds = value / 1_000_000;
  if (Math.abs(seconds) < 60) return `${trim(seconds)} s`;
  if (Math.abs(seconds) < 3_600) return `${trim(seconds / 60)} m`;
  if (Math.abs(seconds) < 172_800) return `${trim(seconds / 3_600)} h`;
  return `${trim(seconds / 86_400)} d`;
}

/** Unit-aware cell value from the catalog's public unit codes. */
export function formatByUnit(
  value: number,
  unit: string | null | undefined,
): string {
  switch (unit) {
    case "us":
      return formatDurationUs(value);
    case "ms":
      return `${trim(value)} ms`;
    case "duration_ms":
      return formatDurationUs(value * 1_000);
    case "seconds":
      return formatDurationUs(value * 1_000_000);
    case "percent":
      return `${numberFormat.format(value)}%`;
    case "ratio":
      return `${trim(value * 100)}%`;
    case "B":
    case "bytes":
      return formatBytes(value);
    case "kib":
      return formatBytes(value * 1024);
    case "bytes_per_second":
      return `${formatBytes(value)}/s`;
    default:
      return numberFormat.format(value);
  }
}

/** int64 µs (number or decimal string) → localized wall time. Explicit
 * components, not dateStyle: the 2-digit year of `short` is ambiguous in
 * logs and snapshots ("7/31/25"), the full year always shows. */
export function formatTimestampUs(value: number | string): string {
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(Number(value) / 1000));
}

/** Columns whose value is an opaque identity, not a quantity: decimal text
 * with thousand separators reads as a number, so show a short hex token. */
export function isIdentityColumn(code: string): boolean {
  return code === "queryid" || code === "planid";
}

/** Numeric identity columns (PIDs, relation OIDs): rendered as the plain
 * integer operators type, never grouped ("12188", not "12,188"). Guarded
 * by the column type at the call site — text columns named `table` hold
 * names, not OIDs. */
export function isPlainIntegerColumn(code: string): boolean {
  return code === "pid" || code === "root_pid" || code === "table";
}

/** uint64 decimal string → `1a2b…9f0e`; falls back to a plain cut for
 * non-numeric tokens. pg_stat_statements queryid is a uint64 exposed as a
 * signed bigint, so negatives normalize to the unsigned 64-bit form first —
 * otherwise "-1999008…" and a hex cut of the same id read as two different
 * identifiers. The full value always stays available in a tooltip. */
export function shortIdToken(raw: string): string {
  const text = raw.trim();
  if (/^-?[0-9]+$/.test(text)) {
    const hex = BigInt.asUintN(64, BigInt(text)).toString(16);
    return hex.length <= 9 ? hex : `${hex.slice(0, 4)}…${hex.slice(-4)}`;
  }
  return text.length <= 10 ? text : `${text.slice(0, 8)}…`;
}

/** Codes like `pg.log.error_group_observed` must break at segment bounds in
 * narrow tooltips, not mid-word: give the renderer a zero-width break after
 * every dot. */
export function breakableCode(code: string): string {
  return code.replaceAll(".", ".\u200B");
}
