#!/usr/bin/env bash
set -euo pipefail

KEY_SCHEMA=2
COMPILER_CACHE_SCHEMA=1
CARGO_TARGET=x86_64-unknown-linux-musl
CARGO_FEATURES=default
PG_MAJORS=15,16,17,18
NIX_BASE_IMAGE=${BDD_NIX_BASE_IMAGE:-docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894}
DOCKER=${BDD_DOCKER:-docker}

DEPENDENCY_CONTENT_PATHS=(
  Dockerfile.bdd-builder
  scripts/bdd-image.sh
  ':(glob)**/*.nix'
  flake.lock
  rust-toolchain.toml
  ':(glob)**/.cargo/**'
  ':(glob)**/Cargo.toml'
  Cargo.lock
)

CARGO_TARGET_TOPOLOGY_PATHS=(
  ':(glob)crates/*/src/lib.rs'
  ':(glob)crates/*/src/main.rs'
  ':(glob)crates/*/src/bin/*.rs'
  ':(glob)crates/*/src/bin/*/main.rs'
  ':(glob)crates/*/benches/*.rs'
  ':(glob)crates/*/benches/*/main.rs'
  ':(glob)crates/*/examples/*.rs'
  ':(glob)crates/*/examples/*/main.rs'
  ':(glob)crates/*/tests/*.rs'
  ':(glob)crates/*/tests/*/main.rs'
  ':(glob)bins/*/src/lib.rs'
  ':(glob)bins/*/src/main.rs'
  ':(glob)bins/*/src/bin/*.rs'
  ':(glob)bins/*/src/bin/*/main.rs'
  ':(glob)bins/*/benches/*.rs'
  ':(glob)bins/*/benches/*/main.rs'
  ':(glob)bins/*/examples/*.rs'
  ':(glob)bins/*/examples/*/main.rs'
  ':(glob)bins/*/tests/*.rs'
  ':(glob)bins/*/tests/*/main.rs'
  xtask/src/lib.rs
  xtask/src/main.rs
)

APP_KEY_PATHS=(
  Dockerfile.bdd-app
  'crates/*/src/**'
  'crates/*/benches/**'
  'bins/*/src/**'
  'bins/*/benches/**'
  'bins/*/static/**'
  'crates/kronika-bdd/features/**'
  'xtask/src/**'
)

APP_SOURCE_PATHS=(
  "${DEPENDENCY_CONTENT_PATHS[@]}"
  "${APP_KEY_PATHS[@]}"
)

usage() {
  cat <<'EOF'
Usage: scripts/bdd-image.sh <command>

Key and metadata commands:
  deps-key                  Full immutable dependency key.
  source-key                Full source/app content key.
  compiler-cache-key        First-party compiler cache namespace.
  app-key DEPS_REF PG_REF   Full runtime key, bound to immutable digests.
  keys-json                 Machine-readable key contract.
  deps-paths                Dependency content and target-topology inputs.
  image-paths               Source/app content inputs.
  dependency-image          Immutable dependency image tag.
  pg-base-image             Immutable PostgreSQL runtime base tag.
  runtime-image [DEPS_REF PG_REF]
                            Runtime tag; digest refs are required in CI.
  resolve-ref IMAGE         Resolve a public image tag to repo@sha256:....
  platform                  Docker platform.
  platform-slug             Docker tag platform fragment.

Build commands:
  dependency-context-tar    Canonical dummy-source dependency context.
  build-dependencies        Build exact Cargo and PG15-18 dependency images.
  resolve-dependencies      Resolve and report both immutable dependency refs.
  verify-pg-runtime IMAGE   Check PG15-18 binaries and extension files.
  assert-source-only-plan FILE
                            Fail if a Nix plan contains PG/extension work.
  build-app-layer           Build the source-only application tar layer.
  assemble-runtime          Add BDD_APP_LAYER to BDD_PG_BASE_DIGEST_REF.
  publish-runtime           Trusted publication of the exact runtime image.
  build-runtime             Build app layer and assemble a local runtime.
  build-builder             Compatibility alias for build-dependencies.
  run [IMAGE] [ARGS...]     Run the BDD image.

Required publication gate:
  BDD_TRUSTED_PUBLISH=1     Required with BDD_DEPENDENCY_PUSH=1 or
                            BDD_RUNTIME_PUSH=1. Pulls never require auth.

Important overrides:
  BDD_IMAGE_PREFIX          Owner prefix; default ghcr.io/vadv.
  BDD_PLATFORM              Default local Docker platform.
  BDD_DEPENDENCY_IMAGE      Exact dependency image tag.
  BDD_PG_BASE_IMAGE         Exact PG runtime base tag.
  BDD_DEPENDENCY_DIGEST_REF Resolved dependency repo@sha256 ref.
  BDD_PG_BASE_DIGEST_REF    Resolved PG base repo@sha256 ref.
  BDD_RUNTIME_IMAGE         Exact runtime image tag.
  BDD_APP_LAYER             App tar path, default app-layer.tar.
  BDD_SCCACHE_DIR           Restored compiler cache directory.
  BDD_SCCACHE_MODE          READ_ONLY for PRs, READ_WRITE for trusted runs.
  BDD_SOURCE_COMMIT         OCI revision label.
  DEBUG                     Passed to the BDD container.
EOF
}

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 2
}

