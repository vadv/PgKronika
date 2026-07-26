#!/usr/bin/env python3
"""Validate the frozen PGM size-reduction measurement summary."""

from __future__ import annotations

import json
import math
import re
import sys
from pathlib import Path
from typing import Any

SCHEMA = "pgkronika.pgm-size-reduction.measurements/v1"
SHA256 = re.compile(r"^[0-9a-f]{64}$")
DEFAULT_SUMMARY = (
    Path(__file__).resolve().parents[1]
    / "docs"
    / "qualification"
    / "pgm-size-reduction-v1.json"
)


def fail(message: str) -> None:
    raise SystemExit(f"pgm-size-reduction validation failed: {message}")


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def close(actual: float, expected: float, label: str) -> None:
    require(
        math.isclose(actual, expected, rel_tol=1e-11, abs_tol=1e-9),
        f"{label}: expected {expected}, got {actual}",
    )


def nearest_rank(values: list[int | float], percentile: float) -> int | float:
    require(values, "nearest-rank input is empty")
    ordered = sorted(values)
    rank = math.ceil(percentile * len(ordered))
    return ordered[rank - 1]


def walk(value: Any) -> list[Any]:
    values = [value]
    if isinstance(value, dict):
        for child in value.values():
            values.extend(walk(child))
    elif isinstance(value, list):
        for child in value:
            values.extend(walk(child))
    return values


def check_digest_and_path_hygiene(summary: dict[str, Any]) -> None:
    def descend(value: Any, key: str | None = None) -> None:
        if key == "sha256":
            require(isinstance(value, str) and SHA256.fullmatch(value) is not None, "bad SHA256")
        if isinstance(value, str):
            require(not value.startswith("/"), f"absolute path in summary: {value}")
            require("/home/" not in value, f"home path in summary: {value}")
        elif isinstance(value, dict):
            for child_key, child in value.items():
                descend(child, child_key)
        elif isinstance(value, list):
            for child in value:
                descend(child)

    descend(summary)


def check_format_contract(summary: dict[str, Any]) -> None:
    contract = summary["candidate_format"]
    require(
        contract["container"] == "PGM, replaced in place; sealed path remains N.pgm",
        "candidate must remain the single PGM container",
    )
    require(
        contract["compatibility"]
        == "one current writer, one current reader, one contract; prior internal layout is "
        "rejected without migration, fallback or toggle",
        "candidate clean-break contract",
    )
    serialized = json.dumps(summary, sort_keys=True)
    forbidden_parallel_name = "PGM" + "2"
    require(
        forbidden_parallel_name not in serialized,
        "single-container PGM contract",
    )


def check_segment(segment: dict[str, Any]) -> None:
    source = segment["source"]
    candidate = segment["candidate"]
    require(source["bytes"] == source["body_bytes"] + source["framing_bytes"], "source byte sum")
    require(
        candidate["bytes"] == candidate["body_bytes"] + candidate["framing_bytes"],
        "candidate byte sum",
    )
    require(
        source["catalog_rows"] == source["data_rows"] + source["dictionary_rows"],
        "source row sum",
    )
    require(
        candidate["catalog_rows"] == candidate["data_rows"] + candidate["dictionary_ids"],
        "candidate row sum",
    )
    require(source["data_rows"] == candidate["data_rows"], "data rows changed")
    require(
        source["dictionary_rows"] - candidate["dictionary_ids"]
        == segment["dictionary_rows_removed"],
        "dictionary dedup count",
    )
    require(
        candidate["sections"] == candidate["data_families"] + 2,
        "candidate section/family count",
    )
    close(source["bytes"] / candidate["bytes"], segment["ratio"], "segment ratio")
    close(
        100.0 * (1.0 - candidate["bytes"] / source["bytes"]),
        segment["saving_percent"],
        "segment saving",
    )


