# PR4: Statements forensic workspace — implementation plan

**Goal:** turn the Statements screen into a dense, honest workspace for roughly 1,000 `pg_stat_statements` identities without rendering the full result set or implying evidence the collector does not have.

**Stack base:** `codex/pr03-forensic-shell`

## Evidence boundary

- `Workload`, `Latency`, `Buffers`, `WAL`, `Temp`, and `Planning` use existing reset-aware statement deltas.
- `Regression` remains prepared but unavailable until comparable baseline deltas exist in the frame contract.
- `Observed samples` remains prepared but unavailable until activity/process samples have a proven per-statement relation.
- Query text is deliberately not collected; the screen and row detail must say so explicitly.
- PostgreSQL buffer reads are not labelled as physical storage I/O, and no statement CPU value is invented.

## Tasks

### 1. Add honest statement lens contracts

- Add catalog presets for `latency` and `planning`, using only existing projected columns.
- Add contract tests for all six executable statement lenses and their ordering.
- Keep unavailable future lenses in a frontend statement-lens model with a reason, not in the executable catalog.

### 2. Build the dense Statements control strip

- Extend the shared toolbar with optional prepared lens descriptors.
- Render eight statement lenses: six executable and two disabled with accessible explanations.
- Expose cumulative/reset-aware semantics and query-text availability in a compact evidence note.
- Preserve URL-backed preset and search state.

### 3. Virtualize the ranked matrix

- Use fixed 28 px rows, measured viewport height, and a small overscan window.
- Keep the header and identity column sticky.
- Retain server cursor pagination; accumulated pages may grow only to the server-reported match bound, while DOM row count stays bounded.
- Reset scroll geometry when the frame intent changes.
- Keep keyboard selection and selected-row visibility correct.

### 4. Preserve the table on narrow screens

- Make the statements matrix horizontally scrollable instead of replacing it with a summary-only card.
- Retain the compact control strip and explicit unavailable evidence states.

### 5. Verify the 1k scenario

- Add unit tests that fail before virtualization and prove the DOM window moves on scroll.
- Add browser verification at 1920×1080 for 1,000 demo statements: bounded DOM rows, at least 16 fully visible evidence rows, pagination visibility, responsive fallback, and bounded search interaction latency.
- Run Rust catalog tests, frontend gates, shell verification, and the new statements verification before review.