docker_cmd() {
  "$DOCKER" "$@"
}

repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || pwd
}

sha256_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

print_git_paths() {
  local root
  root=$(repo_root)
  (
    cd "$root"
    git ls-files -co --exclude-standard -- "$@" | LC_ALL=C sort
  )
}

hash_files() {
  local root path
  root=$(repo_root)
  while IFS= read -r path; do
    [ -f "$root/$path" ] || continue
    printf 'file\0%s\0' "$path"
    cat "$root/$path"
    printf '\0'
  done
}

target_topology() {
  print_git_paths "${CARGO_TARGET_TOPOLOGY_PATHS[@]}"
}

platform() {
  if [ -n "${BDD_PLATFORM:-}" ]; then
    printf '%s' "$BDD_PLATFORM"
    return
  fi

  local os arch
  os=$(docker_cmd info --format '{{.OSType}}')
  arch=$(docker_cmd info --format '{{.Architecture}}')
  case "$arch" in
    x86_64) arch=amd64 ;;
    aarch64) arch=arm64 ;;
  esac
  printf '%s/%s' "$os" "$arch"
}

platform_slug() {
  platform | tr '/_' '-'
}

dependency_contract() {
  printf 'schema\0%s\0platform\0%s\0target\0%s\0features\0%s\0pg-majors\0%s\0nix-base\0%s\0' \
    "$KEY_SCHEMA" "$(platform)" "$CARGO_TARGET" "$CARGO_FEATURES" "$PG_MAJORS" "$NIX_BASE_IMAGE"
  print_git_paths "${DEPENDENCY_CONTENT_PATHS[@]}" | hash_files
  while IFS= read -r path; do
    printf 'cargo-target\0%s\0' "$path"
  done < <(target_topology)
}

deps_key() {
  dependency_contract | sha256_stream
}

source_key() {
  {
    printf 'schema\0%s\0platform\0%s\0' "$KEY_SCHEMA" "$(platform)"
    print_git_paths "${APP_KEY_PATHS[@]}" | hash_files
  } | sha256_stream
}

compiler_cache_key() {
  printf 'schema\0%s\0platform\0%s\0target\0%s\0features\0%s\0dependency\0%s\0' \
    "$COMPILER_CACHE_SCHEMA" "$(platform)" "$CARGO_TARGET" "$CARGO_FEATURES" "$(deps_key)" \
    | sha256_stream
}

