#!/usr/bin/env python3
"""Validate one exact-head overview qualification artifact."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import sys
from pathlib import Path

MODES = (
    "derived-cold",
    "restart-warm",
    "process-hot",
    "range-cold/facts-warm",
    "live",
    "concurrent-identical",
    "concurrent-disjoint",
    "memory-only",
    "oracle-profile",
)

REQUIREMENTS = (
    "restart-warm-zero-pgm",
    "raw-index-all-families",
    "partition-seal-invariance",
    "ovf-fault-fallback",
    "source-damage-visible",
    "policy-reuse",
    "cursor-exactness",
    "live-seal-identity",
    "lossless-live-builder",
    "required-gap-unknown",
    "trusted-floor-downsampling",
    "factor-applicability-loss",
    "counter-halo-range-reset",
    "source-taxonomy-units",
    "admission-singleflight-bounds",
    "memory-fallback-recovery",
    "quota-gc-safety",
    "nine-modes-one-profile",
)

CI_JOBS = (
    "lint",
    "deps",
    "test",
    "coverage",
    "overview-qualification",
    "bdd-matrix",
)

BDD_SCENARIOS = (
    *(
        (
            "crates/kronika-bdd/features/timeline_overview.feature",
            f"PostgreSQL {version} publishes one reconciled source-scoped timeline",
            version,
        )
        for version in range(15, 19)
    ),
    *(
        (
            "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
            (
                f"PostgreSQL {version} real web process recovers sibling indexes "
                "across lifecycle boundaries"
            ),
            version,
        )
        for version in range(15, 19)
    ),
)

def rust_evidence(binary: str, path: str, name: str) -> tuple[str, str, str, str]:
    return ("rust_test", binary, path, name)


def mode_evidence(name: str) -> tuple[str, str, str, str]:
    return (
        "mode",
        "pg-kronika-web::example/overview_qualification",
        "qualification",
        name,
    )


TIMELINE_BDD_EVIDENCE = tuple(
    (
        "bdd_scenario",
        "kronika-bdd",
        "crates/kronika-bdd/features/timeline_overview.feature",
        f"PostgreSQL {version} publishes one reconciled source-scoped timeline",
    )
    for version in range(15, 19)
)

LIFECYCLE_BDD_EVIDENCE = tuple(
    (
        "bdd_scenario",
        "kronika-bdd",
        "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
        (
            f"PostgreSQL {version} real web process recovers sibling indexes "
            "across lifecycle boundaries"
        ),
    )
    for version in range(15, 19)
)

EXPECTED_EVIDENCE = (
    (
        mode_evidence("restart-warm"),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "cold_build_and_cache_hit_report_exact_io_origins",
        ),
        *LIFECYCLE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/facts.rs",
            "every_populated_canonical_block_matches_forced_raw_and_restart_warm",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/facts.rs",
            "all_family_range_edges_use_half_open_ownership_and_one_left_halo",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
        ),
        mode_evidence("oracle-profile"),
        *TIMELINE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "ten_thousand_random_partition_seal_and_merge_seeds_are_invariant",
        ),
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "corrupt_sidecar_is_atomically_replaced_at_the_same_path",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "wrong_source_at_the_expected_name_is_rejected",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "oversized_candidate_is_rebuilt_and_atomically_replaced",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/container.rs",
            "admission_distinguishes_wrong_source_from_incompatible_versions",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "publication_failure_returns_fresh_facts_then_serves_the_fallback",
        ),
        *LIFECYCLE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_resilience.rs",
            "scheduled_source_scrub_prevents_a_durable_fact_from_masking_damage",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/facts.rs",
            "every_all_family_source_body_crc_failure_stays_a_source_error",
        ),
    ),
    (
        mode_evidence("range-cold/facts-warm"),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/overview/cache.rs",
            "policy_versions_rekey_only_the_response_projection",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "preview_and_events_share_typed_fact_ids_and_canonical_order",
        ),
    ),
    (
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "a_cursor_walks_the_retained_set_exactly_once",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "a_cursor_resolves_its_pinned_view_after_a_new_publication",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "a_cursor_presented_to_a_changed_query_is_a_mismatch",
        ),
        *LIFECYCLE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/overview/live.rs",
            "append_then_seal_keeps_one_coherent_event_set",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/notable.rs",
            "public_event_identity_ignores_lineage_but_retains_content",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "duplicate_segment_contents_do_not_invent_path_based_identity",
        ),
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "a_stream_split_into_parts_reports_the_unsplit_counts_and_coverage_envelope",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "an_incomplete_candidate_is_never_promoted",
        ),
    ),
    (
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "health_of_an_empty_range_is_unknown_not_green",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/health.rs",
            "missing_required_penalty_is_unknown_even_with_complete_coverage",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/health.rs",
            "partial_lossy_assumed_or_foreign_coverage_never_turns_green",
        ),
        *TIMELINE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/health_line.rs",
            "trusted_floors_and_unknown_scores_survive_partition_merge_and_downsample",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/live.rs",
            "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
        ),
    ),
    (
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_timeline.rs",
            "all_supported_factor_families_reach_every_timeline_endpoint",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/health.rs",
            "every_strict_coverage_axis_is_enforced",
        ),
        *TIMELINE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/facts.rs",
            "all_family_range_edges_use_half_open_ownership_and_one_left_halo",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/reduce.rs",
            "reset_gap_and_mixed_series_never_become_zero_deltas",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/reduce.rs",
            "boundary_attribution_is_partition_invariant",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/reduce.rs",
            "halo_bridge_is_counted_once_for_every_partition",
        ),
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/facts.rs",
            "every_populated_canonical_block_matches_forced_raw_and_restart_warm",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/facts.rs",
            "extracts_registered_log_event_layouts_once_with_conservative_quality",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/metric_extract.rs",
            "unsupported_factor_coverage_is_explicit",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/metric.rs",
            "factor_codes_and_units_round_trip",
        ),
        rust_evidence(
            "kronika-analytics",
            "crates/kronika-analytics/src/overview/fact.rs",
            "event_taxonomy_codes_round_trip_exhaustively",
        ),
    ),
    (
        mode_evidence("concurrent-identical"),
        mode_evidence("concurrent-disjoint"),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_admission.rs",
            "an_exact_decoded_hit_bypasses_cold_admission",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/tests/overview_admission.rs",
            "an_exact_durable_hit_bypasses_cold_admission_after_restart",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/overview/singleflight.rs",
            "same_fact_key_with_distinct_lineages_runs_independently",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/overview/singleflight.rs",
            "cancelling_the_request_does_not_cancel_the_leader",
        ),
        rust_evidence(
            "pg-kronika-web",
            "bins/pg_kronika-web/src/overview/admission.rs",
            "cancelling_a_waiter_removes_its_ticket",
        ),
    ),
    (
        mode_evidence("memory-only"),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "production_fallback_enforces_lru_hour_byte_and_oversized_budgets",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "backoff_suppresses_a_second_publication_attempt",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/publish.rs",
            "publication_failure_returns_fresh_facts_then_serves_the_fallback",
        ),
        *LIFECYCLE_BDD_EVIDENCE,
    ),
    (
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "quota_accounts_only_derived_files_in_the_owned_data_directory",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "optional_quota_blocks_publication_without_touching_the_source",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "data_directory_owner_contention_fails_closed",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "unlinked_bytes_come_from_the_reopened_validated_inode",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "source_entries_and_symlinks_are_never_followed_or_removed",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "concurrent_live_gc_read_and_publish_preserve_the_sidecar",
        ),
        rust_evidence(
            "kronika-reader",
            "crates/kronika-reader/src/overview/gc/tests.rs",
            "complete_typed_live_set_preserves_each_sibling_sidecar",
        ),
        *LIFECYCLE_BDD_EVIDENCE,
    ),
    (
        (
            "mode_set",
            "pg-kronika-web::example/overview_qualification",
            "qualification",
            "all-nine-modes",
        ),
    ),
)

WORK_FIELDS = (
    "pgm_body_reads",
    "pgm_body_bytes",
    "pgm_sections_decoded",
    "pgm_rows_decoded",
    "fact_reads",
    "fact_stored_bytes",
    "fact_decoded_bytes",
    "sidecar_writes",
    "sidecar_write_bytes",
    "source_builds",
    "singleflight_builds",
    "singleflight_waiters",
    "persistence_failures",
    "publication_attempts",
    "retry_probes",
    "max_inflight_builds",
    "max_inflight_file_descriptors",
    "max_queue_depth",
    "decoded_cache_entries",
    "decoded_cache_bytes",
    "fallback_hits",
    "fallback_request_pgm_body_reads",
    "recovered_restart_pgm_body_reads",
    "fallback_resident_entries",
    "fallback_resident_segment_hours",
    "fallback_resident_bytes",
    "completed_active_parts",
    "visibility_lag_us",
    "tail_pending_from_offset_bytes",
    "tail_pending_to_offset_bytes",
    "successful_responses",
    "serialized_response_bytes",
)


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--exact-head", default=os.environ.get("GITHUB_SHA"))
    parser.add_argument("--final", action="store_true")
    parser.add_argument("--raw-artifact", type=Path)
    parser.add_argument("--checksum-file", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def check(condition: bool, message: str, failures: list[str]) -> None:
    if not condition:
        failures.append(message)


def validate_budgets(
    budgets: dict[str, object],
    final: bool,
    failures: list[str],
    warnings: list[str],
) -> None:
    exact_fields(
        budgets,
        {
            "disk_bytes",
            "resident_bytes",
            "disk_within_budget",
            "resident_within_budget",
            "deployment_budget_status",
            "qualification_blocked",
        },
        "deployment budgets",
        failures,
    )
    disk_bytes = budgets.get("disk_bytes")
    resident_bytes = budgets.get("resident_bytes")
    disk_within = budgets.get("disk_within_budget")
    resident_within = budgets.get("resident_within_budget")
    status = budgets.get("deployment_budget_status")
    blocked = budgets.get("qualification_blocked")

    if disk_bytes is None and resident_bytes is None:
        check(
            disk_within is None and resident_within is None,
            "owner-deferred budgets contain a deployment verdict",
            failures,
        )
        check(status == "owner_deferred", "wrong owner-deferred budget status", failures)
        check(blocked is False, "owner-deferred budgets block qualification", failures)
        warnings.append(
            "deployment budgets are owner-deferred; exact size accounting has no deployment verdict"
        )
        return

    if (disk_bytes is None) != (resident_bytes is None):
        check(False, "dense deployment budgets must be configured together", failures)
        check(
            status == "incomplete_configuration",
            "wrong incomplete deployment budget status",
            failures,
        )
        check(blocked is True, "incomplete deployment budgets do not block", failures)
        return

    within_approved = disk_within is True and resident_within is True
    expected_status = "within_approved" if within_approved else "exceeds_approved"
    expected_blocked = not within_approved
    check(status == expected_status, "wrong configured deployment budget status", failures)
    check(
        blocked is expected_blocked,
        "configured deployment budget block state is inconsistent",
        failures,
    )
    if final:
        check(
            disk_within is True,
            "dense fact file exceeds its approved disk budget",
            failures,
        )
        check(
            resident_within is True,
            "dense pinned working set exceeds its approved resident budget",
            failures,
        )
    elif not within_approved:
        warnings.append("dense accounting exceeds a configured deployment budget")


def validate_storage(storage: dict[str, object], failures: list[str]) -> None:
    exact_fields(
        storage,
        {
            "model",
            "active_journal_name",
            "pgm_file_name",
            "sidecar_file_name",
            "same_stem",
        },
        "storage profile",
        failures,
    )
    pgm_name = storage.get("pgm_file_name")
    sidecar_name = storage.get("sidecar_file_name")
    check(
        storage.get("model") == "owned-data-directory-sibling-sidecars-v1",
        "wrong overview storage model",
        failures,
    )
    check(
        storage.get("active_journal_name") == "active.parts",
        "wrong active journal name",
        failures,
    )
    check(
        isinstance(pgm_name, str)
        and isinstance(sidecar_name, str)
        and pgm_name.endswith(".pgm")
        and sidecar_name.endswith(".ovf")
        and pgm_name.removesuffix(".pgm") == sidecar_name.removesuffix(".ovf")
        and storage.get("same_stem") is True,
        "qualification files are not same-stem PGM/OVF siblings",
        failures,
    )


def is_integer(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def as_mapping(
    value: object, label: str, failures: list[str]
) -> dict[str, object]:
    if not isinstance(value, dict):
        failures.append(f"{label} is not an object")
        return {}
    return value


def as_list(value: object, label: str, failures: list[str]) -> list[object]:
    if not isinstance(value, list):
        failures.append(f"{label} is not an array")
        return []
    return value


def exact_fields(
    value: dict[str, object],
    fields: set[str],
    label: str,
    failures: list[str],
) -> None:
    check(
        set(value) == fields,
        f"{label} does not match the exact v2 schema",
        failures,
    )


def nonnegative_integer(
    value: object, label: str, failures: list[str], *, positive: bool = False
) -> int:
    valid = is_integer(value) and value >= int(positive)
    check(valid, f"{label} is not a {'positive' if positive else 'non-negative'} integer", failures)
    return value if valid else 0


def percentile(values: list[int], percent: int) -> int:
    ordered = sorted(values)
    rank = ((len(ordered) * percent + 99) // 100) - 1
    return ordered[max(0, min(rank, len(ordered) - 1))]


def validate_host(host: dict[str, object], failures: list[str]) -> None:
    exact_fields(
        host,
        {
            "os",
            "arch",
            "kernel",
            "filesystem",
            "filesystem_device",
            "process_samples_are_fresh_children",
            "syscall_trace_scope",
            "storage_cold",
        },
        "host profile",
        failures,
    )
    check(host.get("os") == "linux", "qualification host is not Linux", failures)
    check(
        isinstance(host.get("arch"), str) and bool(host.get("arch")),
        "qualification host architecture is absent",
        failures,
    )
    check(
        isinstance(host.get("kernel"), str) and bool(host.get("kernel")),
        "qualification kernel profile is absent",
        failures,
    )
    check(
        isinstance(host.get("filesystem"), str) and bool(host.get("filesystem")),
        "qualification filesystem profile is absent",
        failures,
    )
    nonnegative_integer(
        host.get("filesystem_device"),
        "qualification filesystem device",
        failures,
    )
    check(
        host.get("process_samples_are_fresh_children") is True,
        "timing samples are not fresh child processes",
        failures,
    )
    check(
        host.get("syscall_trace_scope")
        == "one complete fresh worker process, separate from latency samples",
        "syscall trace scope is not the declared separate process pass",
        failures,
    )
    check(
        host.get("storage_cold") is False,
        "artifact makes an unsupported storage-cold claim",
        failures,
    )


def validate_fixture(fixture: dict[str, object], failures: list[str]) -> None:
    exact_fields(
        fixture,
        {
            "schema_version",
            "cadence_us",
            "source_rows",
            "source_sections",
            "source_bytes",
            "counter_series",
            "counter_samples",
            "gauge_series",
            "gauge_samples",
            "reset_markers",
            "entity_states",
            "factor_coverage",
            "event_facts",
            "auxiliary_datasets",
        },
        "fixture profile",
        failures,
    )
    check(
        fixture.get("schema_version") == "overview-dense-hour-v2",
        "wrong dense fixture schema",
        failures,
    )
    exact = {
        "cadence_us": 5_000_000,
        "source_rows": 720,
        "source_sections": 3,
    }
    for field, expected in exact.items():
        check(
            fixture.get(field) == expected,
            f"dense fixture {field} differs from {expected}",
            failures,
        )
    for field in (
        "source_bytes",
        "counter_series",
        "counter_samples",
        "gauge_series",
        "gauge_samples",
        "reset_markers",
        "factor_coverage",
        "event_facts",
    ):
        nonnegative_integer(
            fixture.get(field), f"dense fixture {field}", failures, positive=True
        )
    nonnegative_integer(
        fixture.get("entity_states"), "dense fixture entity_states", failures
    )
    expected_datasets = {
        "all-canonical-families-v2",
        "sparse-30-percent",
        "reset-at-segment-boundary",
        "duplicate-timestamps-and-rows",
        "fatal-burst-at-collector-limit",
        "explicit-pg-log-gap",
        "two-sources",
        "corrupt-ovf-block",
        "corrupt-pgm-section",
        "mixed-cadence-5-10-30-60-3600",
    }
    datasets = as_list(
        fixture.get("auxiliary_datasets"),
        "dense fixture auxiliary_datasets",
        failures,
    )
    check(
        len(datasets) == len(expected_datasets)
        and set(datasets) == expected_datasets,
        "dense fixture does not name the exact M6 auxiliary datasets",
        failures,
    )


def validate_accounting(
    accounting: dict[str, object], failures: list[str]
) -> None:
    exact_fields(
        accounting,
        {
            "fact_file_logical_bytes",
            "fact_file_allocated_bytes",
            "header_and_directory_bytes",
            "stored_block_bytes",
            "decoded_block_bytes",
            "resident_fact_bytes",
            "pinned_fact_bytes",
            "fixed_metric_stored_bytes",
            "variable_event_string_stored_bytes",
            "retained_metric_samples",
            "fixed_metric_bytes_per_sample_numerator",
            "fixed_metric_bytes_per_sample_denominator",
            "identity_holds",
        },
        "accounting profile",
        failures,
    )
    fields = (
        "fact_file_logical_bytes",
        "fact_file_allocated_bytes",
        "header_and_directory_bytes",
        "stored_block_bytes",
        "decoded_block_bytes",
        "resident_fact_bytes",
        "pinned_fact_bytes",
        "fixed_metric_stored_bytes",
        "variable_event_string_stored_bytes",
        "retained_metric_samples",
        "fixed_metric_bytes_per_sample_numerator",
        "fixed_metric_bytes_per_sample_denominator",
    )
    values = {
        field: nonnegative_integer(
            accounting.get(field), f"accounting {field}", failures, positive=True
        )
        for field in fields
    }
    check(
        values["header_and_directory_bytes"] + values["stored_block_bytes"]
        == values["fact_file_logical_bytes"],
        "logical fact bytes do not equal header/directory plus stored blocks",
        failures,
    )
    check(
        accounting.get("identity_holds") is True,
        "artifact does not assert the exact fact-byte identity",
        failures,
    )
    check(
        values["fact_file_allocated_bytes"] >= values["fact_file_logical_bytes"],
        "allocated fact bytes are smaller than the logical file",
        failures,
    )
    check(
        values["resident_fact_bytes"] >= values["decoded_block_bytes"],
        "resident fact bytes are smaller than decoded blocks",
        failures,
    )
    check(
        values["pinned_fact_bytes"] >= values["resident_fact_bytes"],
        "pinned fact bytes are smaller than resident facts",
        failures,
    )
    check(
        values["fixed_metric_stored_bytes"]
        + values["variable_event_string_stored_bytes"]
        <= values["stored_block_bytes"],
        "classified stored bytes exceed all stored blocks",
        failures,
    )
    check(
        values["fixed_metric_bytes_per_sample_numerator"]
        == values["fixed_metric_stored_bytes"],
        "fixed-byte rational numerator differs from fixed metric bytes",
        failures,
    )
    check(
        values["fixed_metric_bytes_per_sample_denominator"]
        == values["retained_metric_samples"],
        "fixed-byte rational denominator differs from retained samples",
        failures,
    )


def validate_syscalls(
    mode: str, syscalls: dict[str, object], failures: list[str]
) -> None:
    exact_fields(
        syscalls,
        {
            "process_scope",
            "opens",
            "reads",
            "writes",
            "syncs",
            "renames",
            "unlinks",
            "total_traced",
        },
        f"{mode} syscall profile",
        failures,
    )
    check(
        syscalls.get("process_scope") is True,
        f"{mode} syscall evidence is not process-scoped",
        failures,
    )
    categories = ("opens", "reads", "writes", "syncs", "renames", "unlinks")
    counts = {
        field: nonnegative_integer(
            syscalls.get(field), f"{mode} syscall {field}", failures
        )
        for field in categories
    }
    total = nonnegative_integer(
        syscalls.get("total_traced"),
        f"{mode} syscall total_traced",
        failures,
        positive=True,
    )
    check(
        sum(counts.values()) == total,
        f"{mode} syscall categories do not reconcile to total_traced",
        failures,
    )


def validate_sample(
    mode: str, sample: dict[str, object], failures: list[str]
) -> dict[str, int]:
    exact_fields(
        sample,
        {
            "wall_ns",
            "cpu_ns",
            "process_peak_rss_bytes",
            "fd_start",
            "fd_peak",
            "fd_end",
            "proc_io",
            "work",
        },
        f"{mode} sample",
        failures,
    )
    wall_ns = nonnegative_integer(
        sample.get("wall_ns"), f"{mode} sample wall_ns", failures, positive=True
    )
    nonnegative_integer(sample.get("cpu_ns"), f"{mode} sample cpu_ns", failures)
    nonnegative_integer(
        sample.get("process_peak_rss_bytes"),
        f"{mode} sample process_peak_rss_bytes",
        failures,
        positive=True,
    )
    fd_start = nonnegative_integer(
        sample.get("fd_start"), f"{mode} sample fd_start", failures, positive=True
    )
    fd_peak = nonnegative_integer(
        sample.get("fd_peak"), f"{mode} sample fd_peak", failures, positive=True
    )
    fd_end = nonnegative_integer(
        sample.get("fd_end"), f"{mode} sample fd_end", failures, positive=True
    )
    check(
        fd_peak >= max(fd_start, fd_end),
        f"{mode} sample FD peak is below an endpoint count",
        failures,
    )

    proc_io = as_mapping(sample.get("proc_io"), f"{mode} sample proc_io", failures)
    exact_fields(
        proc_io,
        {
            "rchar",
            "wchar",
            "syscr",
            "syscw",
            "read_bytes",
            "write_bytes",
            "cancelled_write_bytes",
        },
        f"{mode} proc_io",
        failures,
    )
    for field in (
        "rchar",
        "wchar",
        "syscr",
        "syscw",
        "read_bytes",
        "write_bytes",
        "cancelled_write_bytes",
    ):
        nonnegative_integer(proc_io.get(field), f"{mode} proc_io {field}", failures)

    raw_work = as_mapping(sample.get("work"), f"{mode} sample work", failures)
    check(
        set(raw_work) == set(WORK_FIELDS),
        f"{mode} work counters do not match the exact v2 schema",
        failures,
    )
    work = {
        field: nonnegative_integer(
            raw_work.get(field), f"{mode} work {field}", failures
        )
        for field in WORK_FIELDS
    }
    check(wall_ns > 0, f"{mode} sample has no elapsed time", failures)
    return work


def all_zero(work: dict[str, int], fields: tuple[str, ...]) -> bool:
    return all(work[field] == 0 for field in fields)


def validate_mode_work(
    mode: str, work: dict[str, int], failures: list[str]
) -> None:
    pgm = (
        "pgm_body_reads",
        "pgm_body_bytes",
        "pgm_sections_decoded",
        "pgm_rows_decoded",
    )
    facts = ("fact_reads", "fact_stored_bytes", "fact_decoded_bytes")
    writes = ("sidecar_writes", "sidecar_write_bytes")

    if mode == "derived-cold":
        check(
            work["pgm_body_reads"] > 0
            and work["pgm_body_bytes"] > 0
            and work["pgm_sections_decoded"] > 0
            and work["pgm_rows_decoded"] == 720,
            "derived-cold did not decode the exact dense-hour PGM",
            failures,
        )
        check(all_zero(work, facts), "derived-cold read an existing OVF", failures)
        check(
            work["source_builds"] == 1
            and work["sidecar_writes"] == 1
            and work["sidecar_write_bytes"] > 0,
            "derived-cold did not build and publish exactly one fact file",
            failures,
        )
    elif mode == "restart-warm":
        check(all_zero(work, pgm), "restart-warm read or decoded PGM bodies", failures)
        check(
            work["fact_reads"] > 0
            and work["fact_stored_bytes"] > 0
            and work["fact_decoded_bytes"] > 0,
            "restart-warm did not read selected OVF blocks",
            failures,
        )
        check(
            work["source_builds"] == 0 and all_zero(work, writes),
            "restart-warm rebuilt or rewrote facts",
            failures,
        )
    elif mode in ("process-hot", "range-cold/facts-warm"):
        check(
            all_zero(work, pgm + facts + writes)
            and work["source_builds"] == 0,
            f"{mode} performed source/fact I/O or rebuilt facts",
            failures,
        )
        check(
            work["decoded_cache_entries"] > 0
            and work["decoded_cache_bytes"] > 0,
            f"{mode} did not use bounded decoded residency",
            failures,
        )
    elif mode == "live":
        check(
            work["completed_active_parts"] == 1,
            "live did not expose exactly one newly completed frame",
            failures,
        )
        check(
            0 < work["visibility_lag_us"] <= 2_500_000,
            "live visibility exceeded 2.5 seconds or was not measured",
            failures,
        )
        check(
            work["tail_pending_from_offset_bytes"] > 0
            and work["tail_pending_to_offset_bytes"]
            - work["tail_pending_from_offset_bytes"]
            == 4,
            "live did not retain the exact four-byte incomplete tail",
            failures,
        )
    elif mode == "concurrent-identical":
        check(
            work["singleflight_builds"] == 1
            and work["source_builds"] == 1
            and work["singleflight_waiters"] == 15
            and work["successful_responses"] == 16,
            "concurrent-identical did not share one build across 16 results",
            failures,
        )
        check(
            0 < work["max_inflight_builds"] <= 4
            and 0 < work["max_inflight_file_descriptors"] <= 16,
            "concurrent-identical exceeded or did not measure cold bounds",
            failures,
        )
    elif mode == "concurrent-disjoint":
        check(
            work["singleflight_builds"] == 16
            and work["source_builds"] == 16
            and work["successful_responses"] == 16,
            "concurrent-disjoint did not complete 16 independent builds",
            failures,
        )
        check(
            0 < work["max_inflight_builds"] <= 4
            and 0 < work["max_inflight_file_descriptors"] <= 16
            and 0 < work["max_queue_depth"] <= 64,
            "concurrent-disjoint did not stay inside worker/FD/queue bounds",
            failures,
        )
    elif mode == "memory-only":
        check(
            work["source_builds"] == 1
            and work["persistence_failures"] == 1
            and work["fallback_hits"] == 1,
            "memory-only did not exercise one failed publication and fallback hit",
            failures,
        )
        check(
            work["fallback_request_pgm_body_reads"] == 0
            and work["recovered_restart_pgm_body_reads"] == 0,
            "memory-only fallback or recovered restart read PGM bodies",
            failures,
        )
        check(
            work["fallback_resident_entries"] == 1
            and 0 < work["fallback_resident_segment_hours"] <= 2
            and 0 < work["fallback_resident_bytes"] <= 16 * 1024 * 1024,
            "memory-only fallback exceeded or omitted byte/hour residency",
            failures,
        )
        check(
            work["publication_attempts"] >= 2
            and work["retry_probes"] == 1
            and work["sidecar_writes"] == 1
            and work["fact_reads"] > 0,
            "memory-only did not prove probe, recovery, and durable restart",
            failures,
        )
    elif mode == "oracle-profile":
        check(
            work["pgm_body_reads"] > 0
            and work["fact_reads"] > 0
            and work["source_builds"] == 0
            and all_zero(work, writes),
            "oracle-profile did not compare raw PGM and admitted OVF without writes",
            failures,
        )
        check(
            work["successful_responses"] == 2,
            "oracle-profile did not compare both full and partial ranges",
            failures,
        )

    expected_responses = {
        "derived-cold": 1,
        "restart-warm": 1,
        "process-hot": 1,
        "range-cold/facts-warm": 1,
        "live": 1,
        "concurrent-identical": 16,
        "concurrent-disjoint": 16,
        "memory-only": 3,
        "oracle-profile": 2,
    }
    check(
        work["successful_responses"] == expected_responses[mode],
        f"{mode} successful response count is wrong",
        failures,
    )
    if mode in {
        "derived-cold",
        "restart-warm",
        "process-hot",
        "range-cold/facts-warm",
        "live",
    }:
        check(
            work["serialized_response_bytes"] > 0,
            f"{mode} did not account serialized HTTP bytes",
            failures,
        )


def validate_modes(
    mode_rows: list[object], final: bool, failures: list[str]
) -> None:
    rows: dict[str, dict[str, object]] = {}
    for index, raw_row in enumerate(mode_rows):
        row = as_mapping(raw_row, f"mode row {index}", failures)
        exact_fields(
            row,
            {
                "mode",
                "semantics",
                "iterations",
                "wall_p50_ns",
                "wall_p95_ns",
                "wall_p99_ns",
                "cpu_p50_ns",
                "cpu_p95_ns",
                "cpu_p99_ns",
                "peak_rss_bytes",
                "peak_open_file_descriptors",
                "samples",
                "syscalls",
            },
            f"mode row {index}",
            failures,
        )
        mode = row.get("mode")
        check(
            isinstance(mode, str) and mode in MODES,
            f"mode row {index} has an unknown mode",
            failures,
        )
        if isinstance(mode, str):
            check(mode not in rows, f"mode {mode} is duplicated", failures)
            rows[mode] = row
    check(
        set(rows) == set(MODES) and len(mode_rows) == len(MODES),
        "artifact does not contain the exact nine modes",
        failures,
    )

    for mode in MODES:
        row = rows.get(mode)
        if row is None:
            continue
        check(
            row.get("semantics")
            == "fresh child process per sample; OS page cache uncontrolled/warm; storage-cold false",
            f"{mode} has the wrong cache/process semantics",
            failures,
        )
        iterations = nonnegative_integer(
            row.get("iterations"), f"{mode} iterations", failures, positive=True
        )
        samples_raw = as_list(row.get("samples"), f"{mode} samples", failures)
        expected_iterations = 20 if final else iterations
        check(
            iterations == len(samples_raw),
            f"{mode} iteration count differs from its samples",
            failures,
        )
        check(
            iterations == expected_iterations
            and (final or 1 <= iterations <= 20),
            f"{mode} does not have {'exactly 20' if final else '1..20'} samples",
            failures,
        )
        samples = [
            as_mapping(sample, f"{mode} sample {index}", failures)
            for index, sample in enumerate(samples_raw)
        ]
        if not samples:
            continue
        work_rows = [
            validate_sample(mode, sample, failures) for sample in samples
        ]
        for work in work_rows:
            validate_mode_work(mode, work, failures)

        wall = [
            nonnegative_integer(
                sample.get("wall_ns"), f"{mode} summary wall sample", failures
            )
            for sample in samples
        ]
        cpu = [
            nonnegative_integer(
                sample.get("cpu_ns"), f"{mode} summary CPU sample", failures
            )
            for sample in samples
        ]
        for field, values, percent in (
            ("wall_p50_ns", wall, 50),
            ("wall_p95_ns", wall, 95),
            ("wall_p99_ns", wall, 99),
            ("cpu_p50_ns", cpu, 50),
            ("cpu_p95_ns", cpu, 95),
            ("cpu_p99_ns", cpu, 99),
        ):
            check(
                row.get(field) == percentile(values, percent),
                f"{mode} {field} is not the exact sample percentile",
                failures,
            )
        check(
            row.get("peak_rss_bytes")
            == max(sample.get("process_peak_rss_bytes", 0) for sample in samples),
            f"{mode} peak RSS does not match its samples",
            failures,
        )
        check(
            row.get("peak_open_file_descriptors")
            == max(sample.get("fd_peak", 0) for sample in samples),
            f"{mode} peak FD count does not match its samples",
            failures,
        )
        validate_syscalls(
            mode,
            as_mapping(row.get("syscalls"), f"{mode} syscalls", failures),
            failures,
        )

    if final and set(rows) == set(MODES):
        cold = rows["derived-cold"].get("wall_p95_ns", 0)
        restart = rows["restart-warm"].get("wall_p95_ns", 0)
        hot = rows["process-hot"].get("wall_p95_ns", 0)
        ranged = rows["range-cold/facts-warm"].get("wall_p95_ns", 0)
        if all(is_integer(value) for value in (cold, restart, hot, ranged)):
            check(
                hot * 4 <= cold,
                "process-hot p95 exceeds 25% of derived-cold",
                failures,
            )
            check(
                restart * 4 <= cold,
                "restart-warm p95 exceeds 25% of derived-cold",
                failures,
            )
            check(
                ranged * 2 <= cold,
                "range-cold/facts-warm p95 exceeds 50% of derived-cold",
                failures,
            )


def expected_binary(path: str) -> str | None:
    if path.startswith("crates/kronika-reader/"):
        return "kronika-reader"
    if path.startswith("crates/kronika-analytics/"):
        return "kronika-analytics"
    if path.startswith("bins/pg_kronika-web/"):
        return "pg-kronika-web"
    return None


def source_contains_test(path: Path, name: str) -> bool:
    source = path.read_text(encoding="utf-8")
    return re.search(rf"\bfn\s+{re.escape(name)}\b", source) is not None


def validate_acceptance(
    acceptance: list[object],
    *,
    final: bool,
    repo_root: Path,
    failures: list[str],
) -> None:
    check(
        len(acceptance) == len(REQUIREMENTS),
        "acceptance dossier is not the exact 18-row set",
        failures,
    )
    for index, requirement in enumerate(REQUIREMENTS, start=1):
        if index > len(acceptance):
            continue
        row = as_mapping(acceptance[index - 1], f"acceptance row {index}", failures)
        exact_fields(
            row,
            {
                "id",
                "requirement",
                "implementation_status",
                "evidence",
                "decision",
            },
            f"acceptance row {index}",
            failures,
        )
        check(row.get("id") == index, f"acceptance row {index} has the wrong ID", failures)
        check(
            row.get("requirement") == requirement,
            f"acceptance row {index} has the wrong requirement code",
            failures,
        )
        check(
            row.get("implementation_status") == "IMPLEMENTED",
            f"acceptance row {index} is not IMPLEMENTED",
            failures,
        )
        expected_decision = "PASS" if final else "PENDING_EXACT_HEAD_CI"
        check(
            row.get("decision") == expected_decision,
            f"acceptance row {index} has the wrong decision",
            failures,
        )
        evidence = as_list(
            row.get("evidence"), f"acceptance row {index} evidence", failures
        )
        check(bool(evidence), f"acceptance row {index} has no direct evidence", failures)
        seen_mode_evidence: set[str] = set()
        actual_evidence: list[tuple[object, object, object, object]] = []
        for evidence_index, raw_ref in enumerate(evidence):
            ref = as_mapping(
                raw_ref,
                f"acceptance row {index} evidence {evidence_index}",
                failures,
            )
            check(
                set(ref) == {"kind", "binary", "path", "name"},
                f"acceptance row {index} evidence has the wrong schema",
                failures,
            )
            kind = ref.get("kind")
            binary = ref.get("binary")
            path = ref.get("path")
            name = ref.get("name")
            actual_evidence.append((kind, binary, path, name))
            check(
                all(isinstance(value, str) and value for value in (kind, binary, path, name)),
                f"acceptance row {index} evidence has an empty identifier",
                failures,
            )
            if not all(isinstance(value, str) for value in (kind, binary, path, name)):
                continue
            if kind == "rust_test":
                check(
                    re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) is not None,
                    f"acceptance row {index} has an invalid Rust test name",
                    failures,
                )
                source = (repo_root / path).resolve()
                try:
                    source.relative_to(repo_root)
                except ValueError:
                    check(False, f"acceptance row {index} test path escapes the repository", failures)
                    continue
                check(source.is_file(), f"acceptance row {index} test source is absent", failures)
                if source.is_file():
                    check(
                        source_contains_test(source, name),
                        f"acceptance row {index} test {name} is absent from {path}",
                        failures,
                    )
                check(
                    binary == expected_binary(path),
                    f"acceptance row {index} names the wrong Rust test binary",
                    failures,
                )
            elif kind == "bdd_scenario":
                source = (repo_root / path).resolve()
                try:
                    source.relative_to(repo_root)
                except ValueError:
                    check(
                        False,
                        f"acceptance row {index} BDD path escapes the repository",
                        failures,
                    )
                    continue
                check(
                    binary == "kronika-bdd"
                    and any(
                        path == expected_path and name == expected_name
                        for expected_path, expected_name, _postgres in BDD_SCENARIOS
                    ),
                    f"acceptance row {index} has invalid BDD evidence coordinates",
                    failures,
                )
                check(
                    source.is_file(),
                    f"acceptance row {index} BDD feature is absent",
                    failures,
                )
                if source.is_file():
                    text = source.read_text(encoding="utf-8")
                    check(
                        f"Scenario: {name}" in text,
                        f"acceptance row {index} BDD scenario {name!r} is absent",
                        failures,
                    )
            elif kind == "mode":
                check(
                    path == "qualification"
                    and binary == "pg-kronika-web::example/overview_qualification"
                    and name in MODES,
                    f"acceptance row {index} has invalid mode evidence",
                    failures,
                )
                seen_mode_evidence.add(name)
            elif kind == "mode_set":
                check(
                    index == 18
                    and path == "qualification"
                    and binary == "pg-kronika-web::example/overview_qualification"
                    and name == "all-nine-modes",
                    f"acceptance row {index} has invalid mode-set evidence",
                    failures,
                )
            else:
                check(False, f"acceptance row {index} has unknown evidence kind", failures)
        check(
            tuple(actual_evidence) == EXPECTED_EVIDENCE[index - 1],
            f"acceptance row {index} does not name its exact direct evidence",
            failures,
        )
        if index == 15:
            check(
                seen_mode_evidence
                == {"concurrent-identical", "concurrent-disjoint"},
                "acceptance row 15 does not name both concurrency modes",
                failures,
            )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_checksum(
    artifact_path: Path,
    checksum_file: Path | None,
    *,
    required: bool,
    failures: list[str],
) -> str:
    digest = sha256(artifact_path)
    if checksum_file is None:
        check(not required, "final artifact has no external checksum file", failures)
        return digest
    fields = checksum_file.read_text(encoding="utf-8").split()
    check(bool(fields), "artifact checksum file is empty", failures)
    if fields:
        check(fields[0] == digest, "artifact checksum does not match its bytes", failures)
    return digest


def validate_bdd_scenarios(
    scenarios: list[object], repo_root: Path, failures: list[str]
) -> None:
    actual: list[tuple[object, object, object]] = []
    for index, raw in enumerate(scenarios):
        row = as_mapping(raw, f"BDD scenario {index}", failures)
        check(
            set(row) == {"path", "name", "postgres"},
            f"BDD scenario {index} has the wrong schema",
            failures,
        )
        actual.append((row.get("path"), row.get("name"), row.get("postgres")))
    check(
        tuple(actual) == BDD_SCENARIOS,
        "final artifact does not name the exact PostgreSQL 15-18 timeline and lifecycle scenarios",
        failures,
    )
    for path, name, _version in BDD_SCENARIOS:
        source = repo_root / path
        check(source.is_file(), f"BDD feature is absent: {path}", failures)
        if source.is_file():
            text = source.read_text(encoding="utf-8")
            check(
                f"Scenario: {name}" in text,
                f"BDD scenario is absent from {path}: {name}",
                failures,
            )


def validate_final_ci(
    artifact: dict[str, object],
    *,
    raw_artifact: Path | None,
    repo_root: Path,
    failures: list[str],
) -> None:
    final_ci = as_mapping(artifact.get("final_ci"), "final_ci", failures)
    exact_fields(
        final_ci,
        {
            "schema",
            "exact_head",
            "run_id",
            "run_attempt",
            "acceptance_job",
            "jobs",
            "bdd_scenarios",
            "raw_artifact_sha256",
            "finalized_unix_ms",
            "decision",
        },
        "final_ci",
        failures,
    )
    check(
        final_ci.get("schema") == "pgkronika-overview-qualification-final-ci-v1",
        "final CI record has the wrong schema",
        failures,
    )
    check(
        final_ci.get("exact_head") == artifact.get("git_head"),
        "final CI exact head differs from the artifact head",
        failures,
    )
    ci = as_mapping(artifact.get("ci"), "ci profile", failures)
    check(
        final_ci.get("run_id") == ci.get("run_id"),
        "final CI run differs from the measurement run",
        failures,
    )
    check(
        final_ci.get("run_attempt") == ci.get("run_attempt") == "1",
        "M6 qualification is not from Actions attempt 1",
        failures,
    )
    check(
        ci.get("job") == "overview-qualification",
        "raw measurements came from the wrong CI job",
        failures,
    )
    check(
        ci.get("artifact_name") == "overview-qualification-raw",
        "raw measurement artifact has the wrong name",
        failures,
    )
    check(
        final_ci.get("acceptance_job") == "overview-m6-acceptance",
        "final artifact names the wrong acceptance job",
        failures,
    )
    check(final_ci.get("decision") == "PASS", "final CI decision is not PASS", failures)
    jobs = as_mapping(final_ci.get("jobs"), "final CI jobs", failures)
    check(set(jobs) == set(CI_JOBS), "final CI job set is incomplete or foreign", failures)
    for job in CI_JOBS:
        check(jobs.get(job) == "success", f"final CI job {job} is not green", failures)
    validate_bdd_scenarios(
        as_list(final_ci.get("bdd_scenarios"), "final CI BDD scenarios", failures),
        repo_root,
        failures,
    )
    raw_digest = final_ci.get("raw_artifact_sha256")
    check(
        isinstance(raw_digest, str)
        and re.fullmatch(r"[0-9a-f]{64}", raw_digest) is not None,
        "final CI raw artifact checksum is invalid",
        failures,
    )
    check(
        is_integer(final_ci.get("finalized_unix_ms"))
        and final_ci.get("finalized_unix_ms", 0) > 0,
        "final CI record has no finalization time",
        failures,
    )
    check(raw_artifact is not None, "final validation has no raw artifact", failures)
    if raw_artifact is not None:
        check(raw_artifact.is_file(), "raw qualification artifact is absent", failures)
        if raw_artifact.is_file():
            check(
                sha256(raw_artifact) == raw_digest,
                "raw qualification checksum differs from final_ci",
                failures,
            )
            raw = json.loads(raw_artifact.read_text(encoding="utf-8"))
            reconstructed = copy.deepcopy(artifact)
            reconstructed.pop("final_ci", None)
            for row in reconstructed.get("acceptance", []):
                if isinstance(row, dict):
                    row["decision"] = "PENDING_EXACT_HEAD_CI"
            check(
                reconstructed == raw,
                "final artifact changes raw measurements beyond CI decisions",
                failures,
            )


def validate_ci_profile(
    ci: dict[str, object], *, final: bool, failures: list[str]
) -> None:
    exact_fields(
        ci,
        {"repository", "run_id", "run_attempt", "job", "artifact_name"},
        "CI profile",
        failures,
    )
    check(
        ci.get("artifact_name") == "overview-qualification-raw",
        "CI profile names the wrong raw artifact",
        failures,
    )
    local = all(
        ci.get(field) is None
        for field in ("repository", "run_id", "run_attempt", "job")
    )
    actions = (
        ci.get("repository") == "vadv/PgKronika"
        and isinstance(ci.get("run_id"), str)
        and bool(re.fullmatch(r"[1-9][0-9]*", ci["run_id"]))
        and isinstance(ci.get("run_attempt"), str)
        and bool(re.fullmatch(r"[1-9][0-9]*", ci["run_attempt"]))
        and ci.get("job") == "overview-qualification"
    )
    check(
        actions if final else local or actions,
        "CI profile is neither local nor one complete Actions identity",
        failures,
    )


def main() -> int:
    args = arguments()
    decoded = json.loads(args.artifact.read_text(encoding="utf-8"))
    failures: list[str] = []
    warnings: list[str] = []
    artifact = as_mapping(decoded, "qualification artifact", failures)
    repo_root = args.repo_root.resolve()

    top_level_fields = {
        "schema",
        "git_head",
        "git_dirty",
        "generated_unix_ms",
        "ci",
        "host",
        "storage",
        "fixture",
        "accounting",
        "budgets",
        "modes",
        "acceptance",
        "limitations",
    }
    if args.final:
        top_level_fields.add("final_ci")
    exact_fields(artifact, top_level_fields, "qualification artifact", failures)
    check(
        artifact.get("schema") == "pgkronika-overview-qualification-v2",
        "wrong artifact schema",
        failures,
    )
    check(
        isinstance(artifact.get("git_head"), str)
        and re.fullmatch(r"[0-9a-f]{40}", artifact["git_head"]) is not None,
        "artifact git head is not a full lowercase SHA-1",
        failures,
    )
    if args.exact_head:
        check(
            artifact.get("git_head") == args.exact_head,
            "artifact git head differs from requested exact head",
            failures,
        )
    check(
        artifact.get("git_dirty") is False,
        "artifact was generated from a dirty tree",
        failures,
    )
    nonnegative_integer(
        artifact.get("generated_unix_ms"),
        "artifact generation time",
        failures,
        positive=True,
    )
    validate_host(
        as_mapping(artifact.get("host"), "host profile", failures),
        failures,
    )
    validate_fixture(
        as_mapping(artifact.get("fixture"), "fixture profile", failures),
        failures,
    )
    validate_storage(
        as_mapping(artifact.get("storage"), "storage profile", failures),
        failures,
    )
    validate_accounting(
        as_mapping(artifact.get("accounting"), "accounting profile", failures),
        failures,
    )
    validate_modes(
        as_list(artifact.get("modes"), "qualification modes", failures),
        args.final,
        failures,
    )
    validate_acceptance(
        as_list(artifact.get("acceptance"), "acceptance dossier", failures),
        final=args.final,
        repo_root=repo_root,
        failures=failures,
    )
    limitations = as_list(artifact.get("limitations"), "limitations", failures)
    expected_limitations = {
        "storage-cold/page-cache-cold is not measured or claimed",
        "deployment size budgets remain owner-deferred unless both approved values are configured",
        "charts remain owner-deferred and are absent from the qualification datasets",
        "the final PASS is assigned only by the same-head same-attempt CI acceptance job",
    }
    check(
        len(limitations) == len(expected_limitations)
        and set(limitations) == expected_limitations,
        "qualification limitations are incomplete or make a foreign claim",
        failures,
    )
    ci = as_mapping(artifact.get("ci"), "ci profile", failures)
    validate_ci_profile(ci, final=args.final, failures=failures)
    if args.final:
        validate_final_ci(
            artifact,
            raw_artifact=args.raw_artifact,
            repo_root=repo_root,
            failures=failures,
        )
    else:
        check(
            "final_ci" not in artifact,
            "preliminary artifact already contains a final CI decision",
            failures,
        )
    validate_budgets(
        as_mapping(artifact.get("budgets"), "deployment budgets", failures),
        args.final,
        failures,
        warnings,
    )
    artifact_digest = validate_checksum(
        args.artifact,
        args.checksum_file,
        required=args.final,
        failures=failures,
    )

    result = {
        "schema": "pgkronika-overview-qualification-validation-v2",
        "exact_head": artifact.get("git_head"),
        "final": args.final,
        "artifact_sha256": artifact_digest,
        "passed": not failures,
        "failures": failures,
        "warnings": warnings,
    }
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded, encoding="utf-8")
    sys.stdout.write(encoded)
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
