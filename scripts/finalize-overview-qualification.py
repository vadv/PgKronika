#!/usr/bin/env python3
"""Bind one raw M6 qualification artifact to one green Actions attempt."""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from pathlib import Path

CI_JOBS = (
    "lint",
    "deps",
    "test",
    "coverage",
    "overview-qualification",
    "bdd-matrix",
)

BDD_SCENARIOS = [
    *[
        {
            "path": "crates/kronika-bdd/features/timeline_overview.feature",
            "name": f"PostgreSQL {version} publishes one reconciled source-scoped timeline",
            "postgres": version,
        }
        for version in range(15, 19)
    ],
    *[
        {
            "path": "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
            "name": (
                f"PostgreSQL {version} real web process recovers sibling indexes "
                "across lifecycle boundaries"
            ),
            "postgres": version,
        }
        for version in range(15, 19)
    ],
]

RAW_FIELDS = {
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


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw_artifact", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--exact-head", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", required=True)
    parser.add_argument(
        "--job",
        action="append",
        default=[],
        metavar="NAME=RESULT",
        help="one required predecessor job result",
    )
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_jobs(values: list[str]) -> dict[str, str]:
    jobs: dict[str, str] = {}
    for value in values:
        name, separator, result = value.partition("=")
        if not separator or not name or not result:
            raise ValueError(f"invalid --job value: {value!r}")
        if name in jobs:
            raise ValueError(f"duplicate --job value: {name}")
        jobs[name] = result
    if set(jobs) != set(CI_JOBS):
        missing = sorted(set(CI_JOBS) - set(jobs))
        foreign = sorted(set(jobs) - set(CI_JOBS))
        raise ValueError(
            f"exact CI job set required; missing={missing}, foreign={foreign}"
        )
    failed = {name: result for name, result in jobs.items() if result != "success"}
    if failed:
        raise ValueError(f"not all predecessor jobs succeeded: {failed}")
    return jobs


def final_artifact(
    raw: dict[str, object],
    *,
    exact_head: str,
    run_id: str,
    run_attempt: str,
    jobs: dict[str, str],
    raw_digest: str,
) -> dict[str, object]:
    if set(raw) != RAW_FIELDS:
        raise ValueError("raw artifact does not match the exact v2 schema")
    if raw.get("schema") != "pgkronika-overview-qualification-v2":
        raise ValueError("raw artifact has the wrong schema")
    if raw.get("git_head") != exact_head:
        raise ValueError("raw artifact head differs from the requested exact head")
    if raw.get("git_dirty") is not False:
        raise ValueError("raw artifact came from a dirty tree")
    if run_attempt != "1":
        raise ValueError("M6 finalization requires Actions attempt 1")
    ci = raw.get("ci")
    if not isinstance(ci, dict):
        raise ValueError("raw artifact has no CI profile")
    if set(ci) != {
        "repository",
        "run_id",
        "run_attempt",
        "job",
        "artifact_name",
    }:
        raise ValueError("raw artifact has the wrong CI profile schema")
    expected_ci = {
        "repository": "vadv/PgKronika",
        "run_id": run_id,
        "run_attempt": run_attempt,
        "job": "overview-qualification",
        "artifact_name": "overview-qualification-raw",
    }
    for field, expected in expected_ci.items():
        if ci.get(field) != expected:
            raise ValueError(
                f"raw CI field {field!r} is {ci.get(field)!r}, expected {expected!r}"
            )

    acceptance = raw.get("acceptance")
    if not isinstance(acceptance, list) or len(acceptance) != 18:
        raise ValueError("raw artifact does not have the exact 18-row dossier")
    for index, row in enumerate(acceptance, start=1):
        if (
            not isinstance(row, dict)
            or set(row)
            != {
                "id",
                "requirement",
                "implementation_status",
                "evidence",
                "decision",
            }
            or row.get("id") != index
            or row.get("implementation_status") != "IMPLEMENTED"
            or row.get("decision") != "PENDING_EXACT_HEAD_CI"
        ):
            raise ValueError(f"raw acceptance row {index} is not a pending implementation")
        row["decision"] = "PASS"

    raw["final_ci"] = {
        "schema": "pgkronika-overview-qualification-final-ci-v1",
        "exact_head": exact_head,
        "run_id": run_id,
        "run_attempt": run_attempt,
        "acceptance_job": "overview-m6-acceptance",
        "jobs": jobs,
        "bdd_scenarios": [dict(row) for row in BDD_SCENARIOS],
        "raw_artifact_sha256": raw_digest,
        "finalized_unix_ms": time.time_ns() // 1_000_000,
        "decision": "PASS",
    }
    return raw


def main() -> int:
    args = arguments()
    if args.output.exists():
        raise FileExistsError(f"refusing to overwrite {args.output}")
    raw = json.loads(args.raw_artifact.read_text(encoding="utf-8"))
    artifact = final_artifact(
        raw,
        exact_head=args.exact_head,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        jobs=parse_jobs(args.job),
        raw_digest=sha256(args.raw_artifact),
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(artifact, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