source_revision() {
  local revision
  revision=${BDD_SOURCE_COMMIT:-$(git -C "$(repo_root)" rev-parse HEAD 2>/dev/null || true)}
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || fail "BDD_SOURCE_COMMIT must be a full Git commit SHA"
  printf '%s' "$revision"
}

app_key() {
  [ "$#" -eq 2 ] || fail "app-key requires dependency and PG digest refs"
  printf 'schema\0%s\0platform\0%s\0deps\0%s\0pg\0%s\0source\0%s\0revision\0%s\0' \
    "$KEY_SCHEMA" "$(platform)" "$1" "$2" "$(source_key)" "$(source_revision)" \
    | sha256_stream
}

image_prefix() {
  printf '%s' "${BDD_IMAGE_PREFIX:-ghcr.io/vadv}"
}

dependency_image() {
  if [ -n "${BDD_DEPENDENCY_IMAGE:-}" ]; then
    printf '%s' "$BDD_DEPENDENCY_IMAGE"
  else
    printf '%s/pgkronika-bdd-builder:deps-%s-%s' \
      "$(image_prefix)" "$(platform_slug)" "$(deps_key)"
  fi
}

pg_base_image() {
  if [ -n "${BDD_PG_BASE_IMAGE:-}" ]; then
    printf '%s' "$BDD_PG_BASE_IMAGE"
  else
    printf '%s/pgkronika-bdd:pg-%s-%s' \
      "$(image_prefix)" "$(platform_slug)" "$(deps_key)"
  fi
}

runtime_image() {
  if [ -n "${BDD_RUNTIME_IMAGE:-}" ]; then
    printf '%s' "$BDD_RUNTIME_IMAGE"
    return
  fi
  local deps_ref=${1:-dependency-key:$(deps_key)}
  local pg_ref=${2:-pg-key:$(deps_key)}
  printf '%s/pgkronika-bdd:app-%s-%s' \
    "$(image_prefix)" "$(platform_slug)" "$(app_key "$deps_ref" "$pg_ref")"
}

image_repository() {
  local ref=${1%@*} last
  last=${ref##*/}
  if [[ "$last" == *:* ]]; then
    printf '%s' "${ref%:*}"
  else
    printf '%s' "$ref"
  fi
}

resolve_ref() {
  local ref=$1 digest
  docker_cmd manifest inspect "$ref" >/dev/null 2>&1 || return 1
  digest=$(docker_cmd buildx imagetools inspect "$ref" --format '{{.Manifest.Digest}}' 2>/dev/null || true)
  [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || return 1
  printf '%s@%s' "$(image_repository "$ref")" "$digest"
}

append_summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '%s\n' "$*" >> "$GITHUB_STEP_SUMMARY"
  fi
}

source_tar() {
  local root
  root=$(repo_root)
  (
    cd "$root"
    git ls-files -co --exclude-standard -z -- "$@" \
      | LC_ALL=C sort -z \
      | tar --null -T - -cf -
  )
}

write_dependency_context() {
  local context=$1 root path
  root=$(repo_root)
  source_tar "${DEPENDENCY_CONTENT_PATHS[@]}" | tar -C "$context" -xf -
  while IFS= read -r path; do
    mkdir -p "$context/$(dirname "$path")"
    case "$path" in
      */src/lib.rs) printf '#![allow(missing_docs)]\n' > "$context/$path" ;;
      *) printf 'fn main() {}\n' > "$context/$path" ;;
    esac
  done < <(target_topology)
  printf '%s\n' "$KEY_SCHEMA" > "$context/.bdd-cache-schema"
  printf '%s\n' "$(deps_key)" > "$context/.bdd-dependency-key"
  test -f "$root/Dockerfile.bdd-builder"
}

dependency_context_tar() {
  local context
  context=$(mktemp -d)
  write_dependency_context "$context"
  tar --sort=name --mtime=@1 --owner=0 --group=0 --numeric-owner -C "$context" -cf - .
  rm -rf "$context"
}

