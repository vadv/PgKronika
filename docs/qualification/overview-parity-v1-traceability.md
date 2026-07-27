# Overview parity-v1 acceptance traceability

Base: `1a6f435ee1f9623b0d9c46cd87b51dd0eba15195` (merged PR #114).

This checklist maps the normative acceptance rows in §20.1 of
`docs/superpowers/specs/2026-07-22-overview-index-timeline-api.md` to the exact
coordinates emitted in the artifact's `acceptance` dossier. The runner and
validator require the same ordered 18-row set. Every row is implemented and
has direct production-path evidence; its decision remains
`PENDING_EXACT_HEAD_CI` until the finalizer binds it to one successful
attempt-1 Actions run at the exact release head. A passing test from another
commit or Actions attempt is not release evidence.

`timeline[15..18]` and `lifecycle[15..18]` below denote the exact BDD
coordinates listed after the table.

| ID | Artifact requirement | Exact direct evidence | State |
| ---: | --- | --- | --- |
| 1 | `restart-warm-zero-pgm` | `restart-warm`; `cold_build_and_cache_hit_report_exact_io_origins`; `lifecycle[15..18]` | IMPLEMENTED |
| 2 | `raw-index-all-families` | `every_populated_canonical_block_matches_forced_raw_and_restart_warm`; `all_family_range_edges_use_half_open_ownership_and_one_left_halo`; `every_all_family_contiguous_partition_reconciles_to_exact_cold_sealed_facts`; `oracle-profile`; `timeline[15..18]` | IMPLEMENTED |
| 3 | `partition-seal-invariance` | `every_all_family_contiguous_partition_reconciles_to_exact_cold_sealed_facts`; `ten_thousand_random_partition_seal_and_merge_seeds_are_invariant` | IMPLEMENTED |
| 4 | `ovf-fault-fallback` | `corrupt_sidecar_is_atomically_replaced_at_the_same_path`; `wrong_source_at_the_expected_name_is_rejected`; `oversized_candidate_is_rebuilt_and_atomically_replaced`; `admission_distinguishes_wrong_source_from_incompatible_versions`; `publication_failure_returns_fresh_facts_then_serves_the_fallback`; `lifecycle[15..18]` | IMPLEMENTED |
| 5 | `source-damage-visible` | `scheduled_source_scrub_prevents_a_durable_fact_from_masking_damage`; `every_all_family_source_body_crc_failure_stays_a_source_error` | IMPLEMENTED |
| 6 | `policy-reuse` | `range-cold/facts-warm`; `policy_versions_rekey_only_the_response_projection`; `preview_and_events_share_typed_fact_ids_and_canonical_order` | IMPLEMENTED |
| 7 | `cursor-exactness` | `a_cursor_walks_the_retained_set_exactly_once`; `a_cursor_resolves_its_pinned_view_after_a_new_publication`; `a_cursor_presented_to_a_changed_query_is_a_mismatch`; `lifecycle[15..18]` | IMPLEMENTED |
| 8 | `live-seal-identity` | `append_then_seal_keeps_one_coherent_event_set`; `public_event_identity_ignores_lineage_but_retains_content`; `every_all_family_contiguous_partition_reconciles_to_exact_cold_sealed_facts`; `duplicate_segment_contents_do_not_invent_path_based_identity` | IMPLEMENTED |
| 9 | `lossless-live-builder` | `a_stream_split_into_parts_reports_the_unsplit_counts_and_coverage_envelope`; `an_incomplete_candidate_is_never_promoted` | IMPLEMENTED |
| 10 | `required-gap-unknown` | `health_of_an_empty_range_is_unknown_not_green`; `missing_required_penalty_is_unknown_even_with_complete_coverage`; `partial_lossy_assumed_or_foreign_coverage_never_turns_green`; `timeline[15..18]` | IMPLEMENTED |
| 11 | `trusted-floor-downsampling` | `trusted_floors_and_unknown_scores_survive_partition_merge_and_downsample`; `every_all_family_contiguous_partition_reconciles_to_exact_cold_sealed_facts` | IMPLEMENTED |
| 12 | `factor-applicability-loss` | `all_supported_factor_families_reach_every_timeline_endpoint`; `every_strict_coverage_axis_is_enforced`; `timeline[15..18]` | IMPLEMENTED |
| 13 | `counter-halo-range-reset` | `all_family_range_edges_use_half_open_ownership_and_one_left_halo`; `reset_gap_and_mixed_series_never_become_zero_deltas`; `boundary_attribution_is_partition_invariant`; `halo_bridge_is_counted_once_for_every_partition` | IMPLEMENTED |
| 14 | `source-taxonomy-units` | `every_populated_canonical_block_matches_forced_raw_and_restart_warm`; `extracts_registered_log_event_layouts_once_with_conservative_quality`; `unsupported_factor_coverage_is_explicit`; `factor_codes_and_units_round_trip`; `event_taxonomy_codes_round_trip_exhaustively` | IMPLEMENTED |
| 15 | `admission-singleflight-bounds` | `concurrent-identical`; `concurrent-disjoint`; decoded and durable admission-bypass tests; `same_fact_key_with_distinct_lineages_runs_independently`; request and waiter cancellation tests | IMPLEMENTED |
| 16 | `memory-fallback-recovery` | `memory-only`; `production_fallback_enforces_lru_hour_byte_and_oversized_budgets`; `backoff_suppresses_a_second_publication_attempt`; `publication_failure_returns_fresh_facts_then_serves_the_fallback`; `lifecycle[15..18]` | IMPLEMENTED |
| 17 | `quota-gc-safety` | exact quota and optional-quota tests; owner contention; reopened-inode accounting; source/symlink preservation; concurrent live GC; complete live-set sibling preservation; `lifecycle[15..18]` | IMPLEMENTED |
| 18 | `nine-modes-one-profile` | exact `all-nine-modes` endpoint set: `derived-cold`, `restart-warm`, `process-hot`, `range-cold/facts-warm`, `live`, `concurrent-identical`, `concurrent-disjoint`, `memory-only`, `oracle-profile`; separate §18.4.6 compact performance profile with all raw samples retained | IMPLEMENTED |

The BDD manifest contains exactly eight scenarios:

- `crates/kronika-bdd/features/timeline_overview.feature`:
  `PostgreSQL 15 publishes one reconciled source-scoped timeline`;
  `PostgreSQL 16 publishes one reconciled source-scoped timeline`;
  `PostgreSQL 17 publishes one reconciled source-scoped timeline`;
  `PostgreSQL 18 publishes one reconciled source-scoped timeline`.
- `crates/kronika-bdd/features/timeline_web_lifecycle.feature`:
  `PostgreSQL 15 real web process recovers sibling indexes across lifecycle boundaries`;
  `PostgreSQL 16 real web process recovers sibling indexes across lifecycle boundaries`;
  `PostgreSQL 17 real web process recovers sibling indexes across lifecycle boundaries`;
  `PostgreSQL 18 real web process recovers sibling indexes across lifecycle boundaries`.

The lifecycle scenarios launch the actual `pg_kronika-web` executable in
isolated owned directories. They use real HTTP and Prometheus requests,
deterministic readiness and publication barriers, graceful shutdown or
asserted process death, and distinct restart processes. Their assertions cover
missing, corrupt and stale sibling indexes, interrupted publication, bounded
fallback and recovery, durable zero-PGM restart hits, process-local cursor
expiry, source preservation and deterministic owner contention.

Release synchronization also requires:

- PostgreSQL 15–18 BDD through collection, PGM, sibling OVF and all three
  timeline endpoints;
- one machine-readable artifact whose evidence manifest names exact test
  binaries, test cases, BDD scenarios, CI jobs, Git commit, run, attempt and
  artifact checksum;
- strict final validation of the same-head Actions run without retries;
- synchronized English/Russian README, qualification guide, normative
  specification, OpenAPI and CI acceptance matrix;
- explicit `owner_deferred` only for the deployment-specific budgets and
  charts that the normative specification already defers.
