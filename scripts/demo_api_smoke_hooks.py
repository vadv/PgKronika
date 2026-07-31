"""Schemathesis fixture overrides and lightweight proof-of-data checks."""

from __future__ import annotations

import json
import os
from typing import Any, Callable

import schemathesis


CONTEXT = json.loads(os.environ["PG_KRONIKA_SMOKE_CONTEXT"])
EXPECTED = {
    "GET /v1/version",
    "GET /v1/sections",
    "GET /v1/segments",
    "GET /v1/section/{name}",
    "GET /v1/sections/batch",
    "GET /v1/section/{name}/diff",
    "GET /v1/sections/batch/diff",
    "GET /v1/timeline/overview",
    "GET /v1/timeline/events",
    "GET /v1/timeline/health",
    "GET /v1/timeline/heatmap",
    "GET /v1/timeline/spine",
    "GET /v1/frame/{view}",
    "GET /v1/ui/catalog",
    "GET /v1/ui/context",
    "GET /v1/data/quality",
    "GET /v1/entity/{view}/{entity}",
    "GET /v1/storage",
    "GET /v1/views/summary",
    "GET /v1/anomalies",
    "GET /v1/incidents",
}


def _queries(label: str) -> dict[str, Any]:
    ranged = {"from": CONTEXT["from"], "to": CONTEXT["to"]}
    return {
        "GET /v1/version": {},
        "GET /v1/sections": {},
        "GET /v1/segments": ranged,
        "GET /v1/section/{name}": {**ranged, "limit": 100},
        "GET /v1/sections/batch": {
            **ranged,
            "names": CONTEXT["names"],
            "limit": 100,
        },
        "GET /v1/section/{name}/diff": ranged,
        "GET /v1/sections/batch/diff": {**ranged, "names": CONTEXT["names"]},
        "GET /v1/timeline/overview": ranged,
        "GET /v1/timeline/events": {**ranged, "limit": 100},
        "GET /v1/timeline/health": {**ranged, "step": CONTEXT["health_step_us"]},
        "GET /v1/timeline/heatmap": {
            **ranged,
            "view": CONTEXT["view"],
            "metric": CONTEXT["metric"],
            "buckets": 8,
            "top": 8,
        },
        "GET /v1/timeline/spine": {**ranged, "buckets": 8},
        "GET /v1/frame/{view}": {
            "at": CONTEXT["at"],
            "limit": 1,
            **(
                {"preset": CONTEXT["frame_preset"]}
                if CONTEXT["frame_preset"] is not None
                else {}
            ),
        },
        "GET /v1/ui/catalog": {},
        "GET /v1/ui/context": {"at": CONTEXT["at"]},
        "GET /v1/data/quality": ranged,
        "GET /v1/entity/{view}/{entity}": {"at": CONTEXT["at"]},
        "GET /v1/storage": {},
        "GET /v1/views/summary": {"at": CONTEXT["at"]},
        "GET /v1/anomalies": {
            **ranged,
            "window": CONTEXT["window"],
            "step": CONTEXT["step"],
            "limit": 10,
            "section": CONTEXT["section"],
        },
        "GET /v1/incidents": {
            **ranged,
            "window": CONTEXT["window"],
            "step": CONTEXT["step"],
            "section": CONTEXT["section"],
        },
    }[label]


@schemathesis.hook
def before_call(ctx, case, **kwargs):
    """Replace generated inputs with one coherent fixture from the running demo."""
    label = case.operation.label
    case.query = _queries(label)
    case.headers = {}
    if "{name}" in label:
        case.path_parameters = {"name": CONTEXT["section"]}
    elif label == "GET /v1/frame/{view}":
        case.path_parameters = {"view": CONTEXT["frame_view"]}
    elif label == "GET /v1/entity/{view}/{entity}":
        case.path_parameters = {
            "view": CONTEXT["frame_view"],
            "entity": CONTEXT["entity"],
        }


def _has_batch_rows(body: dict[str, Any]) -> bool:
    return bool(body) and any(page.get("rows") for page in body.values())


def _has_batch_series(body: dict[str, Any]) -> bool:
    return bool(body) and any(diff.get("series") for diff in body.values())


def _has_timeline_data(body: dict[str, Any]) -> bool:
    meta = body.get("meta", {})
    return meta.get("data_through_us") is not None or meta.get("store_data_through_us") is not None


EVIDENCE: dict[str, Callable[[dict[str, Any]], bool]] = {
    "GET /v1/version": lambda body: body.get("api") == "v1"
    and int(body.get("format_version", 0)) > 0,
    "GET /v1/sections": lambda body: bool(body.get("sections")),
    "GET /v1/segments": lambda body: bool(body.get("segments")),
    "GET /v1/section/{name}": lambda body: bool(body.get("rows")),
    "GET /v1/sections/batch": _has_batch_rows,
    "GET /v1/section/{name}/diff": lambda body: bool(body.get("series")),
    "GET /v1/sections/batch/diff": _has_batch_series,
    "GET /v1/timeline/overview": lambda body: int(
        body.get("retained_coverage_duration_us", 0)
    )
    > 0
    or _has_timeline_data(body),
    "GET /v1/timeline/events": _has_timeline_data,
    "GET /v1/timeline/health": lambda body: bool(body.get("points")),
    "GET /v1/timeline/heatmap": lambda body: bool(body.get("rows"))
    and int(body.get("quality", {}).get("snapshots", 0)) > 0,
    "GET /v1/timeline/spine": lambda body: body.get("grid", {}).get(
        "bucket_count"
    )
    == 8
    and len(body.get("series", [])) == 2,
    "GET /v1/frame/{view}": lambda body: bool(body.get("rows"))
    and body.get("view") == CONTEXT["frame_view"],
    "GET /v1/ui/catalog": lambda body: bool(body.get("views")),
    "GET /v1/ui/context": lambda body: body.get("snapshot_ts_us") is not None
    and isinstance(body.get("databases"), list),
    "GET /v1/data/quality": lambda body: bool(body.get("capabilities"))
    and body.get("status") in {"fresh", "late", "stale", "partial"},
    "GET /v1/entity/{view}/{entity}": lambda body: body.get("mode") == "point"
    and body.get("entity") == CONTEXT["entity"],
    "GET /v1/storage": lambda body: isinstance(body.get("used_bytes"), dict)
    and isinstance(body.get("integrity"), dict),
    "GET /v1/views/summary": lambda body: bool(body.get("views"))
    and int(body.get("quality", {}).get("snapshots", 0)) > 0,
    "GET /v1/anomalies": lambda body: int(
        body.get("coverage", {}).get("sections_scanned", 0)
    )
    > 0,
    "GET /v1/incidents": lambda body: body.get("analysis_status") != "no_data"
    and bool(body.get("coverage_by_section")),
}


