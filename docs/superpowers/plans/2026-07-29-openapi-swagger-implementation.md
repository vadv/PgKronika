# Simple OpenAPI and Swagger UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hand-written OpenAPI and RFC 9457 machinery with direct `utoipa` annotations, standard Swagger UI, and a minimal `{code, params}` error.

**Architecture:** Register each documented handler once in `api_docs` through `OpenApiRouter`, split that registry into the Axum router and generated document, and export the same document as tool-neutral YAML. Keep runtime API behavior tests, delete every OpenAPI/schema synchronization test, and reduce the error responder to two serialized fields plus private response metadata.

**Tech Stack:** Rust 1.96, Axum 0.8, utoipa 5, utoipa-axum 0.2, utoipa-swagger-ui 9, serde/serde_json.

## Global Constraints

- Keep one `OpenApiRouter` registry and one direct YAML exporter.
- Do not add schema comparisons, snapshot tests, generated-file checks, or OpenAPI tests.
- Do not type successful response DTOs as part of this change.
- Keep `/healthz`, `/readyz`, and `/metrics` outside OpenAPI.
- Keep Swagger UI assets vendored for the static musl build.
- OpenAPI/Swagger configuration is intentionally exempt from TDD because the approved spec forbids tests for it.

---

### Task 1: Reduce API errors and delete contract tests

