# PR13 operator-first design QA

## Target

- Baseline viewport: 1920×1080.
- Product rule: PgKronika links retained PostgreSQL and operating-system evidence for investigation. It does not present relation visibility as a proof score.
- Missing snapshots remain visible as local absence and do not suppress discovered relations.

## Visual comparison

The implementation was compared at the same viewport against the approved direction:

- Health reference: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-simplified-healthline.png`
- Signals reference: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-signals-synchronized-evidence.png`
- Detail reference: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-entity-process-detail-refined.png`
- Search reference: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-global-search-refined.png`

Real-data captures from the PR13 build:

- OS: `output/playwright/pr13-os-disk-io-final-1920.png`
- Events and Signals: `output/playwright/pr13-events-signals-24h-final-1920.png`
- Activity: `output/playwright/pr13-activity-overview-final-1920.png`
- Statements: `output/playwright/pr13-statements-latency-final-1920.png`
- Plans: `output/playwright/pr13-plans-final-1920.png`
- Tables: `output/playwright/pr13-tables-final-1920.png`
- Indexes: `output/playwright/pr13-indexes-final-1920.png`
- Process detail: `output/playwright/pr13-process-detail-final-1920.png`
- Process relations: `output/playwright/pr13-process-relations-final-1920.png`
- Search: `output/playwright/pr13-search-final-1920.png`

## Checklist

- [x] The 60 px Health line remains the shared PostgreSQL + OS orientation layer.
- [x] OS, Activity, Statements, and Plans use dense 96-bucket evidence matrices.
- [x] Signals aligns event occurrences, PostgreSQL evidence, OS evidence, and related entities on one time geometry.
- [x] Process detail and global search use human labels and formatted values before raw identifiers.
- [x] Same-PID process/activity records are linked inclusively; process start time only protects rate calculations.
- [x] Tables, Indexes, Vacuum, and Plans expose linked context without normal-screen provenance codes.
- [x] Ordinary missing samples do not produce amber global warnings or `partial`, `gaps`, `gated`, and provenance chrome.
- [x] Opaque entity tokens remain outside normal tooltips and detail summaries.
- [x] Fresh OS and 24-hour Events reloads completed with zero browser console errors and warnings from the client.
- [x] The 24-hour Events view keeps every incident request within the backend's 24-hour contract.
- [x] Impeccable visual detector result: `[]`.

The remote master API briefly returned a recovered `503` while opening one process detail during QA. The same view rendered successfully in another pass, and the new reverse relation behavior is covered by backend integration tests.

## Automated verification

- Frontend: format, lint, typecheck, 328 tests, coverage, and production build passed.
- Backend: formatting and 737 `pg_kronika-web` tests passed.

final result: passed
