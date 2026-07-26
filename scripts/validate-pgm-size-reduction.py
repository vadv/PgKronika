#!/usr/bin/env python3
"""Validate the frozen PGM size-reduction measurement summary."""

from __future__ import annotations

import hashlib
import json
import math
import re
import sys
from pathlib import Path
from typing import Any

SCHEMA = "pgkronika.pgm-size-reduction.measurements/v1"
RELEASE_ITERATIONS = 20
SHA256 = re.compile(r"^[0-9a-f]{64}$")
ALL_CONTRACT_TYPE_IDS_SHA256 = (
    "afc32c2386a0312906afbdc0931afce1a4fcb02fa148da780860bf9bba5fa231"
)
REGISTRY_CONTRACT_INVENTORY_SHA256 = (
    "bbe3008d578d81a56a996ab4fdc897ae848aa494bf5c181e3c5654fa16977cce"
)
SEGMENT_IDENTITY_FIELDS = (
    "catalog_fields_equal",
    "data_type_sets_equal",
    "canonical_arrow_equal",
    "normalized_dictionary_equal",
    "candidate_reader_valid",
)
FIXTURE_IDENTITY_FIELDS = (
    "catalog_identity_equal",
    "data_type_sets_equal",
    "canonical_arrow_equal",
    "normalized_dictionary_equal",
    "candidate_reader_valid",
)
OWNER_CONTRACT_DOCS = (
    "crates/kronika-format/README.md",
    "crates/kronika-format/README.ru.md",
    "docs/superpowers/plans/2026-07-24-pgm-compaction.md",
    "docs/superpowers/specs/2026-07-26-pgm-size-reduction-research.md",
)
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


def object_without_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


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
        if key is not None and key.endswith("sha256"):
            digests = value if isinstance(value, list) else [value]
            require(digests, f"empty SHA256 inventory: {key}")
            require(
                all(isinstance(digest, str) and SHA256.fullmatch(digest) for digest in digests),
                f"bad SHA256: {key}",
            )
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


def check_owner_contract_text() -> None:
    repository_root = Path(__file__).resolve().parents[1]
    forbidden = (
        "pgm" + "2",
        "17" + "x",
        "17" + "×",
        "17" + " раз",
    )
    for relative_path in OWNER_CONTRACT_DOCS:
        text = (repository_root / relative_path).read_text(encoding="utf-8").casefold()
        for token in forbidden:
            require(
                token not in text,
                f"forbidden claim or parallel-format name in {relative_path}",
            )


def check_format_contract(summary: dict[str, Any]) -> None:
    contract = summary["updated_pgm_contract"]
    require(
        contract["container"] == "PGM, replaced in place; sealed path remains N.pgm",
        "candidate must remain the single PGM container",
    )
    require(
        contract["implementation"]
        == "one writer, one reader and one compact PGM contract",
        "single PGM implementation contract",
    )
    serialized = json.dumps(summary, sort_keys=True)
    forbidden_parallel_name = "PGM" + "2"
    require(
        forbidden_parallel_name not in serialized,
        "single-container PGM contract",
    )
    require(contract["data_page_bytes"] == 1_048_576, "data-page byte target")
    require(contract["data_page_row_limit"] == 65_536, "data-page row limit")
    require(contract["row_group_rows"] == 65_536, "row-group row limit")
    require(contract["column_encoding"] == "PLAIN", "column encoding")
    require(not contract["column_dictionary_enabled"], "Parquet dictionary encoding")
    require(contract["compression"] == "ZSTD", "compression codec")
    require(contract["compression_level"] == 6, "compression level")


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
    require(
        source["data_families"] == candidate["data_families"],
        "data family count changed",
    )
    identity = segment["identity"]
    require(
        set(identity) == set(SEGMENT_IDENTITY_FIELDS),
        "segment identity proof fields",
    )
    require(
        all(identity[field] for field in SEGMENT_IDENTITY_FIELDS),
        "segment identity proof",
    )
    for label, item in (("source", source), ("candidate", candidate)):
        require(
            item["parquet_structural_bytes"]
            == item["footer_bytes"]
            + item["column_index_bytes"]
            + item["offset_index_bytes"],
            f"{label} structural-byte sum",
        )
        require(item["sections"] > 0, f"{label} has no sections")
    require(candidate["column_index_bytes"] == 0, "candidate column index")
    require(candidate["offset_index_bytes"] == 0, "candidate offset index")
    encode = segment["encode"]
    require(encode["getrusage_filesystem_inputs"] == 0, "encode input counter")
    require(encode["getrusage_filesystem_outputs"] >= 0, "encode output counter")
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
    require(len({segment["name"] for segment in segments}) == 3, "duplicate segment name")
    require(
        len({segment["source"]["sha256"] for segment in segments}) == 3,
        "duplicate source digest",
    )
    require(
        len({segment["candidate"]["sha256"] for segment in segments}) == 3,
        "duplicate candidate digest",
    )

    source_bytes = [segment["source"]["bytes"] for segment in segments]
    candidate_bytes = [segment["candidate"]["bytes"] for segment in segments]
    ratios = [segment["ratio"] for segment in segments]
    walls = [segment["encode"]["internal_wall_ns"] for segment in segments]
    process_walls = [
        segment["encode"]["process_wall_seconds"] for segment in segments
    ]
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

    process_wall_fields = distribution["encode_process_wall_seconds"]
    close(
        float(nearest_rank(process_walls, 0.50)),
        process_wall_fields["p50"],
        "process wall p50",
    )
    close(
        float(nearest_rank(process_walls, 0.95)),
        process_wall_fields["p95"],
        "process wall p95",
    )
    close(max(process_walls), process_wall_fields["worst"], "process wall worst")

    cpu_fields = distribution["encode_cpu_seconds"]
    close(float(nearest_rank(cpu, 0.50)), cpu_fields["p50"], "CPU p50")
    close(float(nearest_rank(cpu, 0.95)), cpu_fields["p95"], "CPU p95")
    close(max(cpu), cpu_fields["worst"], "CPU worst")

    deterministic = segments[2]["determinism"]
    require(deterministic["repetitions"] == 2, "natural determinism repetitions")
    require(deterministic["byte_identical"], "natural determinism bytes")
    require(
        deterministic["sha256"] == segments[2]["candidate"]["sha256"],
        "natural determinism digest",
    )


