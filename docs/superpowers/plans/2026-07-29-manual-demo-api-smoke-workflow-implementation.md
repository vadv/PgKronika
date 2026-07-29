# Manual Demo API Smoke Workflow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an on-demand GitHub Actions workflow that builds an isolated demo, waits up to 20 minutes for ordinary retained data, and runs the existing evidence-bearing API smoke.

**Architecture:** Extend the Python smoke runner with a bounded retained-segment polling phase that is disabled locally and enabled by CI. Add a dedicated `workflow_dispatch` workflow that reuses the existing dependency-keyed BDD builder, owns the demo lifecycle, and always uploads diagnostics.

**Tech Stack:** GitHub Actions, Bash, Docker, Nix BDD builder, Python 3 standard library, `unittest`, `uvx`, Schemathesis 4.24.3.

## Global Constraints

- The workflow trigger is only `workflow_dispatch`; no `push`, `pull_request`, or scheduled trigger.
- The selected GitHub revision is built and tested in a self-contained runner.
- Keep `KRONIKA_SEGMENT_MAX_AGE_S=900` and other ordinary demo defaults.
- Wait at most 1200 seconds for retained data and poll every ten seconds.
- Local `make demo-api-smoke` remains immediate unless `DEMO_API_WAIT_SECONDS` is set.
- Schemathesis still checks all 15 runtime OpenAPI operations and endpoint-specific real-data evidence.
- Cleanup and diagnostic artifact upload run after success or failure.
- The manual workflow is not a required PR check.

---

### Task 1: Bounded Retained-Data Waiting

**Files:**
- Create: `scripts/test_demo_api_smoke.py`
- Modify: `scripts/demo-api-smoke.py`

**Interfaces:**
- Consumes: `request_json(base_url, "/v1/segments", query=..., authorization=...)`.
- Produces: `parse_wait_seconds(raw: str | None) -> int`.
- Produces: `wait_for_retained_segments(base_url: str, authorization: str | None, wait_seconds: int) -> None`.
- Reads: `DEMO_API_WAIT_SECONDS`, default `0`.

- [x] **Step 1: Write failing configuration tests**

Create a `unittest` module that loads `demo-api-smoke.py` with
`importlib.util.spec_from_file_location` and asserts:

```python
class WaitConfigurationTests(unittest.TestCase):
    def test_absent_wait_is_zero(self) -> None:
        self.assertEqual(SMOKE.parse_wait_seconds(None), 0)

    def test_positive_wait_is_accepted(self) -> None:
        self.assertEqual(SMOKE.parse_wait_seconds("1200"), 1200)

    def test_invalid_wait_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            SMOKE.PreflightError,
            "DEMO_API_WAIT_SECONDS must be a non-negative integer",
        ):
            SMOKE.parse_wait_seconds("twenty")

    def test_negative_wait_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            SMOKE.PreflightError,
            "DEMO_API_WAIT_SECONDS must be a non-negative integer",
        ):
            SMOKE.parse_wait_seconds("-1")
```

- [x] **Step 2: Run the configuration tests and verify RED**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
```

Expected: errors because `parse_wait_seconds` does not exist.

- [x] **Step 3: Implement strict wait parsing**

Add:

```python
def parse_wait_seconds(raw: str | None) -> int:
    if raw is None:
        return 0
    try:
        value = int(raw)
    except ValueError as error:
        raise PreflightError(
            "DEMO_API_WAIT_SECONDS must be a non-negative integer"
        ) from error
    if value < 0:
        raise PreflightError(
            "DEMO_API_WAIT_SECONDS must be a non-negative integer"
        )
    return value
```

- [x] **Step 4: Run the configuration tests and verify GREEN**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
```

Expected: four configuration tests pass.

- [x] **Step 5: Write failing polling tests**

Patch `request_json`, `time.monotonic`, and `time.sleep` and assert:

```python
class RetainedSegmentWaitTests(unittest.TestCase):
    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(SMOKE.time, "monotonic", side_effect=[0.0, 0.0])
    @mock.patch.object(
        SMOKE,
        "request_json",
        return_value={"segments": [{"min_ts": 1, "max_ts": 2}]},
    )
    def test_existing_segment_returns_without_sleep(
        self, request_json, _monotonic, sleep
    ) -> None:
        SMOKE.wait_for_retained_segments("http://demo", None, 1200)
        sleep.assert_not_called()
        request_json.assert_called_once()

    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(
        SMOKE.time,
        "monotonic",
        side_effect=[0.0, 0.0, 10.0, 10.0],
    )
    @mock.patch.object(
        SMOKE,
        "request_json",
        side_effect=[
            {"segments": []},
            {"segments": [{"min_ts": 1, "max_ts": 2}]},
        ],
    )
    def test_empty_result_is_polled_until_a_segment_appears(
        self, request_json, _monotonic, sleep
    ) -> None:
        SMOKE.wait_for_retained_segments("http://demo", None, 1200)
        self.assertEqual(request_json.call_count, 2)
        sleep.assert_called_once_with(10)

    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(
        SMOKE.time,
        "monotonic",
        side_effect=[0.0, 0.0, 1200.0],
    )
    @mock.patch.object(SMOKE, "request_json", return_value={"segments": []})
    def test_empty_result_fails_at_the_deadline(
        self, _request_json, _monotonic, sleep
    ) -> None:
        with self.assertRaisesRegex(
            SMOKE.PreflightError,
            "no retained segments after waiting 1200 seconds",
        ):
            SMOKE.wait_for_retained_segments("http://demo", None, 1200)
        sleep.assert_called_once_with(10)
```

The exact mocked monotonic sequence may be reduced to match the final
implementation, but the assertions and externally visible behavior remain as
shown.

- [x] **Step 6: Run the polling tests and verify RED**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
```

Expected: errors because `wait_for_retained_segments` does not exist.

- [x] **Step 7: Implement bounded polling**

Import `time`, define `SEGMENTS_QUERY`,
`RETAINED_SEGMENT_POLL_SECONDS = 10`, and implement:

```python
def wait_for_retained_segments(
    base_url: str,
    authorization: str | None,
    wait_seconds: int,
) -> None:
    deadline = time.monotonic() + wait_seconds
    while True:
        body = request_json(
            base_url,
            "/v1/segments",
            query=SEGMENTS_QUERY,
            authorization=authorization,
        )
        if body.get("segments", []):
            return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise PreflightError(
                f"demo has no retained segments after waiting {wait_seconds} seconds"
            )
        delay = min(RETAINED_SEGMENT_POLL_SECONDS, remaining)
        print(
            f"demo has no retained segments; waiting {delay:g}s "
            f"({remaining:g}s remaining)",
            flush=True,
        )
        time.sleep(delay)
```

Use `SEGMENTS_QUERY` in both the wait and existing preflight. In `run()`,
parse the environment and invoke the wait only when the value is positive,
then call the unchanged `prepare_context`.

- [x] **Step 8: Run all smoke unit tests and verify GREEN**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
```

Expected: all configuration and polling tests pass.

- [x] **Step 9: Commit retained-data waiting**

```bash
git add scripts/demo-api-smoke.py scripts/test_demo_api_smoke.py
git commit -m "test: wait for retained demo API data"
```

### Task 2: Dedicated Manual GitHub Workflow

**Files:**
- Create: `.github/workflows/demo-api-smoke.yml`
- Modify: `scripts/test_demo_api_smoke.py`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `make demo-build`, `make demo-up`, `make demo-api-smoke`, and
  `make demo-down`.
- Consumes: the exact builder name produced by `scripts/bdd-image.sh
  deps-key` and `platform-slug`.
- Sets: `DEMO_API_URL=http://127.0.0.1:18081`.
- Sets: `DEMO_API_WAIT_SECONDS=1200`.
- Produces: the `demo-api-smoke-<run>-<attempt>` artifact from `demo-data/`.

- [x] **Step 1: Write the failing workflow contract test**

Add a test that reads `.github/workflows/demo-api-smoke.yml` and verifies:

```python
class ManualWorkflowContractTests(unittest.TestCase):
    def test_workflow_is_manual_only_and_owns_the_demo_lifecycle(self) -> None:
        workflow = (
            Path(__file__).resolve().parent.parent
            / ".github/workflows/demo-api-smoke.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotRegex(workflow, r"(?m)^  (push|pull_request|schedule):")
        self.assertIn("timeout-minutes: 60", workflow)
        self.assertIn("DEMO_API_WAIT_SECONDS: \"1200\"", workflow)
        for command in (
            "make demo-build",
            "make demo-up",
            "make demo-api-smoke",
            "make demo-down",
        ):
            self.assertIn(command, workflow)
        self.assertGreaterEqual(workflow.count("if: always()"), 2)
        self.assertIn("actions/upload-artifact@v4", workflow)
```

- [x] **Step 2: Run the contract test and verify RED**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
```

Expected: `FileNotFoundError` because the workflow does not exist.

- [x] **Step 3: Create the manual workflow**

Create a workflow with:

```yaml
name: Demo API smoke