keys_json() {
  printf '{"schema":%s,"platform":"%s","cargo_target":"%s","cargo_features":"%s","postgresql_majors":[15,16,17,18],"nix_base":"%s","dependency_key":"%s","source_key":"%s","compiler_cache_key":"%s"}\n' \
    "$KEY_SCHEMA" "$(platform)" "$CARGO_TARGET" "$CARGO_FEATURES" "$NIX_BASE_IMAGE" "$(deps_key)" "$(source_key)" "$(compiler_cache_key)"
}

print_dependency_paths() {
  print_git_paths "${DEPENDENCY_CONTENT_PATHS[@]}"
  while IFS= read -r path; do
    printf 'topology:%s\n' "$path"
  done < <(target_topology)
}

trusted_publish() {
  [ "${BDD_TRUSTED_PUBLISH:-0}" = "1" ] \
    || fail "publishing requires BDD_TRUSTED_PUBLISH=1"
}

build_target() {
  local target=$1 image=$2 context=$3 output cache_scope
  local -a cache_args=()
  cache_scope="bdd-deps-${KEY_SCHEMA}-$(platform_slug)-$(deps_key)"
  if [ "${BDD_DEPENDENCY_PUSH:-0}" = "1" ]; then
    output=--push
    cache_args=(
      --cache-from "type=gha,scope=$cache_scope"
      --cache-to "type=gha,scope=$cache_scope,mode=max"
    )
  else
    output=--load
  fi
  docker_cmd buildx build \
    -f "$(repo_root)/Dockerfile.bdd-builder" \
    --target "$target" \
    --platform "$(platform)" \
    --build-arg "BDD_NIX_BASE=$NIX_BASE_IMAGE" \
    --build-arg "BDD_KEY_SCHEMA=$KEY_SCHEMA" \
    --build-arg "BDD_DEPENDENCY_KEY=$(deps_key)" \
    "${cache_args[@]}" \
    "$output" \
    -t "$image" \
    "$context"
}

build_dependencies() {
  local deps pg context deps_ref= pg_ref= deps_hit=no pg_hit=no start elapsed
  deps=$(dependency_image)
  pg=$(pg_base_image)
  if [ "${BDD_DEPENDENCY_PUSH:-0}" = "1" ]; then
    trusted_publish
  fi

  start=$SECONDS
  deps_ref=$(resolve_ref "$deps" || true)
  pg_ref=$(resolve_ref "$pg" || true)
  [ -n "$deps_ref" ] && deps_hit=yes
  [ -n "$pg_ref" ] && pg_hit=yes
  if [ -n "$deps_ref" ] && [ -n "$pg_ref" ]; then
    append_summary "## BDD immutable dependencies"
    append_summary ""
    append_summary "- dependency key: \`$(deps_key)\`"
    append_summary "- Cargo artifact hit: yes"
    append_summary "- dependency digest: \`$deps_ref\`"
    append_summary "- PostgreSQL 15-18 base hit: yes"
    append_summary "- PostgreSQL base digest: \`$pg_ref\`"
    printf 'dependency_digest_ref=%s\npg_digest_ref=%s\ndependency_hit=true\n' "$deps_ref" "$pg_ref"
    return
  fi

  context=$(mktemp -d)
  write_dependency_context "$context"
  if [ -z "$deps_ref" ]; then
    build_target bdd-deps "$deps" "$context"
  fi
  if [ -z "$pg_ref" ]; then
    build_target bdd-pg-base "$pg" "$context"
  fi
  rm -rf "$context"

  if [ "${BDD_DEPENDENCY_PUSH:-0}" = "1" ]; then
    deps_ref=$(resolve_ref "$deps") || fail "published dependency image has no digest"
    pg_ref=$(resolve_ref "$pg") || fail "published PostgreSQL base has no digest"
  else
    deps_ref=$deps
    pg_ref=$pg
  fi
  elapsed=$((SECONDS - start))
  append_summary "## BDD immutable dependencies"
  append_summary ""
  append_summary "- dependency key: \`$(deps_key)\`"
  append_summary "- Cargo artifact hit: $deps_hit"
  append_summary "- dependency digest: \`$deps_ref\`"
  append_summary "- PostgreSQL 15-18 base hit: $pg_hit"
  append_summary "- PostgreSQL base digest: \`$pg_ref\`"
  append_summary "- cold build seconds: $elapsed"
  printf 'dependency_digest_ref=%s\npg_digest_ref=%s\ndependency_hit=false\n' "$deps_ref" "$pg_ref"
}

