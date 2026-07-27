#!/usr/bin/env python3
"""Validate the production base/candidate coalesced-section measurement."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from decimal import Decimal
from pathlib import Path
from typing import Any


DEFAULT_REPORT = Path("docs/qualification/pgm-coalesced-sections-production-v1.json")
SCHEMA = "pgkronika.pgm-coalesced-sections.production-measurement/v1"
HEX_DIGITS = frozenset("0123456789abcdef")


def fail(message: str) -> None:
    raise SystemExit(f"invalid coalesced-section measurement: {message}")


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def require_sha256(value: Any, name: str) -> str:
    require(
        isinstance(value, str)
        and len(value) == 64
        and set(value).issubset(HEX_DIGITS),
        f"{name} is not a lowercase SHA-256",
    )
    return value


def require_git_oid(value: Any, name: str) -> str:
    require(
        isinstance(value, str)
        and len(value) in (40, 64)
        and set(value).issubset(HEX_DIGITS),
        f"{name} is not a full Git object id",
    )
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(64 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    report_path = Path(sys.argv[1]) if len(sys.argv) == 2 else DEFAULT_REPORT
    if len(sys.argv) > 2:
        fail("usage: validate-pgm-coalesced-sections.py [REPORT.json]")
    data = json.loads(report_path.read_text(encoding="utf-8"))
    require(data.get("schema") == SCHEMA, "schema mismatch")

    runner = data["runner"]
    runner_sha = require_sha256(runner["source_sha256"], "runner.source_sha256")
    runner_commit = require_git_oid(runner["source_commit"], "runner.source_commit")
    repo_root = Path(__file__).resolve().parent.parent
    runner_source = subprocess.run(
        ["git", "show", f"{runner_commit}:{runner['source']}"],
        cwd=repo_root,
        check=True,
        capture_output=True,
    ).stdout
    require(
        hashlib.sha256(runner_source).hexdigest() == runner_sha,
        "runner source hash mismatch",
    )
    require_sha256(runner["base_binary_sha256"], "runner.base_binary_sha256")
    require_sha256(runner["candidate_binary_sha256"], "runner.candidate_binary_sha256")

    fixture = data["fixture"]
    require(fixture["windows"] == 4, "fixture window count")
    require(fixture["rows_per_type_per_window"] == 8, "fixture rows per type/window")
    require(fixture["registered_types"] == 76, "fixture registry count")
    require(fixture["input_physical_sections"] == 312, "input section count")
    require_sha256(fixture["registry_type_id_sha256"], "registry_type_id_sha256")

    journal = fixture["active_parts"]
    require(journal["bytes"] == 1_728_441, "active.parts byte count")
    journal_sha = require_sha256(journal["sha256"], "active.parts.sha256")
    require(journal["parts"] == 4, "active.parts part count")
    require(journal["base_prepare_sha256"] == journal_sha, "base prepare differs")
    require(journal["candidate_prepare_sha256"] == journal_sha, "candidate prepare differs")
    require(journal["byte_identical"], "base/candidate active.parts are not exact")

    expected_logical = fixture["logical"]
    require(expected_logical["data_rows"] == 2_432, "input logical data rows")
    require(expected_logical["dictionary_entries"] == 33, "input dictionary entries")
    require(expected_logical["canonical_stream_bytes"] == 369_513, "canonical stream bytes")
    logical_sha = require_sha256(expected_logical["sha256"], "input logical SHA-256")

    baseline = data["baseline"]
    candidate = data["candidate"]
    for name, measured in (("baseline", baseline), ("candidate", candidate)):
        logical = measured["logical"]
        require(logical["data_rows"] == expected_logical["data_rows"], f"{name} data rows")
        require(
            logical["dictionary_entries"] == expected_logical["dictionary_entries"],
            f"{name} dictionary entries",
        )
        require(
            logical["canonical_stream_bytes"]
            == expected_logical["canonical_stream_bytes"],
            f"{name} canonical stream length",
        )
        require(logical["sha256"] == logical_sha, f"{name} logical digest")
        require(measured["query"]["rows"] == 96, f"{name} query rows")
        require(measured["query"]["gaps"] == 2, f"{name} query gaps")
        require(not measured["query"]["has_next_cursor"], f"{name} query cursor")
        require(measured["query"]["snapshot_units"] == 1, f"{name} snapshot units")
        require(measured["query"]["snapshot_warnings"] == 0, f"{name} snapshot warnings")
        require(measured["query"]["snapshot_damages"] == 0, f"{name} snapshot damages")
        require_sha256(measured["pgm"]["sha256"], f"{name}.pgm.sha256")
        require_sha256(measured["query"]["strace_sha256"], f"{name}.query.strace_sha256")

    require(
        baseline["catalog"]
        == {"sections": 312, "rows": 2468, "data_rows": 2432, "dictionary_rows": 36},
        "baseline catalog inventory",
    )
    require(
        candidate["catalog"]
        == {"sections": 78, "rows": 2465, "data_rows": 2432, "dictionary_rows": 33},
        "candidate catalog inventory",
    )

    comparison = data["comparison"]
    saved = baseline["pgm"]["bytes"] - candidate["pgm"]["bytes"]
    require(comparison["bytes_saved"] == saved, "saved byte count")
    require(comparison["section_reduction"] == "4", "section reduction")
    ratio = Decimal(baseline["pgm"]["bytes"]) / Decimal(candidate["pgm"]["bytes"])
    require(Decimal(comparison["byte_reduction_ratio"]) == ratio, "byte reduction ratio")
    saving = Decimal(saved) * Decimal(100) / Decimal(baseline["pgm"]["bytes"])
    require(Decimal(comparison["saving_percent"]) == saving, "saving percentage")
    require(comparison["logical_bit_exact"], "logical bit-exact result")

    query_io = candidate["query"]["pread64"]
    require(query_io["calls"] <= 16, "candidate query pread64 call ceiling")
    require(query_io["bytes"] <= 150 * 1024, "candidate query pread64 byte ceiling")

    print(
        "validated coalesced-section production measurement: "
        f"{baseline['pgm']['bytes']} -> {candidate['pgm']['bytes']} bytes, "
        f"logical_sha256={logical_sha}"
    )


if __name__ == "__main__":
    main()