def check_distribution(summary: dict[str, Any]) -> None:
    segments = summary["natural_full_segments"]
    distribution = summary["natural_full_distribution"]
    require(len(segments) == distribution["count"] == 3, "full-segment count")

    source_bytes = [segment["source"]["bytes"] for segment in segments]
    candidate_bytes = [segment["candidate"]["bytes"] for segment in segments]
    ratios = [segment["ratio"] for segment in segments]
    walls = [segment["encode"]["internal_wall_ns"] for segment in segments]
    cpu = [
        segment["encode"]["user_seconds"] + segment["encode"]["system_seconds"]
        for segment in segments
    ]
    rss = [segment["encode"]["max_rss_kib"] for segment in segments]

    require(sum(source_bytes) == distribution["source_bytes_total"], "source byte total")
    require(sum(candidate_bytes) == distribution["candidate_bytes_total"], "candidate byte total")
    close(
        sum(source_bytes) / sum(candidate_bytes),
        distribution["weighted_ratio"],
        "weighted ratio",
    )
    close(
        100.0 * (1.0 - sum(candidate_bytes) / sum(source_bytes)),
        distribution["saving_percent"],
        "weighted saving",
    )

    for label, values, fields in (
        ("candidate bytes", candidate_bytes, distribution["candidate_bytes"]),
        ("wall", walls, distribution["encode_internal_wall_ns"]),
        ("RSS", rss, distribution["encode_max_rss_kib"]),
    ):
        require(nearest_rank(values, 0.50) == fields["p50"], f"{label} p50")
        require(nearest_rank(values, 0.95) == fields["p95"], f"{label} p95")
        require(max(values) == fields["worst"], f"{label} worst")

    ratio_fields = distribution["ratio"]
    close(float(nearest_rank(ratios, 0.50)), ratio_fields["p50"], "ratio p50")
    close(float(nearest_rank(ratios, 0.95)), ratio_fields["p95"], "ratio p95")
    close(min(ratios), ratio_fields["worst"], "ratio worst")

    cpu_fields = distribution["encode_cpu_seconds"]
    close(float(nearest_rank(cpu, 0.50)), cpu_fields["p50"], "CPU p50")
    close(float(nearest_rank(cpu, 0.95)), cpu_fields["p95"], "CPU p95")
    close(max(cpu), cpu_fields["worst"], "CPU worst")


def check_attribution(summary: dict[str, Any]) -> None:
    attribution = summary["segment_1_attribution"]
    source = attribution["source"]
    require(
        source["bytes"]
        == source["compressed_chunk_bytes"]
        + source["parquet_structural_bytes"]
        + source["framing_bytes"],
        "attribution source byte sum",
    )

    for key in ("data_coalesced_current_like", "dictionary_deduplicated_current_like"):
        item = attribution[key]
        require(
            item["bytes"]
            == item["compressed_chunk_bytes"]
            + item["parquet_structural_bytes"]
            + item["framing_bytes"],
            f"{key} byte sum",
        )
        require(source["bytes"] - item["bytes"] == item["saving_bytes"], f"{key} saving")
        require(
            sum(item["saving_components"].values()) == item["saving_bytes"],
            f"{key} component sum",
        )

    full = attribution["full_coalesced_deduplicated_current_like"]
    require(source["bytes"] - full["bytes"] == full["saving_bytes"], "full structural saving")
    require(
        attribution["data_coalesced_current_like"]["saving_bytes"]
        + attribution["dictionary_deduplicated_current_like"]["saving_bytes"]
        == full["saving_bytes"],
        "data/dictionary additivity",
    )

    previous = full["bytes"]
    incremental = 0
    for profile in attribution["incremental_profiles"]:
        require(profile["before_bytes"] == previous, f"{profile['change']} chain")
        require(
            profile["before_bytes"] - profile["after_bytes"] == profile["saving_bytes"],
            f"{profile['change']} saving",
        )
        component_saving = sum(
            value
            for key, value in profile.items()
            if key.endswith("_saving_bytes") and key != "saving_bytes"
        )
        require(component_saving == profile["saving_bytes"], f"{profile['change']} components")
        previous = profile["after_bytes"]
        incremental += profile["saving_bytes"]

    final = attribution["final"]
    require(previous == final["bytes"], "incremental chain final")
    require(full["bytes"] - final["bytes"] == incremental, "incremental saving sum")
    require(source["bytes"] - final["bytes"] == final["saving_bytes"], "final saving")
    require(sum(final["saving_components"].values()) == final["saving_bytes"], "final components")


