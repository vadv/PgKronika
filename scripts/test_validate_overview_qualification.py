#!/usr/bin/env python3
"""Regression tests for deployment-budget qualification semantics."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from types import ModuleType


def load_validator() -> ModuleType:
    path = Path(__file__).with_name("validate-overview-qualification.py")
    spec = importlib.util.spec_from_file_location("overview_qualification_validator", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


VALIDATOR = load_validator()


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


if __name__ == "__main__":
    unittest.main()
