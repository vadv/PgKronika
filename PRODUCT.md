# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

PgKronika is for DevOps engineers, DBAs, and PostgreSQL developers investigating what happened to a PostgreSQL instance and its operating system. The primary situation is forensic replay: an operator sits down after or during an incident and needs enough trustworthy evidence in one view to form and test hypotheses quickly.

## Product Purpose

PgKronika records PostgreSQL and Linux evidence and presents it as a time-bounded investigation workspace. Success means the operator can move from a health anomaly to the responsible workload, process, plan, relation, maintenance action, or event without confusing temporal coincidence with proven causality.

## Positioning

The product combines PostgreSQL and operating-system evidence while preserving provenance, collection quality, snapshot/range semantics, and conservative entity relations. It does not flatten heterogeneous sources into an unjustified correlation score: the Health Line and temporal matrices make relationships visible while leaving causal judgment to the operator.

## Operating Context

- The baseline workstation viewport is 1920 × 1080; all investigation-critical information must be available without root-page scrolling.
- Operators investigate through prepared analytical lenses for OS, Activity, Statements, Plans, Tables, Indexes, Vacuum, and Events.
- Statements is the primary reference screen, followed in importance by Activity and Plans.
- A typical Statements population is about 1,000 `pg_stat_statements` entities, so server paging, virtualization, dense scanning, search, and drill-down are core workflow requirements.
- Activity is point-in-time evidence and may be conservatively related to an operating-system process using PID plus process lifetime evidence. Very short queries can fall between samples.

## Capabilities and Constraints

- One persistent Health Line combines PostgreSQL and OS evidence across the selected window.
- Heatmaps are first-class evidence because operators use their shape to see possible temporal relationships; statistical correlation may be unavailable or misleading.
- Temporal evidence must distinguish zero from missing/not-retained data.
- Screens combine multiple sources behind prepared lenses rather than exposing one source per tab.
- Search must support keyboard-first investigation and open Entity Detail.
- Entity Detail is a reusable pattern for Statements, Activity/processes, Tables, Indexes, and related entities; full SQL belongs there when row payloads must stay bounded.
- Activity/process relations must reject ambiguous PID reuse and must state snapshot/lifetime semantics honestly.
- The interface must not claim causality when it only has coincidence, partial coverage, or a point observation.
- English and Russian UI text are both maintained.

## Brand Commitments

The product name is PgKronika. Its voice is concise, technical, and explicit about uncertainty. The interface should feel modern and lightweight while remaining dense enough for expert incident investigation; decoration must not displace evidence.

## Evidence on Hand

- Approved Statements visual target: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-statements-overview.png`.
- Implemented browser captures live under `web/demo/shots/`.
- The repository's API catalog, provenance responses, data-quality responses, tests, and demo fixtures are the factual source for supported lenses and relations.
- No customer claims, testimonials, or external benchmarks are approved; future surfaces must not fabricate them.

## Product Principles

1. Show evidence and its quality together.
2. Preserve time semantics before suggesting relationships.
3. Keep the first viewport dense, bounded, and operational.
4. Make expert depth discoverable through search, lenses, tooltips, and Entity Detail.
5. Prefer conservative “unknown” or “not retained” states over false precision.

## Accessibility & Inclusion

Core investigation paths must be keyboard reachable, state must not depend on color alone, missing evidence must remain semantically distinct from zero, and reduced-motion preferences must be respected. Dense desktop layouts must remain usable at the supported compact viewport without hiding persistent controls.
