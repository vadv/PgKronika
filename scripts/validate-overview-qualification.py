#!/usr/bin/env python3
"""Validate one exact-head overview qualification artifact."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

MODES = {
    "derived-cold",
    "restart-warm",
    "process-hot",
    "range-cold/facts-warm",
    "live",
    "concurrent-identical",
    "concurrent-disjoint",
    "memory-only",
    "oracle-profile",
}


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--exact-head", default=os.environ.get("GITHUB_SHA"))
    parser.add_argument("--final", action="store_true")
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


def main() -> int:
    args = arguments()
    artifact = json.loads(args.artifact.read_text(encoding="utf-8"))
    failures: list[str] = []
    warnings: list[str] = []

    check(
        artifact.get("schema") == "pgkronika-overview-qualification-v1",
        "wrong artifact schema",
        failures,
    )
    if args.exact_head:
        check(
            artifact.get("git_head") == args.exact_head,
            "artifact git head differs from requested exact head",
            failures,
        )
    check(not artifact.get("git_dirty"), "artifact was generated from a dirty tree", failures)
    check(
        artifact.get("fixture", {}).get("schema_version") == "overview-dense-hour-v1",
        "wrong dense fixture schema",
        failures,
    )
    check(
        artifact.get("fixture", {}).get("source_rows") == 720,
        "dense fixture is not exactly 720 source samples",
        failures,
    )
    check(
        artifact.get("accounting", {}).get("retained_metric_samples", 0) > 0,
        "dense fixture retained no metric samples",
        failures,
    )
    check(
        artifact.get("accounting", {}).get("fixed_metric_stored_bytes", 0) > 0,
        "fixed metric byte accounting is empty",
        failures,
    )
    validate_storage(artifact.get("storage", {}), failures)

    mode_rows = artifact.get("modes", [])
    modes = {row.get("mode"): row for row in mode_rows}
    check(set(modes) == MODES, "artifact does not contain the exact nine modes", failures)
    check(len(mode_rows) == len(MODES), "artifact contains duplicate modes", failures)
    if set(modes) == MODES:
        cold = modes["derived-cold"]
        restart = modes["restart-warm"]
        hot = modes["process-hot"]
        ranged = modes["range-cold/facts-warm"]
        identical = modes["concurrent-identical"]
        disjoint = modes["concurrent-disjoint"]
        memory = modes["memory-only"]

        check(
            restart["work_per_iteration"]["pgm_body_reads"] == 0,
            "restart-warm read PGM bodies",
            failures,
        )
        check(
            hot["work_per_iteration"]["pgm_body_reads"] == 0
            and hot["work_per_iteration"]["sidecar_writes"] == 0,
            "process-hot performed cold I/O or a sidecar write",
            failures,
        )
        check(
            ranged["work_per_iteration"]["pgm_body_reads"] == 0,
            "range-cold/facts-warm read PGM bodies",
            failures,
        )
        check(
            identical["work_per_iteration"]["successful_responses"] == 16,
            "concurrent-identical did not produce 16 responses",
            failures,
        )
        check(
            disjoint["work_per_iteration"]["successful_responses"] == 16,
            "concurrent-disjoint did not produce 16 responses",
            failures,
        )
        check(
            memory["work_per_iteration"]["pgm_body_reads"] == 0,
            "memory-only reread PGM bodies",
            failures,
        )
        if args.final:
            check(
                hot["p95_ns"] * 4 <= cold["p95_ns"],
                "process-hot p95 exceeds 25% of derived-cold",
                failures,
            )
            check(
                restart["p95_ns"] * 4 <= cold["p95_ns"],
                "restart-warm p95 exceeds 25% of derived-cold",
                failures,
            )
            check(
                ranged["p95_ns"] * 2 <= cold["p95_ns"],
                "range-cold/facts-warm p95 exceeds 50% of derived-cold",
                failures,
            )

    acceptance = artifact.get("acceptance", [])
    check(
        [row.get("id") for row in acceptance] == list(range(1, 19)),
        "acceptance dossier is not the exact ordered 18-row set",
        failures,
    )
    ci = artifact.get("ci", {})
    if args.final:
        check(bool(ci.get("run_id")), "final artifact has no CI run ID", failures)
        check(bool(ci.get("run_attempt")), "final artifact has no CI run attempt", failures)
    validate_budgets(artifact.get("budgets", {}), args.final, failures, warnings)

    result = {
        "schema": "pgkronika-overview-qualification-validation-v1",
        "exact_head": artifact.get("git_head"),
        "final": args.final,
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