on:
  workflow_dispatch:

concurrency:
  group: demo-api-smoke
  cancel-in-progress: false

jobs:
  smoke:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    permissions:
      contents: read
      packages: write
```

Reuse the exact builder computation, pull, login, Buildx, and missing-builder
steps from `bdd-matrix`. Then install `uv` with
`astral-sh/setup-uv@v6`, build the demo, start it, and run:

```yaml
      - name: Run API smoke
        env:
          DEMO_API_URL: http://127.0.0.1:18081
          DEMO_API_WAIT_SECONDS: "1200"
        run: |
          set -o pipefail
          make demo-api-smoke 2>&1 | tee demo-data/api-smoke.log
```

Add two unconditional final steps. The first captures any live container log
and runs `make demo-down` without replacing the smoke failure. The second
uploads `demo-data` with a 14-day retention period and
`if-no-files-found: warn`.

- [x] **Step 4: Register the fast workflow test in regular CI**

In the `deps` job, add:

```yaml
      - run: python3 -B -m unittest scripts/test_demo_api_smoke.py -v
```

This validates wait logic and the manual-only workflow contract without
running the expensive smoke on PRs.

- [x] **Step 5: Run unit and workflow syntax checks**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
ruby -e 'require "yaml"; YAML.safe_load(File.read(".github/workflows/demo-api-smoke.yml"), aliases: true)'
```

Expected: unit tests pass and Ruby exits zero.

- [x] **Step 6: Commit the workflow**

```bash
git add .github/workflows/demo-api-smoke.yml .github/workflows/ci.yml \
  scripts/test_demo_api_smoke.py
git commit -m "ci: add manual demo API smoke"
```

### Task 3: Documentation and Final Verification

**Files:**
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`
- Modify: `docs/superpowers/specs/2026-07-29-manual-demo-api-smoke-workflow-design.md`
- Modify: `docs/superpowers/plans/2026-07-29-manual-demo-api-smoke-workflow-implementation.md`

**Interfaces:**
- Documents: local immediate smoke and manual hosted smoke.
- Records: ordinary 900-second rollover and 1200-second deadline.

- [x] **Step 1: Update both README files**

Replace the statement that the smoke is not run in CI. State that:

- local `make demo-api-smoke` checks an already running demo immediately;
- `DEMO_API_WAIT_SECONDS` optionally waits for retained segments;
- the `Demo API smoke` Actions workflow is manually dispatched for a selected
  revision;
- it uses ordinary rollover and waits up to 20 minutes.

- [x] **Step 2: Run documentation and formatting checks**

Run:

```bash
python3 -B scripts/validate-single-root-terminology.py
cargo fmt --all --check
git diff --check
```

Expected: all commands exit zero.

- [x] **Step 3: Run the full focused verification**

Run:

```bash
python3 -B -m unittest scripts/test_demo_api_smoke.py -v
python3 -m py_compile scripts/demo-api-smoke.py scripts/test_demo_api_smoke.py
ruby -e 'require "yaml"; YAML.safe_load(File.read(".github/workflows/demo-api-smoke.yml"), aliases: true)'
make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
```

Expected: tests and syntax checks pass, and OpenAPI regeneration produces no
diff.

- [x] **Step 4: Inspect the final diff**

Run:

```bash
git status --short
git diff --stat
git diff --check
```

Confirm that only the smoke runner, its test, the two workflows, the two
README files, and these design/plan documents changed.

- [x] **Step 5: Commit documentation and planning artifacts**

```bash
git add bins/pg_kronika-web/README.md bins/pg_kronika-web/README.ru.md \
  docs/superpowers/specs/2026-07-29-manual-demo-api-smoke-workflow-design.md \
  docs/superpowers/plans/2026-07-29-manual-demo-api-smoke-workflow-implementation.md
git commit -m "docs: describe manual demo API smoke"
```

- [x] **Step 6: Push the implementation branch**

Run:

```bash
git push origin feat/typed-multifile-openapi
```

Expected: the existing pull request is updated with all implementation
commits.

- [ ] **Step 7: Dispatch hosted verification after merge**

GitHub accepts `workflow_dispatch` only after the workflow file exists on the
default branch. After this pull request is merged, run:

```bash
gh workflow run demo-api-smoke.yml --ref main
gh run list --workflow demo-api-smoke.yml --branch main --limit 1
```

Record the run URL. The hosted run is expected to take at least one ordinary
900-second rollover before it can execute Schemathesis.
