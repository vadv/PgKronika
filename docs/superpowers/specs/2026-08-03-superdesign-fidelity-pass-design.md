# PgKronika Superdesign fidelity pass

**Status:** approved for autonomous implementation
**Visual source of truth:** <https://pgkronika-forensic-u.superdesign.cloud/>
**Baseline viewport:** 1920×1080, DPR 1, 100% zoom

## 1. Goal

Bring the production PgKronika shell, Activity overview, and reusable Entity Detail materially closer to the published Superdesign reference while preserving the real API contract and the operator semantics agreed after the mockups.

The page must feel like one forensic instrument rather than a stack of bordered widgets. At 1920×1080 the global context, grouped navigation, combined PostgreSQL+OS Health line, active analytical lens, dense evidence surface, and footer context remain visible without root-page scrolling.

## 2. Product semantics that override the reference copy

The reference is authoritative for composition, geometry, density, typography, and visual hierarchy. It is not authoritative for the following wording:

- PgKronika links related observations; it does not “prove” a causal or identity claim.
- A same-PID relationship is shown whenever the retained evidence gives the operator a useful path. Linux PID reuse does not justify hiding a possible relationship.
- `backend_start` and process `starttime` may bound rate continuity, but they do not gate relation visibility.
- Normal UI does not expose `EXACT MATCH`, confidence scores, `gaps`, `gated`, provenance payloads, or opaque entity tokens.
- Missing snapshots are rendered locally and calmly as missing measurements. They do not turn the whole page into an amber “partial data” warning.
- Technical identity, raw quality, source, and routing material remain available only in the explicit Raw tab.

## 3. Shared shell contract

The desktop shell adopts the reference's full-bleed ruled-board composition:

- 44 px global context bar.
- 32 px grouped primary navigation with Workload, Data, Host, and Events.
- 60 px combined Health line immediately below navigation, without outer card margins.
- 24 px footer/status line.
- No root scroll at 1920×1080; each table or detail body owns its overflow.
- Matte surfaces `#0d1117`, `#11161d`, and `#161b22`; 1 px `#30363d` separators; no persistent shadows.
- Inter for chrome and JetBrains Mono for identifiers, timestamps, SQL, and measurements.
- Compact controls use 2–4 px radius, 24–28 px height, and semantic color only for state or interaction.

The header keeps actual instance, role, replication, data availability, incident counts, current time, search, and share. It gains the reference hierarchy and compact selector treatment without fabricating a database selector unsupported by the API.

## 4. Health line

Health remains the only persistent combined PostgreSQL+OS time visualization. It becomes a full-width 60 px band:

- left identity block: `Health · PostgreSQL + OS`, score, readable verdict, and one dominant factor when available;
- right time field: restrained aggregate trace, incident/event marks, cursor, and baseline on one axis;
- local missing intervals remain visible in the trace but do not dominate the header copy;
- interaction semantics and the existing exact shared time geometry remain unchanged.

## 5. Activity overview

The default Activity/Overview lens follows the published snapshot composition. Activity is a point-in-time source, so its default surface is a dense PostgreSQL/backend and Linux/process evidence table, not a misleading continuous-history claim.

### 5.1 Context rows

Immediately below Health:

1. A 36 px screen header with `Activity / Overview` and prepared lenses: Overview, Waits, Statements, CPU, System, Memory, Vacuum.
2. A 32 px snapshot strip with snapshot time, visible backend count, a quiet note that very short queries may fall between samples, and compact state counts.
3. A 32 px filter/sort strip with state filters, `PID / SQL / queryid` search, and current ranking.

These are straight ruled bands, not nested rounded cards.

### 5.2 Default evidence table

The first viewport is dominated by a single virtualized table with three column groups:

- **PostgreSQL backend context:** PID, type/application, database/user, state, wait event, query age, transaction age, queryid.
- **Link:** a narrow relation affordance. It communicates “linked by PID” in tooltip/detail, not proof quality.
- **OS process metrics:** state, CPU, RSS, read/s, write/s, command/threads.

Row height is 34–38 px when identity needs a second line. Numeric cells use tabular monospace. The selected row gets a 2 px blue rail and a subtle blue wash. Missing process data stays `—` with a calm explanation; the row remains useful and clickable.

### 5.3 Temporal Activity lenses

The 96-bucket heatmap is preserved. Waits, CPU, System, Memory, and sampling-oriented lenses may switch the same ranked evidence surface into a temporal matrix. Heatmap availability is not treated as a confidence gate for the point snapshot or PID relationship. The default Overview intentionally prioritizes the joined snapshot table because that is the faithful representation of `pg_stat_activity`.

## 6. Reusable Entity Detail

On desktop, row detail becomes a full-width forensic workspace below the persistent shell and Health line instead of a narrow undifferentiated right drawer. Incident browsing remains a right-side dock.

### 6.1 Detail header

- breadcrumb back to the originating view;
- human identity (`PID 12496`, relation name, queryid, plan id, index name, etc.);
- snapshot time and a neutral state chip;
- compact close/back and copy-link actions;
- no opaque token and no “exact match” claim.

### 6.2 Activity/process detail composition

The summary is grouped into a bounded analytical grid inspired by the published Process Detail:

- identity strip: backend identity, database/user, application/client, cgroup/control when available, current command;
- synchronized mini-lanes: PG state, PG wait, Linux state, and pressure/history where the API has samples;
- left column: CPU/scheduler and memory measurements;
- center column: I/O and cache-path measurements, with explicit formula caveats only when a value is derived;
- right column: active query, wait/session facts, and directly related entities.

Only fields with a value or a meaningful unavailable reason render in Summary. Replication-only null fields do not pollute a normal client-backend detail. History and Relationships remain dedicated tabs. Raw contains the full entity token, endpoint, response, quality, and provenance.

The grouping model is reusable for statements, plans, tables, indexes, vacuum workers, and OS processes: identity first, dominant measurements second, related evidence and next paths third.

## 7. Accessibility and interaction

- Existing keyboard routes, focus rings, deep links, and URL state remain intact.
- Tables keep native semantic headers and rows.
- Link affordances have accessible names describing the destination, not evidence certainty.
- Color is never the only state indicator.
- Reduced-motion mode disables nonessential transitions.
- At 760 px and below, Activity keeps the existing mobile triage behavior and Entity Detail remains a bottom sheet; the desktop full-screen detail contract starts above that breakpoint.

## 8. Data and backend boundary

PR14 consumes the current typed frame/entity/history APIs first. Backend or demo-fixture changes are allowed only when the reference needs an already-collected field that is absent from a public projection. No synthetic production field or client-calculated causal claim is added for visual completeness.

## 9. Acceptance

- At 1920×1080 the root does not scroll and at least 18 Activity rows are visible in the default overview on the deterministic dense fixture.
- The Activity overview visually contains the three evidence groups and a narrow relation column.
- A same-PID Activity row exposes a process link even with missing intervals elsewhere in the window.
- Missing OS fields do not hide the Activity row or relationship.
- A desktop row detail replaces the analytical center below Health and exposes grouped summary, History, Relationships, and Raw.
- Summary excludes irrelevant null-only fields and never exposes an opaque token.
- Raw exposes the complete token and server response.
- The Activity temporal lenses retain a 96-bucket heatmap.
- Reference and implementation screenshots are compared side-by-side at 1920×1080 and recorded in `design-qa.md`.
- Frontend unit, accessibility, type, lint, shell, bundle, and backend suites remain green.