resolve_dependencies() {
  local deps_ref pg_ref
  deps_ref=$(resolve_ref "$(dependency_image)") \
    || fail "immutable dependency image is absent; run the trusted dependency publisher"
  pg_ref=$(resolve_ref "$(pg_base_image)") \
    || fail "immutable PostgreSQL base is absent; run the trusted dependency publisher"
  printf 'dependency_digest_ref=%s\npg_digest_ref=%s\n' "$deps_ref" "$pg_ref"
}

assert_source_only_plan() {
  local plan=$1
  [ -f "$plan" ] || fail "Nix plan file does not exist: $plan"
  if grep -Eiq 'pg_store_plans|postgresql-and-plugins|pgkronika-bdd-pg-matrix|postgresql_(15|16|17|18)([^[:alnum:]]|$)|pgkronika-bdd-deps-deps' "$plan"; then
    printf 'Source-only Nix plan contains dependency or PostgreSQL work:\n' >&2
    grep -Ei 'pg_store_plans|postgresql-and-plugins|pgkronika-bdd-pg-matrix|postgresql_(15|16|17|18)([^[:alnum:]]|$)|pgkronika-bdd-deps-deps' "$plan" >&2 || true
    exit 1
  fi
}

verify_dependency_image() {
  local ref=$1 expected=$2
  docker_cmd run --rm "$ref" sh -ceu '
    test "$(cat /opt/bdd-cache/schema)" = "$1"
    test "$(cat /opt/bdd-cache/dependency-key)" = "$2"
    test -s /opt/bdd-cache/cargo-closure.json
    test -x /opt/bdd-cache/compiler-tools/bin/sccache
    test -x /opt/bdd-cache/compiler-tools/bin/pgkronika-sccache
  ' sh "$KEY_SCHEMA" "$expected"
}

verify_pg_base() {
  local ref=$1 expected=$2
  docker_cmd run --rm --entrypoint /bin/sh "$ref" -ceu '
    IFS= read -r schema < /opt/pgkronika/pg/schema
    IFS= read -r dependency_key < /opt/pgkronika/pg/dependency-key
    test "$schema" = "$1"
    test "$dependency_key" = "$2"
    test -s /opt/pgkronika/pg/closure.json
    for major in 15 16 17 18; do
      root="/opt/pgkronika/pg/$major"
      test -x "$root/bin/postgres"
      pkglibdir="$root/lib"
      if [ -d "$pkglibdir/postgresql" ]; then
        pkglibdir="$pkglibdir/postgresql"
      fi
      sharedir="$root/share/postgresql"
      test -f "$pkglibdir/pg_store_plans.so"
      test -f "$sharedir/extension/pg_store_plans.control"
      set -- "$sharedir"/extension/pg_store_plans--*.sql
      test -f "$1"
    done
  ' sh "$KEY_SCHEMA" "$expected"
}

verify_pg_runtime() {
  [ "$#" -eq 1 ] || fail "verify-pg-runtime requires one image"
  verify_pg_base "$1" "$(deps_key)"
}