def _evidence_summary(label: str, body: dict[str, Any]) -> str:
    summaries: dict[str, Callable[[dict[str, Any]], str]] = {
        "GET /v1/version": lambda value: (
            f"api={value.get('api')}, format_version={value.get('format_version')}"
        ),
        "GET /v1/sections": lambda value: (
            f"sections={len(value.get('sections', []))}"
        ),
        "GET /v1/segments": lambda value: (
            f"segments={len(value.get('segments', []))}"
        ),
        "GET /v1/section/{name}": lambda value: (
            f"rows={len(value.get('rows', []))}"
        ),
        "GET /v1/sections/batch": lambda value: (
            f"pages={len(value)}, rows={sum(len(page.get('rows', [])) for page in value.values())}"
        ),
        "GET /v1/section/{name}/diff": lambda value: (
            f"series={len(value.get('series', []))}"
        ),
        "GET /v1/sections/batch/diff": lambda value: (
            f"sections={len(value)}, "
            f"series={sum(len(diff.get('series', [])) for diff in value.values())}"
        ),
        "GET /v1/timeline/overview": lambda value: (
            f"coverage_us={value.get('retained_coverage_duration_us', 0)}"
        ),
        "GET /v1/timeline/events": lambda value: (
            f"events={len(value.get('events', []))}, "
            f"data_through_us={value.get('meta', {}).get('data_through_us')}"
        ),
        "GET /v1/timeline/health": lambda value: (
            f"points={len(value.get('points', []))}"
        ),
        "GET /v1/timeline/heatmap": lambda value: (
            f"rows={len(value.get('rows', []))}, "
            f"snapshots={value.get('quality', {}).get('snapshots', 0)}"
        ),
        "GET /v1/timeline/spine": lambda value: (
            f"buckets={value.get('grid', {}).get('bucket_count')}, "
            f"series={len(value.get('series', []))}"
        ),
        "GET /v1/frame/{view}": lambda value: (
            f"view={value.get('view')}, rows={len(value.get('rows', []))}"
        ),
        "GET /v1/ui/catalog": lambda value: (
            f"views={len(value.get('views', []))}"
        ),
        "GET /v1/ui/context": lambda value: (
            f"snapshot={value.get('snapshot_ts_us')}, "
            f"databases={len(value.get('databases', []))}"
        ),
        "GET /v1/data/quality": lambda value: (
            f"status={value.get('status')}, "
            f"capabilities={len(value.get('capabilities', []))}"
        ),
        "GET /v1/entity/{view}/{entity}": lambda value: (
            f"view={value.get('view')}, fields={len(value.get('fields', []))}"
        ),
        "GET /v1/storage": lambda value: (
            f"pgm_bytes={value.get('used_bytes', {}).get('pgm')}, "
            f"readable_segments={value.get('integrity', {}).get('readable_segments')}"
        ),
        "GET /v1/views/summary": lambda value: (
            f"views={len(value.get('views', []))}, "
            f"snapshots={value.get('quality', {}).get('snapshots', 0)}"
        ),
        "GET /v1/anomalies": lambda value: (
            f"sections_scanned={value.get('coverage', {}).get('sections_scanned', 0)}, "
            f"episodes={len(value.get('episodes', []))}"
        ),
        "GET /v1/incidents": lambda value: (
            f"analysis_status={value.get('analysis_status')}, "
            f"coverage_sections={len(value.get('coverage_by_section', {}))}, "
            f"incidents={len(value.get('incidents', []))}"
        ),
    }
    return summaries[label](body)


@schemathesis.check
class DemoEvidence:
    """Require one schema-valid 200 with endpoint-specific evidence from every operation."""

    def __init__(self):
        self.observed: dict[str, str] = {}

    def after_response(self, ctx, response, case):
        label = case.operation.label
        assert response.status_code == 200, f"{label} returned {response.status_code}, expected 200"
        body = response.json()
        assert isinstance(body, dict), f"{label} returned a non-object JSON body"
        assert EVIDENCE[label](body), f"{label} returned no smoke-test evidence"
        self.observed[label] = _evidence_summary(label, body)

    def after_run(self, ctx):
        missing = EXPECTED - self.observed.keys()
        assert not missing, "operations not proven: " + ", ".join(sorted(missing))
        print("\nDemo data evidence:")
        for label in sorted(self.observed):
            print(f"  {label}: status=200, {self.observed[label]}")