def check_query_distributions(summary: dict[str, Any]) -> None:
    query = summary["reader_benchmarks"]["snapshot_query_full_segments"]
    for prefix in ("source_restart", "candidate_restart", "source_query", "candidate_query"):
        values = query[f"{prefix}_mean_ns"]
        fields = query[f"{prefix}_distribution_ns"]
        require(nearest_rank(values, 0.50) == fields["p50"], f"{prefix} p50")
        require(nearest_rank(values, 0.95) == fields["p95"], f"{prefix} p95")
        require(max(values) == fields["worst"], f"{prefix} worst")
    require(
        len(query["rows"]) == len(query["pairwise_page_sha256"]) == 3,
        "query pair inventory",
    )


def check_fixture_tail_and_ovf(summary: dict[str, Any]) -> None:
    tail = summary["natural_tail"]
    check_segment(tail)
    tail_distribution = tail["nominal_candidate_bytes_distribution"]
    require(tail_distribution["count"] == 1, "tail count")
    require(not tail_distribution["population_estimate"], "tail population label")
    require(
        tail_distribution["p50"]
        == tail_distribution["p95"]
        == tail_distribution["worst"]
        == tail["candidate"]["bytes"],
        "tail nominal distribution",
    )

    fixture = summary["all_contract_fixture"]
    require(fixture["source"]["data_rows"] == fixture["candidate"]["data_rows"], "fixture rows")
    require(
        fixture["candidate"]["sections"] == fixture["registered_contracts"] + 2,
        "fixture sections",
    )
    require(
        fixture["candidate"]["data_type_ids"] == fixture["registered_contracts"],
        "fixture type coverage",
    )
    close(
        fixture["source"]["bytes"] / fixture["candidate"]["bytes"],
        fixture["ratio"],
        "fixture ratio",
    )

    mechanism = summary["ovf_interaction"]["successful_two_segment_mechanism_corpus"]
    source = mechanism["source"]
    candidate = mechanism["candidate"]
    require(source["pgm_bytes"] + source["ovf_bytes"] == source["combined_bytes"], "source OVF sum")
    require(
        candidate["pgm_bytes"] + candidate["ovf_bytes"] == candidate["combined_bytes"],
        "candidate OVF sum",
    )
    require(
        source["ovf_bytes"] - candidate["ovf_bytes"] == mechanism["ovf_reduction_bytes"],
        "OVF reduction",
    )
    close(source["pgm_bytes"] / candidate["pgm_bytes"], mechanism["pgm_ratio"], "OVF PGM ratio")
    close(
        source["combined_bytes"] / candidate["combined_bytes"],
        mechanism["combined_ratio"],
        "OVF combined ratio",
    )


def validate(path: Path) -> None:
    try:
        summary = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(str(error))
    require(summary.get("schema") == SCHEMA, "unknown schema")
    check_digest_and_path_hygiene(summary)
    check_format_contract(summary)
    for segment in summary["natural_full_segments"]:
        check_segment(segment)
    check_distribution(summary)
    check_attribution(summary)
    check_query_distributions(summary)
    check_fixture_tail_and_ovf(summary)
    require(len(walk(summary)) < 10000, "summary unexpectedly large")


def main() -> None:
    require(len(sys.argv) <= 2, "usage: validate-pgm-size-reduction.py [SUMMARY.json]")
    path = Path(sys.argv[1]) if len(sys.argv) == 2 else DEFAULT_SUMMARY
    validate(path)
    print(f"validated {path}: {SCHEMA}")


if __name__ == "__main__":
    main()
