# PR206: Statements forensic detail fidelity

## Outcome

Selecting one `pg_stat_statements` row turns the analytical canvas below the
persistent Health Line into a statement-specific investigation workspace. The
operator keeps the selected range and can see impact, latency, buffer pressure,
WAL/temp work, SQL, history and related plans in one 1920×1080 frame.

The ranked 1,000-row Statements matrix and its 96-bucket heatmap remain the
overview. Detail replaces it only while a row is selected; closing detail
returns to the same matrix, lens, filter and cursor.

## Composition

1. A 40px entity strip shows the human context: Statements, database, role,
   query ID, snapshot time, collection state and the strongest impact signal.
   The opaque routing token never appears.
2. Four aligned temporal lanes share one time axis:
   - impact — total execution time and calls;
   - latency — mean time and milliseconds per row;
   - buffers — PostgreSQL buffer reads and hit ratio;
   - write pressure — WAL bytes and temporary blocks.
3. A compact related-workload lane exposes recorded plans and any future
   activity/process links without displaying attribution internals.
4. The lower grid contains:
   - the calls × mean = total impact semantic center and bounded SQL text;
   - a current / window delta / baseline metric matrix;
   - related evidence cards plus calm empty-state copy when no related samples
     were collected.

## Interaction and language

- The entire row and all related cards are keyboard-operable.
- Tooltips and column labels retain exact PostgreSQL units. Buffer reads are
  never called physical disk I/O, and no statement CPU value is invented.
- Missing values read as “not collected”; they are not rendered as zero.
- Collection discontinuities may affect the calm collection-state chip, but
  `gaps`, `gated`, `proof`, provenance methods and endpoint/token strings are
  absent from the primary workspace.
- Related entities are investigation links. Their cards say what was observed,
  not that the UI proved causality.

## Bounds

- History range: at most six hours.
- History metrics: at most six requested columns.
- History response: at most 96 samples.
- Root page: no vertical scrolling at 1920×1080.
- Detail must not mount the generic row dock. The 1,000-row overview stays
  mounted but hidden so pagination and scroll position survive closing detail;
  it must not paint, receive focus or participate in layout behind detail.

## Verification

- Component contract tests cover the four lanes, semantic center, matrix,
  related navigation, missing values and bounded history request.
- App routing test proves inline canvas ownership and close behavior.
- The shell verifier covers a real demo statement at exactly 1920×1080 and
  asserts Health Line visibility, no generic dock, no root scroll and no opaque
  token text.
