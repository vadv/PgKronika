# PR14 Superdesign fidelity QA

## Target

- Reference: `https://pgkronika-forensic-u.superdesign.cloud/`.
- Baseline viewport: 1920×1080 with no document scroll and no clipped primary evidence.
- Product rule: PgKronika links retained PostgreSQL and Linux observations for investigation; normal screens do not grade or prove those relations.
- Health Line is the only persistent PostgreSQL + OS time overview. Activity overview is a dense joined snapshot; prepared analytical lenses retain their 96-bucket heatmaps.

## Compared surfaces

Fresh same-viewport captures were compared directly with the published reference for:

- Activity overview;
- Activity/process entity detail;
- Health Line;
- Events timeline and Signals.

The deterministic shell verifier also captured Statements, Activity CPU, Activity waits, Plans, OS, Tables, Indexes, Vacuum, Events at 1920×1080, plus Statements and Events at 1440×900.

## Fidelity and behavior

- [x] Layout: header, grouped navigation, 60 px Health Line, context strip, analytical workspace, and 24 px footer fill exactly 1920×1080.
- [x] Density: Activity overview renders a joined PG/PID/OS snapshot; Statements loads 1,000 rows and virtualizes the visible 27 px rows.
- [x] Detail: desktop entity detail is a full-width investigation workspace below Health Line with identity/state strips and reusable grouped measurements.
- [x] Health: a continuous severity trace replaces the block ribbon; event ticks, OS load trace, selection, hover, missing intervals, and keyboard behavior remain.
- [x] Events: the retained 96-bucket heatmap reads as sparse event lanes; five compact Signals fit without clipping while range rows and event families remain visible.
- [x] Typography and color: Inter/JetBrains Mono hierarchy, compact 9–12 px scale, restrained blue focus, and semantic warn/critical colors match the reference system.
- [x] Surfaces: full-bleed analytical regions use one-pixel dividers and 4 px radii with no decorative shadows on the desktop detail workspace.
- [x] Copy: opaque tokens and collection internals stay in explicit Raw/diagnostic surfaces; normal detail, links, OS context, and Events remain human-readable.
- [x] Accessibility: semantic buttons/tabs, keyboard row navigation, focus trap, skip link, reduced motion, labels, and local missing-cell titles remain covered.
- [x] Responsiveness: no root overflow or clipped Signals at 1920×1080 and 1440×900; the existing mobile bottom-sheet behavior is preserved.

## Resolved findings

- P1 — Activity was a detached temporal matrix instead of the approved joined snapshot. Replaced the default lens with grouped PostgreSQL, PID-link, and OS columns; heatmaps remain in analytical lenses.
- P1 — Entity detail was a narrow drawer. Converted desktop row detail to a full analytical workspace and grouped measurements by compute, I/O/cache, and query/session context.
- P1 — Health read as a dense block ribbon. Replaced it with a continuous severity line while preserving its synchronized events and OS signal.
- P2 — Events heatmap read as a solid blue carpet. Quiet cells are now subdued and event peaks remain visually prominent.
- P2 — The last Signals row clipped in the 176 px overview. Limited the bounded rail to five dense 24 px rows and verified both desktop sizes.
- P2 — Shell QA still required internal provenance and collection-quality jargon. Updated the contract to verify human links and keep internals out of ordinary UI.
- P1 review — Detail grouping suppressed unavailable catalog fields together with ordinary nulls. Ordinary nulls remain quiet; unavailable fields now stay visible with a calm `not collected / нет данных` value.
- P2 review — `Linked by PID` repeated across every Activity row. The dense relation cell now says only `Linked / Связано`; the shared PID remains an explanatory tooltip and Relationships keeps a human `Linked process` target.

## Automated verification

- Frontend: 53 files / 329 tests passed with 93.31% statement coverage.
- Typecheck, ESLint, Prettier, production build, and deterministic shell verification passed.
- Shell verifier exercised 1,000 Statements, 96-bucket analytical matrices, full-width detail, all infrastructure screens, Events, keyboard paths, and 1920×1080 root geometry.

final result: passed
