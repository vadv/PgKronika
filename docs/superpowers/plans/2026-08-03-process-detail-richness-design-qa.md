# Process Detail visual QA

## Target and states

- Published reference: <https://pgkronika-forensic-u.superdesign.cloud/refined-entity-detail-process-18422>
- Local deterministic state: `/#view=processes&dock=row&entity=proc%3A12041`
- Product baseline: 1920×1080, DPR 1, 100% zoom.
- Same-browser comparison: reference and local state were captured together at the in-app Browser's 1280×720 viewport.
- Baseline verification: the deterministic shell captured the local state at 1920×1080 and asserted an exact 1920×1080 root with no root-page scroll.

## Visible comparison

The published composition was used as the source of truth for the compact identity band and the three-column evidence surface. The implementation now keeps these visible in one viewport:

- PID, process type, state, and cgroup in a four-part identity band without clipping;
- CPU and scheduler evidence followed by Memory/VMM in the left column;
- logical reads, an explicitly approximate cache-served estimate, physical reads, writes, and syscall rates in the middle column;
- the related Activity query/session at the same retained snapshot plus process lifetime, ownership, and command in the right column;
- compact `S`, `G`, `R`, and `EST` badges with accessible explanations;
- real source labels for `/proc/[pid]/stat`, `/proc/[pid]/status`, and `/proc/[pid]/io`;
- the combined PostgreSQL + OS Health line and global investigation context above the detail workspace.

The reference's `EXACT MATCH` language was intentionally not copied. PgKronika exposes the useful Activity ↔ process path from the server's related-entity response and lets the operator continue into either history; normal UI does not display confidence, proof, gap, gating, or opaque provenance payloads.

## Evidence

- Local 1920×1080 screenshot: `web/demo/shots/forensic-process-detail-1920x1080.png`
- Shell result: `forensic shell PASS`; root `scrollHeight=clientHeight=1080`, `scrollY=0`.
- Dense statement safety remained intact: 1,000 rows loaded, 39 DOM rows virtualized, 96 temporal buckets.
- Frontend gate: 53 test files and 330 tests passed with coverage above the configured thresholds.

## Remaining deliberate differences

The published reference includes 15-second and peak columns that are not present in the point-response contract. The implementation does not synthesize them in the client. Historical samples stay in the History tab, and the process rate fields remain reset-safe backend projections. This preserves the visual hierarchy without inventing production evidence.
