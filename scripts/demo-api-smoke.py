#!/usr/bin/env python3
"""Prepare real demo inputs and run one Schemathesis case per API operation."""

from __future__ import annotations

import base64
import json
import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


DEFAULT_BASE_URL = "http://127.0.0.1:8688"
DEFAULT_SCHEMATHESIS_VERSION = "4.24.3"
MAX_SMOKE_RANGE_US = 6 * 60 * 60 * 1_000_000
SECTION_PRIORITY = (
    "pg_stat_database",
    "pg_stat_io",
    "pg_stat_wal",
    "pg_stat_bgwriter",
    "pg_stat_statements",
)


class PreflightError(RuntimeError):
    """The running demo cannot provide a meaningful smoke-test fixture."""


def authorization_header(credentials: str | None) -> str | None:
    if credentials is None:
        return None
    if ":" not in credentials:
        raise PreflightError("PG_KRONIKA_SMOKE_AUTH must use user:password")
    encoded = base64.b64encode(credentials.encode("utf-8")).decode("ascii")
    return f"Basic {encoded}"


def request(
    base_url: str,
    path: str,
    *,
    query: dict[str, Any] | None = None,
    authorization: str | None = None,
) -> tuple[int, bytes]:
    url = f"{base_url}{path}"
    if query:
        url = f"{url}?{urllib.parse.urlencode(query)}"
    headers = {"Accept": "application/json"}
    if authorization is not None:
        headers["Authorization"] = authorization
    try:
        with urllib.request.urlopen(
            urllib.request.Request(url, headers=headers),
            timeout=10,
        ) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        raise PreflightError(f"{url} returned HTTP {error.code}: {body}") from error
    except urllib.error.URLError as error:
        raise PreflightError(f"cannot reach {url}: {error.reason}") from error


def request_json(
    base_url: str,
    path: str,
    *,
    query: dict[str, Any] | None = None,
    authorization: str | None = None,
) -> Any:
    status, body = request(base_url, path, query=query, authorization=authorization)
    if status != 200:
        raise PreflightError(f"{path} returned HTTP {status}, expected 200")
    try:
        return json.loads(body)
    except json.JSONDecodeError as error:
        raise PreflightError(f"{path} did not return JSON: {error}") from error


def prepare_context(base_url: str, authorization: str | None) -> dict[str, Any]:
    health_status, _ = request(base_url, "/healthz")
    ready_status, _ = request(base_url, "/readyz")
    if health_status != 200 or ready_status != 200:
        raise PreflightError(
            f"demo is not ready: healthz={health_status}, readyz={ready_status}"
        )

    swagger_status, _ = request(
        base_url,
        "/swagger-ui/",
        authorization=authorization,
    )
    if swagger_status != 200:
        raise PreflightError(
            f"Swagger UI returned HTTP {swagger_status}, expected 200"
        )

    schema = request_json(base_url, "/openapi.json", authorization=authorization)
    operation_count = sum(
        1
        for path_item in schema.get("paths", {}).values()
        for operation in path_item.values()
        if isinstance(operation, dict) and operation.get("operationId")
    )
    if operation_count != 15:
        raise PreflightError(
            f"runtime OpenAPI exposes {operation_count} operations, expected 15"
        )

    segments_body = request_json(
        base_url,
        "/v1/segments",
        query={"from": -(2**63), "to": 2**63 - 1},
        authorization=authorization,
    )
    segments = segments_body.get("segments", [])
    if not segments:
        raise PreflightError("demo has no retained segments")

    minimum = min(int(segment["min_ts"]) for segment in segments)
    maximum = max(int(segment["max_ts"]) for segment in segments)
    to_us = min(maximum + 1, 2**63 - 1)
    from_us = max(minimum, to_us - MAX_SMOKE_RANGE_US)
    if to_us - from_us < 1_000_000:
        raise PreflightError("demo needs at least one second of retained data")

    rows_by_section: dict[str, int] = {}
    for segment in segments:
        if int(segment["max_ts"]) < from_us or int(segment["min_ts"]) >= to_us:
            continue
        for section in segment.get("sections", []):
            name = str(section["name"])
            rows_by_section[name] = rows_by_section.get(name, 0) + int(section["rows"])
    section = next(
        (candidate for candidate in SECTION_PRIORITY if rows_by_section.get(candidate, 0) > 0),
        None,
    )
    if section is None:
        raise PreflightError(
            "demo range has none of the scorable sections required by the smoke test"
        )
    names = [section]
    names.extend(
        name
        for name, rows in sorted(
            rows_by_section.items(),
            key=lambda item: (-item[1], item[0]),
        )
        if rows > 0 and name != section
    )
    names = names[:2]

    catalog = request_json(base_url, "/v1/ui/catalog", authorization=authorization)
    projection = next(
        (
            (view["code"], metric["code"])
            for view in catalog.get("views", [])
            if view.get("availability") == "available"
            for metric in view.get("metrics", [])
            if metric.get("availability") == "available"
        ),
        None,
    )
    if projection is None:
        raise PreflightError("demo catalog has no available heatmap metric")

    span_seconds = max(1, (to_us - from_us) // 1_000_000)
    window_seconds = max(1, min(300, span_seconds // 2))
    step_seconds = max(1, window_seconds // 4)
    return {
        "from": from_us,
        "to": to_us,
        "at": to_us - 1,
        "window": f"{window_seconds}s",
        "step": f"{step_seconds}s",
        "health_step_us": step_seconds * 1_000_000,
        "section": section,
        "names": ",".join(names),
        "view": projection[0],
        "metric": projection[1],
    }


def run() -> int:
    root = Path(__file__).resolve().parent.parent
    base_url = os.environ.get("DEMO_API_URL", DEFAULT_BASE_URL).rstrip("/")
    credentials = os.environ.get("PG_KRONIKA_SMOKE_AUTH")
    version = os.environ.get("SCHEMATHESIS_VERSION", DEFAULT_SCHEMATHESIS_VERSION)
    if shutil.which("uvx") is None:
        raise PreflightError("uvx is required; install uv before running the smoke test")

    context = prepare_context(base_url, authorization_header(credentials))
    print(
        "demo API smoke fixture: "
        f"range=[{context['from']}, {context['to']}), "
        f"section={context['section']}, names={context['names']}, "
        f"heatmap={context['view']}/{context['metric']}",
        flush=True,
    )

    environment = os.environ.copy()
    environment["PG_KRONIKA_SMOKE_CONTEXT"] = json.dumps(context, separators=(",", ":"))
    environment["SCHEMATHESIS_HOOKS"] = str(root / "scripts" / "demo_api_smoke_hooks.py")
    command = [
        "uvx",
        "--from",
        f"schemathesis=={version}",
        "schemathesis",
        "run",
        f"{base_url}/openapi.json",
        "--url",
        base_url,
        "--phases",
        "fuzzing",
        "--mode",
        "positive",
        "--max-examples",
        "1",
        "--generation-deterministic",
        "--workers",
        "1",
        "--checks",
        (
            "not_a_server_error,status_code_conformance,content_type_conformance,"
            "response_schema_conformance,positive_data_acceptance,DemoEvidence"
        ),
        "--request-timeout",
        "30",
    ]
    if credentials is not None:
        command.extend(["--auth", credentials])
    return subprocess.run(command, cwd=root, env=environment, check=False).returncode


if __name__ == "__main__":
    try:
        raise SystemExit(run())
    except PreflightError as error:
        print(f"demo API smoke preflight failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