def check_attribution(summary: dict[str, Any]) -> None:
    attribution = summary["segment_1_attribution"]
    source = attribution["source"]
    require(
        source["sha256"] == summary["natural_full_segments"][0]["source"]["sha256"],
        "attribution source digest",
    )
    require(
        source["bytes"]
        == source["compressed_chunk_bytes"]
        + source["parquet_structural_bytes"]
        + source["framing_bytes"],
        "attribution source byte sum",
    )

    for key in ("data_coalesced_current_like", "dictionary_deduplicated_current_like"):
        item = attribution[key]
        require(SHA256.fullmatch(item["sha256"]) is not None, f"{key} digest")
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
    require(SHA256.fullmatch(full["sha256"]) is not None, "full current-like digest")
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
            SHA256.fullmatch(profile["after_sha256"]) is not None,
            f"{profile['change']} digest",
        )
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
    require(
        final["sha256"] == summary["natural_full_segments"][0]["candidate"]["sha256"],
        "attribution final digest",
    )
    require(
        attribution["incremental_profiles"][-1]["after_sha256"] == final["sha256"],
        "incremental final digest",
    )
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


def check_fault_corpus(summary: dict[str, Any]) -> None:
    cases = summary["dictionary_and_fault_cases"]
    actual_cases = {case["case"]: case["result"] for case in cases}
    expected_cases = {
        "identical duplicate across sections": "accepted and deduplicated",
        "reversed dictionary section order": "byte-identical candidate",
        "same ID with different value": "rejected before output publication",
        "same ID in strings and blobs with incompatible representation": (
            "rejected before output publication"
        ),
        "descending IDs": "rejected by reader and research encoder",
        "same-section duplicate ID": "rejected by reader and research encoder",
        "section body corruption": "rejected by section CRC32C",
        "catalog corruption": "rejected by catalog CRC32C",
        "one-byte tail truncation": "rejected by tail-index validation",
    }
    require(len(cases) == len(actual_cases), "duplicate fault case")
    require(actual_cases == expected_cases, "fault-case results")

    files = summary["fault_corpus_files"]
    expected_file_names = {
        "corrupt-catalog.pgm",
        "corrupt-section-body.pgm",
        "dictionary-conflicting-duplicate.pgm",
        "dictionary-order-a.compact.pgm",
        "dictionary-order-a.pgm",
        "dictionary-order-b.compact.pgm",
        "dictionary-order-b.pgm",
        "dictionary-placement-conflict.pgm",
        "dictionary-row-order-invalid.pgm",
        "dictionary-same-section-duplicate.pgm",
        "truncated-tail.pgm",
    }
    actual_file_names = {file["name"] for file in files}
    require(len(files) == len(actual_file_names), "duplicate fault file")
    require(actual_file_names == expected_file_names, "fault-corpus file inventory")
    for file in files:
        require(file["bytes"] > 0, "empty fault file")
        require(SHA256.fullmatch(file["sha256"]) is not None, "fault-file digest")
    by_name = {file["name"]: file for file in files}
    require(
        by_name["dictionary-order-a.compact.pgm"]["sha256"]
        == by_name["dictionary-order-b.compact.pgm"]["sha256"],
        "dictionary-order candidate digest",
    )
    require(
        by_name["dictionary-order-a.compact.pgm"]["bytes"]
        == by_name["dictionary-order-b.compact.pgm"]["bytes"],
        "dictionary-order candidate length",
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
    fixture_type_ids = fixture["data_type_ids"]
    require(
        len(fixture_type_ids) == fixture["registered_contracts"],
        "fixture type inventory length",
    )
    require(
        fixture_type_ids == sorted(fixture_type_ids),
        "fixture type inventory order",
    )
    require(
        len(set(fixture_type_ids)) == len(fixture_type_ids),
        "fixture type inventory uniqueness",
    )
    fixture_type_ids_sha256 = hashlib.sha256(
        "".join(f"{type_id}\n" for type_id in fixture_type_ids).encode()
    ).hexdigest()
    require(
        fixture_type_ids_sha256
        == fixture["data_type_ids_sha256"]
        == ALL_CONTRACT_TYPE_IDS_SHA256,
        "fixture type inventory digest",
    )
    require(
        fixture["registry_contract_inventory_sha256"]
        == REGISTRY_CONTRACT_INVENTORY_SHA256,
        "fixture registry inventory digest",
    )
    require(fixture["source"]["data_rows"] == fixture["candidate"]["data_rows"], "fixture rows")
    require(
        fixture["candidate"]["sections"] == fixture["registered_contracts"] + 2,
        "fixture sections",
    )
    require(
        fixture["candidate"]["data_type_id_count"] == fixture["registered_contracts"],
        "fixture type coverage",
    )
    require(fixture["source_id"] != 0, "fixture must exercise non-zero source_id")
    require(fixture["min_ts_us"] < fixture["max_ts_us"], "fixture timestamp range")
    for field in FIXTURE_IDENTITY_FIELDS:
        require(fixture[field], f"fixture {field}")
    deterministic = fixture["determinism"]
    require(deterministic["repetitions"] == 2, "fixture determinism repetitions")
    require(deterministic["byte_identical"], "fixture determinism bytes")
    require(
        deterministic["sha256"] == fixture["candidate"]["sha256"],
        "fixture determinism digest",
    )
    close(
        fixture["source"]["bytes"] / fixture["candidate"]["bytes"],
        fixture["ratio"],
        "fixture ratio",
    )

    ovf = summary["ovf_interaction"]
    natural = ovf["natural_46_minute_corpus"]
    require(natural["source_result"] == natural["candidate_result"], "natural OVF result")
    require(not natural["ovf_bytes_reported"], "natural OVF bytes must remain unknown")

    mechanism = ovf["successful_two_segment_mechanism_corpus"]
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
    for label, item in (("source", source), ("candidate", candidate)):
        files = item["files"]
        require(len(files) == 2, f"{label} OVF inventory")
        require(
            sum(file["pgm_bytes"] for file in files) == item["pgm_bytes"],
            f"{label} per-file PGM sum",
        )
        require(
            sum(file["ovf_bytes"] for file in files) == item["ovf_bytes"],
            f"{label} per-file OVF sum",
        )
        require(
            [file["source_manifest_items"] for file in files]
            == item["source_manifest_items"],
            f"{label} SourceManifest inventory",
        )
        for file in files:
            require(SHA256.fullmatch(file["pgm_sha256"]) is not None, f"{label} PGM digest")
            require(SHA256.fullmatch(file["ovf_sha256"]) is not None, f"{label} OVF digest")
    require(
        [file["name"] for file in source["files"]]
        == [file["name"] for file in candidate["files"]],
        "OVF source/candidate stems",
    )
    require(mechanism["all_non_manifest_fact_blocks_equal"], "OVF fact blocks changed")
    require(mechanism["ovf_codec"] == "None", "OVF codec")
    close(source["pgm_bytes"] / candidate["pgm_bytes"], mechanism["pgm_ratio"], "OVF PGM ratio")
    close(
        source["combined_bytes"] / candidate["combined_bytes"],
        mechanism["combined_ratio"],
        "OVF combined ratio",
    )


def validate(path: Path) -> None:
    try:
        summary = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=object_without_duplicate_keys,
        )
    except (OSError, json.JSONDecodeError, ValueError) as error:
        fail(str(error))
    try:
        require(isinstance(summary, dict), "summary root must be an object")
        require(summary.get("schema") == SCHEMA, "unknown schema")
        check_digest_and_path_hygiene(summary)
        check_owner_contract_text()
        check_format_contract(summary)
        for segment in summary["natural_full_segments"]:
            check_segment(segment)
        check_distribution(summary)
        check_attribution(summary)
        check_query_distributions(summary)
        check_fault_corpus(summary)
        check_fixture_tail_and_ovf(summary)
        require(len(walk(summary)) < 10000, "summary unexpectedly large")
    except (KeyError, TypeError, ValueError) as error:
        fail(f"invalid summary structure: {error}")