run_app_nix() {
  local ref=$1 mode=$2 cache_dir cache_mode mount_mode
  cache_dir=${BDD_SCCACHE_DIR:-}
  cache_mode=${BDD_SCCACHE_MODE:-READ_ONLY}
  [ -n "$cache_dir" ] || fail "BDD_SCCACHE_DIR is required for source builds"
  [ -d "$cache_dir" ] || fail "BDD_SCCACHE_DIR does not exist: $cache_dir"
  case "$cache_mode" in
    READ_ONLY) mount_mode=ro ;;
    READ_WRITE) mount_mode=rw ;;
    *) fail "BDD_SCCACHE_MODE must be READ_ONLY or READ_WRITE" ;;
  esac
  printf '%s\n' "$cache_mode" > "$cache_dir/.mode"
  chmod 0644 "$cache_dir/.mode"
  source_tar "${APP_SOURCE_PATHS[@]}" | docker_cmd run --rm -i \
    -v "$cache_dir:/var/cache/pgkronika-sccache:$mount_mode" \
    "$ref" sh -ceu '
    mode=$1
    cache_mode=$2
    mkdir -p /tmp/src
    tar -C /tmp/src -xf -
    cd /tmp/src
    if [ "$mode" = plan ]; then
      nix build .#bddAppLayer .#bddCompilerStats --dry-run --no-link
    else
      nix build --option sandbox false .#bddAppLayer --out-link /tmp/bdd-app-layer 1>&2
      nix build --option sandbox false .#bddCompilerStats \
        --out-link /tmp/bdd-compiler-stats 1>&2
      printf "BDD_SCCACHE_STATS=" 1>&2
      cat "$(readlink -f /tmp/bdd-compiler-stats)" 1>&2
      printf "\n" 1>&2
      cat "$(readlink -f /tmp/bdd-app-layer)"
    fi
  ' sh "$mode" "$cache_mode"
}

report_sccache_stats() {
  local build_log=$1 stats_json metrics
  stats_json=$(sed -n 's/^BDD_SCCACHE_STATS=//p' "$build_log" | tail -n 1)
  [ -n "$stats_json" ] || fail "sccache did not report compiler statistics"
  metrics=$(python3 - "$stats_json" <<'PY'
import json
import sys

data = json.loads(sys.argv[1])["stats"]

def total(name):
    value = data[name]
    return sum(value.get("counts", {}).values())

print(data["compile_requests"], total("cache_hits"), total("cache_misses"),
      data["compilations"], data["cache_write_errors"], sep="\t")
PY
  )
  IFS=$'\t' read -r SCCACHE_REQUESTS SCCACHE_HITS SCCACHE_MISSES SCCACHE_COMPILATIONS SCCACHE_WRITE_ERRORS <<< "$metrics"
  [ "$SCCACHE_REQUESTS" -gt 0 ] || fail "source build issued no rustc requests through sccache"
  [ "$SCCACHE_WRITE_ERRORS" -eq 0 ] || fail "sccache reported write errors"
  append_summary "- sccache compile requests: $SCCACHE_REQUESTS"
  append_summary "- sccache hits: $SCCACHE_HITS"
  append_summary "- sccache misses: $SCCACHE_MISSES"
  append_summary "- sccache compilations: $SCCACHE_COMPILATIONS"
  append_summary "- sccache write errors: $SCCACHE_WRITE_ERRORS"
  printf 'sccache requests=%s hits=%s misses=%s compilations=%s write_errors=%s\n' \
    "$SCCACHE_REQUESTS" "$SCCACHE_HITS" "$SCCACHE_MISSES" "$SCCACHE_COMPILATIONS" "$SCCACHE_WRITE_ERRORS" >&2
}

