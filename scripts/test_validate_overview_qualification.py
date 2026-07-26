#!/usr/bin/env python3
"""Regression tests for the strict M6 artifact/finalization contract."""

from __future__ import annotations

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from types import ModuleType


def load_script(filename: str, module_name: str) -> ModuleType:
    path = Path(__file__).with_name(filename)
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


VALIDATOR = load_script(
    "validate-overview-qualification.py", "overview_qualification_validator"
)
FINALIZER = load_script(
    "finalize-overview-qualification.py", "overview_qualification_finalizer"
)
REPO_ROOT = Path(__file__).resolve().parent.parent


class DeploymentBudgetValidationTests(unittest.TestCase):
    def validate(
        self, budgets: dict[str, object], *, final: bool = True
    ) -> tuple[list[str], list[str]]:
        failures: list[str] = []
        warnings: list[str] = []
        VALIDATOR.validate_budgets(budgets, final, failures, warnings)
        return failures, warnings

    def test_owner_deferred_budgets_do_not_block_final_qualification(self) -> None:
        failures, warnings = self.validate(
            {
                "disk_bytes": None,
                "resident_bytes": None,
                "disk_within_budget": None,
                "resident_within_budget": None,
                "deployment_budget_status": "owner_deferred",
                "qualification_blocked": False,
            }
        )

        self.assertEqual(failures, [])
        self.assertEqual(len(warnings), 1)
        self.assertIn("owner-deferred", warnings[0])

    def test_configured_budgets_must_be_supplied_together(self) -> None:
        failures, _ = self.validate(
            {
                "disk_bytes": 200_000,
                "resident_bytes": None,
                "disk_within_budget": True,
                "resident_within_budget": None,
                "deployment_budget_status": "incomplete_configuration",
                "qualification_blocked": True,
            }
        )

        self.assertIn("dense deployment budgets must be configured together", failures)

    def test_owner_deferred_status_cannot_claim_a_qualification_block(self) -> None:
        failures, _ = self.validate(
            {
                "disk_bytes": None,
                "resident_bytes": None,
                "disk_within_budget": None,
                "resident_within_budget": None,
                "deployment_budget_status": "owner_deferred",
                "qualification_blocked": True,
            }
        )

        self.assertIn("owner-deferred budgets block qualification", failures)

    def test_configured_budgets_pass_only_when_both_measurements_fit(self) -> None:
        failures, warnings = self.validate(
            {
                "disk_bytes": 200_000,
                "resident_bytes": 300_000,
                "disk_within_budget": True,
                "resident_within_budget": True,
                "deployment_budget_status": "within_approved",
                "qualification_blocked": False,
            }
        )

        self.assertEqual(failures, [])
        self.assertEqual(warnings, [])

    def test_configured_budget_excess_blocks_final_qualification(self) -> None:
        failures, _ = self.validate(
            {
                "disk_bytes": 100_000,
                "resident_bytes": 300_000,
                "disk_within_budget": False,
                "resident_within_budget": True,
                "deployment_budget_status": "exceeds_approved",
                "qualification_blocked": True,
            }
        )

        self.assertIn("dense fact file exceeds its approved disk budget", failures)