def check_release_pgm_profile(artifact: dict[str, Any]) -> None:
    profile = artifact["pgm_compaction"]
    require(
        set(profile)
        == {
            "production_path",
            "output_name",
            "input_parts",
            "registered_layouts",
            "all_family_exact_equality",
            "seal_iterations",
            "seal_wall_p50_ns",
            "seal_wall_p95_ns",
            "seal_wall_p99_ns",
            "seal_cpu_p50_ns",
            "seal_cpu_p95_ns",
            "seal_cpu_p99_ns",
            "seal_peak_rss_bytes",
            "samples",
        },
        "release PGM profile fields",
    )
    require(
        profile["production_path"]
        == "kronika_writer::Journal -> seal -> kronika_reader::PgmUnit",
        "release measurement bypasses the production writer or reader",
    )
    require(
        profile["output_name"] == "1000000.pgm",
        "release output is not the timestamp-named PGM",
    )
    require(profile["input_parts"] == 40, "release spill fixture input count")
    require(profile["registered_layouts"] == 75, "registered layout count")

    exact = profile["all_family_exact_equality"]
    require(
        exact
        == {
            "gate_passed": True,
            "fixture_classes": [
                "dense",
                "reset-heavy",
                "nullable-heavy",
                "short-tail",
            ],
            "test_binary": "kronika-reader::pgm_compaction_oracle",
            "test_path": "crates/kronika-reader/tests/pgm_compaction_oracle.rs",
            "roundtrip_test": (
                "every_registered_layout_roundtrips_all_fixture_classes_exactly"
            ),
            "determinism_test": (
                "all_layout_bytes_are_deterministic_under_window_reordering"
            ),
        },
        "all-family exact-equality gate identity",
    )
    repository_root = Path(__file__).resolve().parents[1]
    oracle = (repository_root / exact["test_path"]).read_text(encoding="utf-8")
    require(exact["roundtrip_test"] in oracle, "all-family roundtrip test is absent")
    require(exact["determinism_test"] in oracle, "all-family determinism test is absent")

    samples = profile["samples"]
    require(
        isinstance(samples, list)
        and len(samples) == RELEASE_ITERATIONS
        and profile["seal_iterations"] == len(samples),
        f"release seal requires exactly {RELEASE_ITERATIONS} samples",
    )
    wall: list[int] = []
    cpu: list[int] = []
    digests: set[str] = set()
    pgm_lengths: set[int] = set()
    for sample in samples:
        require(
            set(sample)
            == {
                "wall_ns",
                "cpu_ns",
                "process_peak_rss_bytes",
                "proc_io",
                "source_journal_bytes",
                "pgm_logical_bytes",
                "pgm_allocated_bytes",
                "spill_bytes",
                "writer_write_bytes",
                "write_amplification_numerator",
                "write_amplification_denominator",
                "admitted_memory_bytes",
                "sections",
                "rows",
                "pgm_sha256",
                "exact_source_facts_equal",
                "reader_reopen_equal",
            },
            "release PGM sample fields",
        )
        for field in (
            "wall_ns",
            "cpu_ns",
            "process_peak_rss_bytes",
            "source_journal_bytes",
            "pgm_logical_bytes",
            "pgm_allocated_bytes",
            "spill_bytes",
            "writer_write_bytes",
            "write_amplification_numerator",
            "write_amplification_denominator",
            "admitted_memory_bytes",
            "sections",
            "rows",
        ):
            require(
                isinstance(sample[field], int) and sample[field] > 0,
                f"release PGM sample {field}",
            )
        require(sample["sections"] == 3, "release PGM section count")
        require(sample["rows"] == 1_441, "release PGM row count")
        require(
            sample["spill_bytes"] + sample["pgm_logical_bytes"]
            == sample["writer_write_bytes"],
            "writer bytes are not spill plus PGM",
        )
        require(
            sample["write_amplification_numerator"] == sample["writer_write_bytes"]
            and sample["write_amplification_denominator"]
            == sample["pgm_logical_bytes"],
            "write-amplification rational",
        )
        require(
            sample["pgm_allocated_bytes"] >= sample["pgm_logical_bytes"],
            "allocated PGM bytes are smaller than logical bytes",
        )
        require(
            sample["source_journal_bytes"] > sample["pgm_logical_bytes"],
            "release fixture does not demonstrate compaction",
        )
        require(
            sample["exact_source_facts_equal"] is True
            and sample["reader_reopen_equal"] is True,
            "production PGM semantic or reopen equality",
        )
        require(SHA256.fullmatch(sample["pgm_sha256"]) is not None, "release PGM digest")
        proc_io = sample["proc_io"]
        require(
            set(proc_io)
            == {
                "rchar",
                "wchar",
                "syscr",
                "syscw",
                "read_bytes",
                "write_bytes",
                "cancelled_write_bytes",
            },
            "release seal process I/O fields",
        )
        require(
            all(isinstance(value, int) and value >= 0 for value in proc_io.values()),
            "release seal process I/O values",
        )
        require(
            proc_io["wchar"] >= sample["writer_write_bytes"]
            and proc_io["syscw"] > 0
            and proc_io["write_bytes"] > 0,
            "seal process write accounting is below writer bytes",
        )
        wall.append(sample["wall_ns"])
        cpu.append(sample["cpu_ns"])
        digests.add(sample["pgm_sha256"])
        pgm_lengths.add(sample["pgm_logical_bytes"])

    require(len(digests) == len(pgm_lengths) == 1, "release PGM is not deterministic")
    require(
        profile["seal_wall_p50_ns"] == nearest_rank(wall, 0.50)
        and profile["seal_wall_p95_ns"] == nearest_rank(wall, 0.95)
        and profile["seal_wall_p99_ns"] == nearest_rank(wall, 0.99),
        "seal wall distributions",
    )
    require(
        profile["seal_cpu_p50_ns"] == nearest_rank(cpu, 0.50)
        and profile["seal_cpu_p95_ns"] == nearest_rank(cpu, 0.95)
        and profile["seal_cpu_p99_ns"] == nearest_rank(cpu, 0.99),
        "seal CPU distributions",
    )
    require(
        profile["seal_peak_rss_bytes"]
        == max(sample["process_peak_rss_bytes"] for sample in samples),
        "seal RSS maximum",
    )