**Files:**
- Move: `bins/pg_kronika-web/src/problem.rs` to `bins/pg_kronika-web/src/api_error.rs`
- Modify: Rust users of `crate::problem`, `ApiProblem`, and `ProblemCode`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`
- Modify: `bins/pg_kronika-web/src/tests/problems.rs`
- Modify: `crates/kronika-bdd/features/web_api.feature`
- Modify: `crates/kronika-bdd/src/steps/web.rs`
- Modify: `crates/kronika-bdd/src/harness/web.rs`

**Interfaces:**
- Produces: `ApiError`, `ErrorCode`, and the existing constructor set under `crate::api_error`
- Serialized response: exactly `{"code": <string>, "params": <object>}`
- Private response metadata: `StatusCode`, optional `Allow`, optional `Retry-After`

- [ ] **Step 1: Make the shared assertion require the new body**

Change `assert_problem` to `assert_api_error`:

```rust
fn assert_api_error(
    body: &serde_json::Value,
    status: StatusCode,
    code: &str,
    params: serde_json::Value,
) {
    assert_eq!(
        body,
        &serde_json::json!({
            "code": code,
            "params": params,
        })
    );
    assert!(status.is_client_error() || status.is_server_error());
}
```

Update its call sites mechanically without changing their expected status,
code, or params.

- [ ] **Step 2: Delete obsolete tests before running the focused behavior test**

Remove `problem_example`, `every_problem_code_has_the_exact_body_and_headers`,
`documented_v1_paths_reach_the_actual_router_with_contextual_allow`,
`generated_request_ids_are_unique_and_match_the_instance_header_invariant`,
`correlation_is_server_generated_and_does_not_reflect_request_data`,
`openapi_is_a_closed_projection_of_the_machine_registries`, and every helper
used only by those tests. Remove the locale-neutral BDD scenario, step, and
harness function.

- [ ] **Step 3: Run the focused test and verify RED**

Run:

```bash
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::problems::routing_method_and_query_shape_use_the_closed_registry
```

Expected: FAIL because the old response still contains `type`, `status`, and
`instance`.

- [ ] **Step 4: Implement the minimal error**

Define:

```rust
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ApiError {
    #[schema(value_type = String)]
    code: ErrorCode,
    #[schema(value_type = Object)]
    params: serde_json::Value,
    #[serde(skip)]
    status: StatusCode,
    #[serde(skip)]
    allow: Option<&'static str>,
    #[serde(skip)]
    retry_after_seconds: Option<u64>,
}
```

Keep the existing error constructors but build `params` with
`serde_json::json!`. Remove type URIs, request IDs, `ProblemParams`, all typed
parameter structs, `application/problem+json`, and `Cache-Control: no-store`.
Rename the module and types mechanically and remove `request_id` from log
fields.

- [ ] **Step 5: Run focused and package tests and verify GREEN**

Run:

```bash
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::problems::routing_method_and_query_shape_use_the_closed_registry
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin
```

Expected: all retained web tests pass.

- [ ] **Step 6: Commit the error simplification**

Commit the renamed module, updated users, and deleted contract tests together.

### Task 2: Add one documented router and Swagger UI

**Files:**
- Create: `bins/pg_kronika-web/src/api_docs.rs`
- Create: `bins/pg_kronika-web/examples/export_openapi.rs`
- Create: `bins/pg_kronika-web/swagger.yaml`
- Modify: `bins/pg_kronika-web/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `bins/pg_kronika-web/src/lib.rs`
- Modify: `bins/pg_kronika-web/src/handlers/v1.rs`
- Modify: `bins/pg_kronika-web/src/handlers/anomalies.rs`
- Modify: `bins/pg_kronika-web/src/handlers/incidents.rs`
- Modify: `bins/pg_kronika-web/src/overview/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Delete: `bins/pg_kronika-web/openapi.json`

**Interfaces:**
- Produces: one `(Router, OpenApi)` from the documented route registry
- Serves: `/swagger-ui/` and `/openapi.json`
- Exports: `bins/pg_kronika-web/swagger.yaml`

- [ ] **Step 1: Add the three dependencies**

Use:

```toml
utoipa = { version = "5", features = ["yaml"] }
utoipa-axum = "0.2"
utoipa-swagger-ui = { version = "9", features = ["axum", "vendored"] }
```

- [ ] **Step 2: Define the documented router**

Create one `OpenApiRouter` registry listing exactly these handlers:

```text
version, sections, segments, section_data, sections_batch,
section_diff, sections_batch_diff, overview, events, health,
heatmap, catalog, summary, anomalies, incidents
```

Split it into the production Axum router and generated document. Do not repeat
these `/v1/*` registrations in `lib.rs`.

- [ ] **Step 3: Annotate the handlers**

Each handler gets `get`, its literal `/v1/*` path, direct parameter tuples,
and:

```rust
responses(
    (status = 200, description = "OK"),
    (status = "default", description = "API error", body = ApiError),
)
```

Required parameter groups:

```text
segments: from, to
section_data: name, from, to; optional limit, cursor
sections_batch: from, to, names; optional limit
section_diff: name, from, to
sections_batch_diff: from, to, names
overview: from, to
events: from, to; optional limit, cursor, min_severity, kind
health: from, to; optional step
heatmap: view, metric, from, to; optional buckets, top
catalog: optional If-None-Match header
summary: at
anomalies: from, to; optional window, step, threshold, eps_rel, limit, section
incidents: from, to; optional window, step, threshold, eps_rel, epsilon,
           max_cluster_span, section
```

- [ ] **Step 4: Mount standard Swagger UI**

Split the documented router and merge:

```rust
let (api, document) = api_docs::router_and_document();
api.merge(SwaggerUi::new("/swagger-ui").url("/openapi.json", document))
```

into the existing protected router before Basic Auth is layered. Do not change
the public probes, metrics router, or SPA fallback.

- [ ] **Step 5: Delete the hand-written document and compile**

Delete `bins/pg_kronika-web/openapi.json`, then run:

```bash
cargo check -p pg_kronika-web --all-targets --target aarch64-apple-darwin
```

Expected: compilation succeeds with no OpenAPI-specific test added.

- [ ] **Step 6: Export neutral YAML**

Add the direct exporter and run:

```bash
make swagger
```

Do not add a generated-file check or OpenAPI test.

- [ ] **Step 7: Commit OpenAPI and Swagger UI**

Commit dependencies, annotations, router integration, deletion of
`openapi.json`, and generated `swagger.yaml` together.

### Task 3: Document and verify the operator path

**Files:**
- Modify: `README.md`
- Modify: `README.ru.md`
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`

- [ ] **Step 1: Document the two routes and minimal error**

Add `/swagger-ui/` and `/openapi.json` to the endpoint section, state that
they share Basic Auth with `/v1/*`, and replace RFC 9457 wording with
`{"code": "...", "params": {...}}`.

- [ ] **Step 2: Format and run full static verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin
cargo clippy -p pg_kronika-web --all-targets \
  --target aarch64-apple-darwin -- -D warnings
git diff --check
```

- [ ] **Step 3: Verify the approved demo acceptance**

Run the available demo image or build it if necessary:

```bash
scripts/demo-stand.sh up
```

Open `http://127.0.0.1:18081/swagger-ui/`, execute `GET /v1/version` with
`Try it out`, and confirm HTTP 200. Stop the stand with:

```bash
scripts/demo-stand.sh down
```

- [ ] **Step 4: Commit and push**

Commit the implementation and push `docs/openapi-swagger-migration` so PR #138
contains the spec and implementation.