class StorageValidationTests(unittest.TestCase):
    def test_owned_directory_uses_same_stem_siblings(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_storage(
            {
                "model": "owned-data-directory-sibling-sidecars-v1",
                "active_journal_name": "active.parts",
                "pgm_file_name": "dense-hour.pgm",
                "sidecar_file_name": "dense-hour.ovf",
                "same_stem": True,
            },
            failures,
        )
        self.assertEqual(failures, [])

    def test_different_stems_are_rejected(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_storage(
            {
                "model": "owned-data-directory-sibling-sidecars-v1",
                "active_journal_name": "active.parts",
                "pgm_file_name": "dense-hour.pgm",
                "sidecar_file_name": "other.ovf",
                "same_stem": False,
            },
            failures,
        )
        self.assertIn(
            "qualification files are not same-stem PGM/OVF siblings", failures
        )


def valid_work(mode: str) -> dict[str, int]:
    work = dict.fromkeys(VALIDATOR.WORK_FIELDS, 0)
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
    work["successful_responses"] = expected_responses[mode]
    if mode in {
        "derived-cold",
        "restart-warm",
        "process-hot",
        "range-cold/facts-warm",
        "live",
    }:
        work["serialized_response_bytes"] = 1
    if mode == "derived-cold":
        work.update(
            pgm_body_reads=3,
            pgm_body_bytes=100,
            pgm_sections_decoded=3,
            pgm_rows_decoded=720,
            sidecar_writes=1,
            sidecar_write_bytes=80,
            source_builds=1,
        )
    elif mode == "restart-warm":
        work.update(
            fact_reads=8,
            fact_stored_bytes=80,
            fact_decoded_bytes=100,
            decoded_cache_entries=1,
            decoded_cache_bytes=100,
        )
    elif mode in {"process-hot", "range-cold/facts-warm"}:
        work.update(decoded_cache_entries=1, decoded_cache_bytes=100)
    elif mode == "live":
        work.update(
            pgm_body_reads=4,
            pgm_body_bytes=120,
            pgm_sections_decoded=3,
            pgm_rows_decoded=720,
            sidecar_writes=1,
            sidecar_write_bytes=80,
            source_builds=1,
            completed_active_parts=1,
            visibility_lag_us=100_000,
            tail_pending_from_offset_bytes=100,
            tail_pending_to_offset_bytes=104,
        )
    elif mode == "concurrent-identical":
        work.update(
            pgm_body_reads=3,
            pgm_body_bytes=100,
            pgm_sections_decoded=3,
            pgm_rows_decoded=720,
            sidecar_writes=1,
            sidecar_write_bytes=80,
            source_builds=1,
            singleflight_builds=1,
            singleflight_waiters=15,
            max_inflight_builds=1,
            max_inflight_file_descriptors=4,
        )
    elif mode == "concurrent-disjoint":
        work.update(
            pgm_body_reads=48,
            pgm_body_bytes=1_600,
            pgm_sections_decoded=48,
            pgm_rows_decoded=512,
            sidecar_writes=4,
            sidecar_write_bytes=320,
            source_builds=16,
            singleflight_builds=16,
            max_inflight_builds=4,
            max_inflight_file_descriptors=16,
            max_queue_depth=12,
        )
    elif mode == "memory-only":
        work.update(
            pgm_body_reads=3,
            pgm_body_bytes=100,
            pgm_sections_decoded=3,
            pgm_rows_decoded=720,
            fact_reads=8,
            fact_stored_bytes=80,
            fact_decoded_bytes=100,
            sidecar_writes=1,
            sidecar_write_bytes=80,
            source_builds=1,
            persistence_failures=1,
            publication_attempts=2,
            retry_probes=1,
            fallback_hits=1,
            fallback_resident_entries=1,
            fallback_resident_segment_hours=1,
            fallback_resident_bytes=80,
        )
    elif mode == "oracle-profile":
        work.update(
            pgm_body_reads=3,
            pgm_body_bytes=100,
            pgm_sections_decoded=3,
            pgm_rows_decoded=720,
            fact_reads=8,
            fact_stored_bytes=80,
            fact_decoded_bytes=100,
        )
    return work


def mode_row(mode: str, *, iterations: int = 1) -> dict[str, object]:
    wall_ns = {
        "derived-cold": 100,
        "restart-warm": 20,
        "process-hot": 20,
        "range-cold/facts-warm": 40,
    }.get(mode, 50)
    sample = {
        "wall_ns": wall_ns,
        "cpu_ns": 10,
        "process_peak_rss_bytes": 1024,
        "fd_start": 3,
        "fd_peak": 4,
        "fd_end": 3,
        "proc_io": {
            "rchar": 1,
            "wchar": 1,
            "syscr": 1,
            "syscw": 1,
            "read_bytes": 0,
            "write_bytes": 0,
            "cancelled_write_bytes": 0,
        },
        "work": valid_work(mode),
    }
    samples = [copy.deepcopy(sample) for _ in range(iterations)]
    return {
        "mode": mode,
        "semantics": (
            "fresh child process per sample; OS page cache uncontrolled/warm; "
            "storage-cold false"
        ),
        "iterations": iterations,
        "wall_p50_ns": wall_ns,
        "wall_p95_ns": wall_ns,
        "wall_p99_ns": wall_ns,
        "cpu_p50_ns": 10,
        "cpu_p95_ns": 10,
        "cpu_p99_ns": 10,
        "peak_rss_bytes": 1024,
        "peak_open_file_descriptors": 4,
        "samples": samples,
        "syscalls": {
            "process_scope": True,
            "opens": 1,
            "reads": 1,
            "writes": 1,
            "syncs": 1,
            "renames": 1,
            "unlinks": 1,
            "total_traced": 6,
        },
    }


def compact_mode_row(
    mode: str, *, iterations: int = 1, wall_ns: int | None = None
) -> dict[str, object]:
    if wall_ns is None:
        wall_ns = {
            "derived-cold": 100,
            "restart-warm": 20,
            "process-hot": 20,
            "range-cold/facts-warm": 40,
        }[mode]
    return {
        "mode": mode,
        "iterations": iterations,
        "wall_p50_ns": wall_ns,
        "wall_p95_ns": wall_ns,
        "wall_p99_ns": wall_ns,
        "samples_ns": [wall_ns] * iterations,
    }


def compact_performance_profile(*, iterations: int = 1) -> dict[str, object]:
    return {
        "semantics": (
            "compact sealed facts read + bucket; excludes router, HTTP, JSON, "
            "and server bootstrap"
        ),
        "modes": [
            compact_mode_row(mode, iterations=iterations)
            for mode in VALIDATOR.COMPACT_MODES
        ],
    }


def acceptance_rows(*, final: bool = False) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for index, (requirement, evidence) in enumerate(
        zip(VALIDATOR.REQUIREMENTS, VALIDATOR.EXPECTED_EVIDENCE, strict=True),
        start=1,
    ):
        rows.append(
            {
                "id": index,
                "requirement": requirement,
                "implementation_status": "IMPLEMENTED",
                "evidence": [
                    {
                        "kind": kind,
                        "binary": binary,
                        "path": path,
                        "name": name,
                    }
                    for kind, binary, path, name in evidence
                ],
                "decision": "PASS" if final else "PENDING_EXACT_HEAD_CI",
            }
        )
    return rows


class ModeValidationTests(unittest.TestCase):
    def test_exact_nine_mode_profile_passes_preliminary_validation(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_modes(
            [mode_row(mode) for mode in VALIDATOR.MODES], False, failures
        )
        self.assertEqual(failures, [])

    def test_final_profile_requires_twenty_samples(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_modes(
            [mode_row(mode, iterations=20) for mode in VALIDATOR.MODES],
            True,
            failures,
        )
        self.assertEqual(failures, [])

    def test_endpoint_timings_are_retained_without_compact_ratio_gating(self) -> None:
        rows = [mode_row(mode, iterations=20) for mode in VALIDATOR.MODES]
        restart = rows[1]
        for sample in restart["samples"]:
            sample["wall_ns"] = 80
        for field in ("wall_p50_ns", "wall_p95_ns", "wall_p99_ns"):
            restart[field] = 80
        failures: list[str] = []
        VALIDATOR.validate_modes(rows, True, failures)
        self.assertEqual(failures, [])

    def test_restart_pgm_body_read_is_rejected(self) -> None:
        rows = [mode_row(mode) for mode in VALIDATOR.MODES]
        rows[1]["samples"][0]["work"]["pgm_body_reads"] = 1
        failures: list[str] = []
        VALIDATOR.validate_modes(rows, False, failures)
        self.assertIn("restart-warm read or decoded PGM bodies", failures)

    def test_memory_fallback_and_recovered_restart_must_both_avoid_pgm(self) -> None:
        rows = [mode_row(mode) for mode in VALIDATOR.MODES]
        rows[7]["samples"][0]["work"]["recovered_restart_pgm_body_reads"] = 1
        failures: list[str] = []
        VALIDATOR.validate_modes(rows, False, failures)
        self.assertIn(
            "memory-only fallback or recovered restart read PGM bodies", failures
        )

    def test_live_pending_tail_must_be_the_exact_exposed_range(self) -> None:
        rows = [mode_row(mode) for mode in VALIDATOR.MODES]
        rows[4]["samples"][0]["work"]["tail_pending_to_offset_bytes"] = 105
        failures: list[str] = []
        VALIDATOR.validate_modes(rows, False, failures)
        self.assertIn(
            "live did not retain the exact four-byte incomplete tail", failures
        )


class CompactPerformanceValidationTests(unittest.TestCase):
    def test_preliminary_profile_accepts_one_sample_per_mode(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_compact_performance(
            compact_performance_profile(), False, failures
        )
        self.assertEqual(failures, [])

    def test_final_profile_requires_twenty_samples_and_exact_ratios(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_compact_performance(
            compact_performance_profile(iterations=20), True, failures
        )
        self.assertEqual(failures, [])

    def test_complete_preliminary_profile_enforces_exact_ratios(self) -> None:
        cases = (
            (
                1,
                "restart-warm",
                26,
                "compact restart-warm p95 exceeds 25% of derived-cold",
            ),
            (
                2,
                "process-hot",
                26,
                "compact process-hot p95 exceeds 25% of derived-cold",
            ),
            (
                3,
                "range-cold/facts-warm",
                51,
                "compact range-cold/facts-warm p95 exceeds 50% of derived-cold",
            ),
        )
        for index, mode, wall_ns, expected_failure in cases:
            with self.subTest(mode=mode):
                profile = compact_performance_profile(iterations=20)
                profile["modes"][index] = compact_mode_row(
                    mode, iterations=20, wall_ns=wall_ns
                )
                failures: list[str] = []
                VALIDATOR.validate_compact_performance(profile, False, failures)
                self.assertIn(expected_failure, failures)


class AcceptanceEvidenceTests(unittest.TestCase):
    def test_exact_evidence_manifest_resolves_to_real_tests(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_acceptance(
            acceptance_rows(),
            final=False,
            repo_root=REPO_ROOT,
            failures=failures,
        )
        self.assertEqual(failures, [])

    def test_fabricated_test_name_is_rejected(self) -> None:
        rows = acceptance_rows()
        rows[1]["evidence"][0]["name"] = "fabricated_test"
        failures: list[str] = []
        VALIDATOR.validate_acceptance(
            rows,
            final=False,
            repo_root=REPO_ROOT,
            failures=failures,
        )
        self.assertTrue(
            any("fabricated_test is absent" in failure for failure in failures)
        )
        self.assertIn(
            "acceptance row 2 does not name its exact direct evidence", failures
        )


def raw_finalization_input() -> dict[str, object]:
    return {
        "schema": "pgkronika-overview-qualification-v2",
        "git_head": "a" * 40,
        "git_dirty": False,
        "generated_unix_ms": 1,
        "ci": {
            "repository": "vadv/PgKronika",
            "run_id": "123",
            "run_attempt": "1",
            "job": "overview-qualification",
            "artifact_name": "overview-qualification-raw",
        },
        "host": {},
        "storage": {},
        "fixture": {},
        "accounting": {},
        "budgets": {},
        "modes": [],
        "compact_performance": compact_performance_profile(iterations=20),
        "acceptance": acceptance_rows(),
        "limitations": [],
    }


class FinalizationTests(unittest.TestCase):
    def test_one_attempt_and_six_green_jobs_produce_pass(self) -> None:
        raw = raw_finalization_input()
        jobs = dict.fromkeys(FINALIZER.CI_JOBS, "success")
        final = FINALIZER.final_artifact(
            copy.deepcopy(raw),
            exact_head="a" * 40,
            run_id="123",
            run_attempt="1",
            jobs=jobs,
            raw_digest="b" * 64,
        )
        self.assertEqual(final["final_ci"]["decision"], "PASS")
        self.assertTrue(
            all(row["decision"] == "PASS" for row in final["acceptance"])
        )
        self.assertEqual(len(final["final_ci"]["bdd_scenarios"]), 8)
        self.assertEqual(
            {
                row["path"] for row in final["final_ci"]["bdd_scenarios"]
            },
            {
                "crates/kronika-bdd/features/timeline_overview.feature",
                "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
            },
        )

    def test_attempt_two_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "attempt 1"):
            FINALIZER.final_artifact(
                raw_finalization_input(),
                exact_head="a" * 40,
                run_id="123",
                run_attempt="2",
                jobs=dict.fromkeys(FINALIZER.CI_JOBS, "success"),
                raw_digest="b" * 64,
            )

    def test_foreign_raw_field_is_rejected(self) -> None:
        raw = raw_finalization_input()
        raw["final_ci"] = {}
        with self.assertRaisesRegex(ValueError, "exact v2 schema"):
            FINALIZER.final_artifact(
                raw,
                exact_head="a" * 40,
                run_id="123",
                run_attempt="1",
                jobs=dict.fromkeys(FINALIZER.CI_JOBS, "success"),
                raw_digest="b" * 64,
            )

    def test_non_green_or_incomplete_job_set_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "not all predecessor jobs succeeded"):
            FINALIZER.parse_jobs(
                [
                    *(f"{job}=success" for job in FINALIZER.CI_JOBS[:-1]),
                    f"{FINALIZER.CI_JOBS[-1]}=failure",
                ]
            )
        with self.assertRaisesRegex(ValueError, "exact CI job set required"):
            FINALIZER.parse_jobs(
                [f"{job}=success" for job in FINALIZER.CI_JOBS[:-1]]
            )

    def test_final_ci_checksum_and_raw_identity_are_verified(self) -> None:
        raw = raw_finalization_input()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw_path = root / "raw.json"
            raw_path.write_text(json.dumps(raw), encoding="utf-8")
            final = FINALIZER.final_artifact(
                copy.deepcopy(raw),
                exact_head="a" * 40,
                run_id="123",
                run_attempt="1",
                jobs=dict.fromkeys(FINALIZER.CI_JOBS, "success"),
                raw_digest=VALIDATOR.sha256(raw_path),
            )
            failures: list[str] = []
            VALIDATOR.validate_final_ci(
                final,
                raw_artifact=raw_path,
                repo_root=REPO_ROOT,
                failures=failures,
            )
            self.assertEqual(failures, [])

            final["git_head"] = "c" * 40
            failures = []
            VALIDATOR.validate_final_ci(
                final,
                raw_artifact=raw_path,
                repo_root=REPO_ROOT,
                failures=failures,
            )
            self.assertIn(
                "final CI exact head differs from the artifact head", failures
            )

    def test_missing_real_process_lifecycle_scenario_is_rejected(self) -> None:
        raw = raw_finalization_input()
        final = FINALIZER.final_artifact(
            copy.deepcopy(raw),
            exact_head="a" * 40,
            run_id="123",
            run_attempt="1",
            jobs=dict.fromkeys(FINALIZER.CI_JOBS, "success"),
            raw_digest="b" * 64,
        )
        final["final_ci"]["bdd_scenarios"].pop()
        failures: list[str] = []
        VALIDATOR.validate_final_ci(
            final,
            raw_artifact=None,
            repo_root=REPO_ROOT,
            failures=failures,
        )
        self.assertIn(
            "final artifact does not name the exact PostgreSQL 15-18 timeline and lifecycle scenarios",
            failures,
        )


class AccountingAndChecksumTests(unittest.TestCase):
    def test_exact_dense_accounting_identity_passes(self) -> None:
        failures: list[str] = []
        VALIDATOR.validate_accounting(
            {
                "fact_file_logical_bytes": 100,
                "fact_file_allocated_bytes": 4096,
                "header_and_directory_bytes": 20,
                "stored_block_bytes": 80,
                "decoded_block_bytes": 90,
                "resident_fact_bytes": 120,
                "pinned_fact_bytes": 140,
                "fixed_metric_stored_bytes": 60,
                "variable_event_string_stored_bytes": 10,
                "retained_metric_samples": 12,
                "fixed_metric_bytes_per_sample_numerator": 60,
                "fixed_metric_bytes_per_sample_denominator": 12,
                "identity_holds": True,
            },
            failures,
        )
        self.assertEqual(failures, [])

    def test_checksum_file_must_match_artifact_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "artifact.json"
            checksum = root / "artifact.json.sha256"
            artifact.write_text("{}\n", encoding="utf-8")
            checksum.write_text(f"{VALIDATOR.sha256(artifact)}  artifact.json\n")
            failures: list[str] = []
            VALIDATOR.validate_checksum(
                artifact, checksum, required=True, failures=failures
            )
            self.assertEqual(failures, [])

            checksum.write_text(f"{'0' * 64}  artifact.json\n")
            failures = []
            VALIDATOR.validate_checksum(
                artifact, checksum, required=True, failures=failures
            )
            self.assertIn("artifact checksum does not match its bytes", failures)


if __name__ == "__main__":
    unittest.main()