def check_release_storage_and_queries(artifact: dict[str, Any]) -> None:
    fixture = artifact["fixture"]
    accounting = artifact["accounting"]
    seal_samples = artifact["pgm_compaction"]["samples"]
    require(
        fixture["schema_version"] == "overview-dense-hour-v3",
        "release fixture schema",
    )
    require(
        accounting["pgm_logical_bytes"] == fixture["source_bytes"],
        "profile PGM bytes differ from the production fixture",
    )
    require(
        all(
            sample["pgm_logical_bytes"] == accounting["pgm_logical_bytes"]
            for sample in seal_samples
        ),
        "timestamp-named PGM bytes differ from the query and OVF source",
    )
    require(
        accounting["ovf_logical_bytes"] == accounting["fact_file_logical_bytes"]
        and accounting["ovf_allocated_bytes"]
        == accounting["fact_file_allocated_bytes"],
        "OVF bytes differ from the fact file",
    )
    require(
        accounting["combined_logical_bytes"]
        == accounting["pgm_logical_bytes"] + accounting["ovf_logical_bytes"]
        and accounting["combined_allocated_bytes"]
        == accounting["pgm_allocated_bytes"] + accounting["ovf_allocated_bytes"],
        "PGM plus OVF byte totals",
    )
    require(
        accounting["pgm_allocated_bytes"] >= accounting["pgm_logical_bytes"]
        and accounting["ovf_allocated_bytes"] >= accounting["ovf_logical_bytes"],
        "allocated release storage bytes",
    )

    modes = {mode["mode"]: mode for mode in artifact["modes"]}
    require(
        set(modes)
        == {
            "derived-cold",
            "restart-warm",
            "process-hot",
            "range-cold/facts-warm",
            "live",
            "concurrent-identical",
            "concurrent-disjoint",
            "memory-only",
            "oracle-profile",
        },
        "release query mode inventory",
    )
    for name in ("derived-cold", "restart-warm", "process-hot", "oracle-profile"):
        mode = modes[name]
        require(
            mode["iterations"] == RELEASE_ITERATIONS
            and len(mode["samples"]) == RELEASE_ITERATIONS,
            f"{name} release sample count",
        )
        require(
            mode["wall_p50_ns"] > 0
            and mode["wall_p95_ns"] > 0
            and mode["cpu_p50_ns"] >= 0
            and mode["peak_rss_bytes"] > 0,
            f"{name} latency/resource measurement",
        )
        require(mode["samples"], f"{name} samples")
        for sample in mode["samples"]:
            require(
                sample["proc_io"]["rchar"] > 0 and sample["proc_io"]["syscr"] > 0,
                f"{name} process I/O measurement",
            )
    derived = modes["derived-cold"]["samples"]
    require(
        all(
            sample["work"]["pgm_body_reads"] > 0
            and sample["work"]["pgm_body_bytes"] > 0
            and sample["work"]["sidecar_writes"] == 1
            and sample["work"]["sidecar_write_bytes"]
            == accounting["ovf_logical_bytes"]
            for sample in derived
        ),
        "derived-cold does not measure production PGM to OVF",
    )
    restart = modes["restart-warm"]["samples"]
    require(
        all(
            sample["work"]["pgm_body_reads"] == 0
            and sample["work"]["fact_reads"] > 0
            and sample["work"]["fact_stored_bytes"] > 0
            and sample["work"]["sidecar_writes"] == 0
            for sample in restart
        ),
        "restart-warm does not measure OVF reuse separately",
    )
    hot = modes["process-hot"]["samples"]
    require(
        all(
            sample["work"]["pgm_body_reads"] == 0
            and sample["work"]["fact_reads"] == 0
            and sample["work"]["sidecar_writes"] == 0
            for sample in hot
        ),
        "process-hot query performed storage I/O",
    )
    oracle = modes["oracle-profile"]["samples"]
    require(
        all(
            sample["work"]["pgm_body_reads"] > 0
            and sample["work"]["fact_reads"] > 0
            and sample["work"]["successful_responses"] == 2
            for sample in oracle
        ),
        "oracle profile does not compare raw PGM and OVF query paths",
    )


