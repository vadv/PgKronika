# PR204 reference-faithful forensic chrome

**Status:** approved for autonomous implementation
**Visual source of truth:** <https://pgkronika-forensic-u.superdesign.cloud/>
**Baseline viewport:** 1920×1080, DPR 1, 100% zoom

## Goal

Keep PR204's useful density improvements while removing decorative treatments that diverge from the approved forensic-console reference. The shell remains compact, ruled, and information-first: the health score is readable without dominating the combined PostgreSQL+OS timeline; group structure is visible without badges; global controls are compact rectangles rather than consumer-style pills.

## Approved visual decisions

- Keep the larger section title introduced in `PageHeader`.
- Keep the 16 px Activity-to-OS relationship glyph.
- Keep thin separators between primary-navigation groups.
- Render the Health score as one compact `N/100` reading; use semantic color, but do not enlarge it into a hero metric.
- Render navigation ordinals as calm inline mono numerals next to the group label. No circular container, fill, or badge border.
- Render header chips and buttons with `var(--radius-sm)` and dense on-grid padding. No fully rounded pills.
- Use only existing design tokens and the 4 px spacing grid.

## Product semantics

- The Health line continues to combine PostgreSQL and OS evidence on one shared timeline.
- Relationship affordances link useful observations; they do not claim proof or confidence.
- Missing observations remain calm and local. No new gap/gated/provenance language is introduced.
- URL state, keyboard navigation, API behavior, and timeline interaction stay unchanged.

## Acceptance

- `spine-score` exposes a compact visible `N/100` value when data is available and an honest dash when unavailable.
- Primary navigation retains group separators while ordinal labels have no circular presentation.
- Shared header chips and buttons use `var(--radius-sm)` and grid-aligned padding.
- The design-token gate reports zero violations.
- The shell has no root overflow at 1920×1080 or 1440×900.
- Final screenshots are compared side-by-side with the approved reference at the same viewport.