build_app_layer() {
  local deps_ref pg_ref output plan build_log start elapsed
  deps_ref=${BDD_DEPENDENCY_DIGEST_REF:-}
  pg_ref=${BDD_PG_BASE_DIGEST_REF:-}
  output=${BDD_APP_LAYER:-app-layer.tar}
  [[ "$deps_ref" == *@sha256:* ]] || fail "BDD_DEPENDENCY_DIGEST_REF must be immutable"
  [[ "$pg_ref" == *@sha256:* ]] || fail "BDD_PG_BASE_DIGEST_REF must be immutable"
  verify_dependency_image "$deps_ref" "$(deps_key)"
  verify_pg_base "$pg_ref" "$(deps_key)"

  plan=$(mktemp)
  build_log=$(mktemp)
  start=$SECONDS
  if ! run_app_nix "$deps_ref" plan > "$plan" 2>&1; then
    cat "$plan" >&2
    rm -f "$plan" "$build_log"
    return 1
  fi
  cat "$plan" >&2
  assert_source_only_plan "$plan"
  if ! run_app_nix "$deps_ref" build > "$output" 2> "$build_log"; then
    cat "$build_log" >&2
    rm -f "$plan" "$build_log" "$output"
    return 1
  fi
  cat "$build_log" >&2
  assert_source_only_plan "$build_log"
  test -s "$output" || fail "application layer is empty"
  elapsed=$((SECONDS - start))
  append_summary "## BDD source-only build"
  append_summary ""
  append_summary "- dependency key: \`$(deps_key)\`"
  append_summary "- dependency digest: \`$deps_ref\`"
  append_summary "- PostgreSQL base digest: \`$pg_ref\`"
  append_summary "- source key: \`$(source_key)\`"
  append_summary "- compiler cache key: \`$(compiler_cache_key)\`"
  append_summary "- compiler cache mode: \`${BDD_SCCACHE_MODE:-READ_ONLY}\`"
  append_summary "- Cargo dependency derivations planned/fetched/built: 0"
  append_summary "- PostgreSQL derivations planned/fetched/built: 0"
  report_sccache_stats "$build_log"
  append_summary "- app-layer bytes: $(wc -c < "$output")"
  append_summary "- app build seconds: $elapsed"
  rm -f "$plan" "$build_log"
}

runtime_build_args() {
  local pg_ref=$1 key=$2
  printf '%s\n' \
    --build-arg "BDD_PG_BASE=$pg_ref" \
    --build-arg "BDD_APP_KEY=$key" \
    --build-arg "BDD_DEPENDENCY_KEY=$(deps_key)" \
    --build-arg "BDD_SOURCE_COMMIT=${BDD_SOURCE_COMMIT:-$(git rev-parse HEAD 2>/dev/null || true)}"
}

assemble_runtime() {
  local pg_ref deps_ref layer key runtime context
  deps_ref=${BDD_DEPENDENCY_DIGEST_REF:-}
  pg_ref=${BDD_PG_BASE_DIGEST_REF:-}
  layer=${BDD_APP_LAYER:-app-layer.tar}
  [[ "$pg_ref" == *@sha256:* ]] || fail "BDD_PG_BASE_DIGEST_REF must be immutable"
  [ -s "$layer" ] || fail "application layer does not exist: $layer"
  key=$(app_key "$deps_ref" "$pg_ref")
  runtime=${BDD_RUNTIME_IMAGE:-$(runtime_image "$deps_ref" "$pg_ref")}
  context=$(mktemp -d)
  cp "$layer" "$context/app-layer.tar"
  mapfile -t args < <(runtime_build_args "$pg_ref" "$key")
  docker_cmd buildx build \
    -f "$(repo_root)/Dockerfile.bdd-app" \
    --platform "$(platform)" \
    "${args[@]}" \
    --load \
    -t "$runtime" \
    "$context"
  rm -rf "$context"
  printf '%s\n' "$runtime"
}