def validate_release(path: Path, exact_head: str | None) -> None:
    try:
        artifact = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=object_without_duplicate_keys,
        )
    except (OSError, json.JSONDecodeError, ValueError) as error:
        fail(str(error))
    try:
        require(isinstance(artifact, dict), "release artifact root must be an object")
        require(
            artifact.get("schema") == "pgkronika-overview-parity-v1-evidence-v3",
            "unknown release artifact schema",
        )
        require(artifact.get("git_dirty") is False, "release artifact came from a dirty tree")
        head = artifact.get("git_head")
        require(
            isinstance(head, str) and re.fullmatch(r"[0-9a-f]{40}", head) is not None,
            "release artifact git head",
        )
        if exact_head is not None:
            require(head == exact_head, "release artifact head differs from requested exact head")
        check_digest_and_path_hygiene(artifact)
        check_release_pgm_profile(artifact)
        check_release_storage_and_queries(artifact)
    except (KeyError, TypeError, ValueError) as error:
        fail(f"invalid release artifact structure: {error}")


def main() -> None:
    if len(sys.argv) >= 2 and sys.argv[1] == "--release":
        require(
            3 <= len(sys.argv) <= 4,
            "usage: validate-pgm-size-reduction.py --release ARTIFACT.json [EXACT_HEAD]",
        )
        path = Path(sys.argv[2])
        exact_head = sys.argv[3] if len(sys.argv) == 4 else None
        validate_release(path, exact_head)
        print(f"validated production PGM release artifact {path}")
        return
    require(len(sys.argv) <= 2, "usage: validate-pgm-size-reduction.py [SUMMARY.json]")
    path = Path(sys.argv[1]) if len(sys.argv) == 2 else DEFAULT_SUMMARY
    validate(path)
    print(f"validated {path}: {SCHEMA}")


if __name__ == "__main__":
    main()