publish_runtime() {
  local pg_ref deps_ref layer key runtime context
  trusted_publish
  [ "${BDD_RUNTIME_PUSH:-0}" = "1" ] || fail "BDD_RUNTIME_PUSH=1 is required"
  deps_ref=${BDD_DEPENDENCY_DIGEST_REF:-}
  pg_ref=${BDD_PG_BASE_DIGEST_REF:-}
  layer=${BDD_APP_LAYER:-app-layer.tar}
  [[ "$pg_ref" == *@sha256:* ]] || fail "BDD_PG_BASE_DIGEST_REF must be immutable"
  [ -s "$layer" ] || fail "application layer does not exist: $layer"
  key=$(app_key "$deps_ref" "$pg_ref")
  runtime=${BDD_RUNTIME_IMAGE:-$(runtime_image "$deps_ref" "$pg_ref")}
  if resolve_ref "$runtime" >/dev/null 2>&1; then
    printf 'runtime_hit=true\nruntime=%s\n' "$runtime"
    return
  fi
  context=$(mktemp -d)
  cp "$layer" "$context/app-layer.tar"
  mapfile -t args < <(runtime_build_args "$pg_ref" "$key")
  docker_cmd buildx build \
    -f "$(repo_root)/Dockerfile.bdd-app" \
    --platform "$(platform)" \
    "${args[@]}" \
    --push \
    -t "$runtime" \
    "$context"
  rm -rf "$context"
  resolve_ref "$runtime" >/dev/null || fail "published runtime has no digest"
  printf 'runtime_hit=false\nruntime=%s\n' "$runtime"
}

build_runtime() {
  local output
  output=${BDD_APP_LAYER:-app-layer.tar}
  if [ "${BDD_RUNTIME_REUSE_LOCAL:-1}" = "1" ]; then
    local runtime
    runtime=${BDD_RUNTIME_IMAGE:-$(runtime_image "${BDD_DEPENDENCY_DIGEST_REF:-dependency-key:$(deps_key)}" "${BDD_PG_BASE_DIGEST_REF:-pg-key:$(deps_key)}")}
    if docker_cmd image inspect "$runtime" >/dev/null 2>&1; then
      printf '%s\n' "$runtime"
      return
    fi
  fi
  BDD_APP_LAYER=$output build_app_layer
  BDD_APP_LAYER=$output assemble_runtime
}

run_runtime() {
  local image
  if [ "$#" -gt 0 ] && [[ "$1" != -* ]]; then
    image=$1
    shift
  else
    image=$(runtime_image "${BDD_DEPENDENCY_DIGEST_REF:-dependency-key:$(deps_key)}" "${BDD_PG_BASE_DIGEST_REF:-pg-key:$(deps_key)}")
  fi
  local args=(--rm)
  if [ -n "${DEBUG:-}" ]; then
    args+=(-e "DEBUG=$DEBUG")
  fi
  docker_cmd run "${args[@]}" "$image" "$@"
}

cmd=${1:-}
case "$cmd" in
  deps-key) deps_key ;;
  source-key|image-key) source_key ;;
  compiler-cache-key) compiler_cache_key ;;
  app-key) shift; app_key "$@" ;;
  keys-json) keys_json ;;
  deps-paths|builder-paths) print_dependency_paths ;;
  image-paths) print_git_paths "${APP_KEY_PATHS[@]}" ;;
  dependency-image) dependency_image; printf '\n' ;;
  pg-base-image) pg_base_image; printf '\n' ;;
  runtime-image) shift; runtime_image "$@"; printf '\n' ;;
  resolve-ref) shift; [ "$#" -eq 1 ] || fail "resolve-ref requires one image"; resolve_ref "$1"; printf '\n' ;;
  platform) platform; printf '\n' ;;
  platform-slug) platform_slug; printf '\n' ;;
  dependency-context-tar|builder-context-tar) dependency_context_tar ;;
  build-dependencies|build-builder) build_dependencies ;;
  resolve-dependencies) resolve_dependencies ;;
  verify-pg-runtime) shift; verify_pg_runtime "$@" ;;
  assert-source-only-plan) shift; [ "$#" -eq 1 ] || fail "assert-source-only-plan requires a file"; assert_source_only_plan "$1" ;;
  build-app-layer) build_app_layer ;;
  assemble-runtime) assemble_runtime ;;
  publish-runtime) publish_runtime ;;
  build-runtime) build_runtime ;;
  run) shift; run_runtime "$@" ;;
  -h|--help|help|'') usage ;;
  *) usage >&2; exit 2 ;;
esac
