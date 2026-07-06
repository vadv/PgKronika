#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$ROOT/scripts/bdd-image.sh"
TEST_TMP=$(mktemp -d)

cleanup() {
  if [ -f "$TEST_TMP/kronika-source-log.Cargo.toml.bak" ]; then
    cp "$TEST_TMP/kronika-source-log.Cargo.toml.bak" \
      "$ROOT/crates/kronika-source-log/Cargo.toml"
  fi
  rm -rf "$TEST_TMP"
  rm -f \
    "$ROOT/scripts/.cache-key-host-only-probe" \
    "$ROOT/crates/kronika-source-log/src/cache_key_probe.rs" \
    "$ROOT/crates/kronika-bdd/features/cache_key_probe.feature"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  local file=$1
  local text=$2
  grep -F -- "$text" "$file" >/dev/null || fail "expected log to contain: $text"
}

assert_not_contains() {
  local file=$1
  local text=$2
  if grep -F -- "$text" "$file" >/dev/null; then
    fail "expected log not to contain: $text"
  fi
}

assert_eq() {
  local actual=$1
  local expected=$2
  if [ "$actual" != "$expected" ]; then
    fail "expected '$expected', got '$actual'"
  fi
}

hash_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

builder_context_content_key() {
  local context
  context=$(mktemp -d "$TEST_TMP/builder-context-key.XXXXXX")
  "$SCRIPT" builder-context-tar | tar -C "$context" -xf -
  (
    cd "$context"
    find . -type f -print0 \
      | LC_ALL=C sort -z \
      | while IFS= read -r -d '' path; do
          printf '%s\0' "${path#./}"
          cat "$path"
          printf '\0'
        done
  ) | hash_stdin
}

make_mock_docker() {
  local dir=$1
  cat > "$dir/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >> "$MOCK_DOCKER_LOG"

if [ "$1" = "info" ]; then
  case "$3" in
    '{{.OSType}}') echo linux ;;
    '{{.Architecture}}') echo x86_64 ;;
  esac
  exit 0
fi

if [ "$1" = "image" ] && [ "$2" = "inspect" ]; then
  case "$3" in
    *local-hit*) exit 0 ;;
    *) exit 1 ;;
  esac
fi

if [ "$1" = "manifest" ] && [ "$2" = "inspect" ]; then
  case "$3" in
    *exact-hit*) exit 0 ;;
    *exact-appeared*) exit 0 ;;
    *branch-feature-one) [ "${MOCK_BRANCH_CACHE_EXISTS:-0}" = "1" ] && exit 0 || exit 1 ;;
    *branch-main) [ "${MOCK_MAIN_CACHE_EXISTS:-0}" = "1" ] && exit 0 || exit 1 ;;
    *) exit 1 ;;
  esac
fi

if [ "$1" = "buildx" ] && [ "$2" = "imagetools" ] && [ "$3" = "inspect" ]; then
  if [[ "$4" == *branch-feature-one ]] && [ "${MOCK_BRANCH_CACHE_EXISTS:-0}" = "1" ]; then
    echo "sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000"
    exit 0
  fi
  if [[ "$4" == *branch-main ]] && [ "${MOCK_MAIN_CACHE_EXISTS:-0}" = "1" ]; then
    echo "sha256:aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999"
    exit 0
  fi
  exit 1
fi

if [ "$1" = "run" ]; then
  cat >/dev/null || true
  exit 0
fi

case "$1" in
  buildx|pull|push|tag)
    exit 0
    ;;
esac

exit 0
EOF
  chmod +x "$dir/docker"
}

run_case() {
  local name=$1
  shift
  local tmp
  tmp=$(mktemp -d "$TEST_TMP/$name.XXXXXX")
  make_mock_docker "$tmp"
  export MOCK_DOCKER_LOG="$tmp/docker.log"
  export BDD_DOCKER="$tmp/docker"
  export BDD_PLATFORM=linux/amd64
  export BDD_BRANCH_NAME=feature/one
  export BDD_BUILDER_BRANCH_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
  export BDD_BUILDER_MAIN_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-main"
  : > "$MOCK_DOCKER_LOG"
  "$@"
  echo "$MOCK_DOCKER_LOG"
}

test_exact_hit_pulls_and_does_not_build() {
  local log
  log=$(run_case exact-hit env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-hit" \
    BDD_BUILDER_PULL=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "manifest inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-hit"
  assert_contains "$log" "pull ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-hit"
  assert_not_contains "$log" "buildx build"
}

test_local_exact_builder_skips_pull_and_build() {
  local log
  log=$(run_case local-hit env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-local-hit" \
    BDD_BUILDER_PULL=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "image inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-local-hit"
  assert_not_contains "$log" "manifest inspect"
  assert_not_contains "$log" "buildx build"
}

test_branch_cache_digest_used_for_miss() {
  local log
  log=$(run_case branch-cache-hit env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new" \
    BDD_BUILDER_PULL=1 \
    MOCK_BRANCH_CACHE_EXISTS=1 \
    MOCK_MAIN_CACHE_EXISTS=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "manifest inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new"
  assert_contains "$log" "manifest inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
  assert_contains "$log" "buildx imagetools inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one --format {{.Manifest.Digest}}"
  assert_not_contains "$log" "buildx imagetools inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-main"
  assert_contains "$log" "--build-arg BDD_BUILDER_BASE=ghcr.io/acme/pgkronika/pgkronika-bdd-builder@sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000"
}

test_main_cache_is_fallback_after_branch_cache_miss() {
  local log
  log=$(run_case main-cache-hit env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new" \
    BDD_BUILDER_PULL=1 \
    MOCK_MAIN_CACHE_EXISTS=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "manifest inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
  assert_contains "$log" "manifest inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-main"
  assert_contains "$log" "buildx imagetools inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-main --format {{.Manifest.Digest}}"
  assert_contains "$log" "--build-arg BDD_BUILDER_BASE=ghcr.io/acme/pgkronika/pgkronika-bdd-builder@sha256:aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999"
}

test_branch_cache_can_be_disabled() {
  local log
  log=$(run_case branch-cache-disabled env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new" \
    BDD_BUILDER_PULL=1 \
    BDD_BUILDER_USE_BRANCH_CACHE=0 \
    MOCK_BRANCH_CACHE_EXISTS=1 \
    MOCK_MAIN_CACHE_EXISTS=1 \
    "$SCRIPT" build-builder)
  assert_not_contains "$log" "branch-feature-one"
  assert_not_contains "$log" "branch-main"
  assert_contains "$log" "--build-arg BDD_BUILDER_BASE=docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894"
}

test_push_updates_exact_but_not_branch_cache_by_default() {
  local log
  log=$(run_case push-no-branch-cache env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new" \
    BDD_BUILDER_PUSH=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "push ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new"
  assert_not_contains "$log" "tag ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
}

test_exact_hit_updates_branch_cache_when_enabled() {
  local log
  log=$(run_case exact-hit-branch-update env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-hit" \
    BDD_BUILDER_PULL=1 \
    BDD_BUILDER_UPDATE_BRANCH_CACHE=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "pull ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-hit"
  assert_contains "$log" "tag ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-hit ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
  assert_contains "$log" "push ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
  assert_not_contains "$log" "buildx build"
}

test_branch_cache_updates_only_when_enabled() {
  local log
  log=$(run_case branch-cache-update env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new" \
    BDD_BUILDER_PUSH=1 \
    BDD_BUILDER_UPDATE_BRANCH_CACHE=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "push ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new"
  assert_contains "$log" "tag ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-new ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
  assert_contains "$log" "push ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-branch-feature-one"
}

test_exact_tag_is_not_overwritten_if_it_appears_before_push() {
  local log
  log=$(run_case exact-appeared env \
    BDD_BUILDER_IMAGE="ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-appeared" \
    BDD_BUILDER_PUSH=1 \
    "$SCRIPT" build-builder)
  assert_contains "$log" "manifest inspect ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-appeared"
  assert_not_contains "$log" "push ghcr.io/acme/pgkronika/pgkronika-bdd-builder:deps-linux-amd64-exact-appeared"
}

test_branch_slug_is_tag_safe() {
  local slug
  slug=$("$SCRIPT" branch-slug "Feature/Add cache@CI")
  assert_eq "$slug" "feature-add-cache-ci"
}

test_run_passes_args_and_debug_to_container() {
  local log
  log=$(run_case run-args env \
    DEBUG=1 \
    "$SCRIPT" run pgkronika-bdd:local --tags @pg_log)
  assert_contains "$log" "run --rm -e DEBUG=1 pgkronika-bdd:local --tags @pg_log"
}

test_runtime_reuse_local_skips_build() {
  local log
  log=$(run_case runtime-local-hit env \
    BDD_RUNTIME_IMAGE="pgkronika-bdd:local-hit" \
    BDD_RUNTIME_REUSE_LOCAL=1 \
    "$SCRIPT" build-runtime)
  assert_contains "$log" "image inspect pgkronika-bdd:local-hit"
  assert_not_contains "$log" "run --rm -i"
  assert_not_contains "$log" "load -i"
}

test_runtime_build_uses_filtered_stdin_tar() {
  local log output
  output="$TEST_TMP/runtime-build.tar"
  log=$(run_case runtime-build env \
    BDD_BUILDER_IMAGE="pgkronika-bdd-builder:test" \
    BDD_RUNTIME_IMAGE="pgkronika-bdd:runtime-build" \
    BDD_OUTPUT_TAR="$output" \
    "$SCRIPT" build-runtime)
  assert_contains "$log" "run --rm -i pgkronika-bdd-builder:test sh -ceu"
  assert_not_contains "$log" "-v $ROOT:/src:ro"
  assert_contains "$log" "load -i $output"
  assert_contains "$log" "tag pgkronika-bdd:latest pgkronika-bdd:runtime-build"
}

test_runtime_paths_exclude_host_only_helpers() {
  local paths
  paths=$("$SCRIPT" image-paths)
  if printf '%s\n' "$paths" | grep -Fx -- "scripts/bdd-image.sh" >/dev/null; then
    fail "runtime paths must not include scripts/bdd-image.sh"
  fi
  if printf '%s\n' "$paths" | grep -Fx -- "scripts/test-bdd-local.sh" >/dev/null; then
    fail "runtime paths must not include scripts/test-bdd-local.sh"
  fi
  if printf '%s\n' "$paths" | grep -Fx -- "Makefile" >/dev/null; then
    fail "runtime paths must not include Makefile"
  fi
  printf '%s\n' "$paths" | grep -Fx -- "crates/kronika-bdd/features/pg_log.feature" >/dev/null \
    || fail "runtime paths must include pg_log.feature"
  printf '%s\n' "$paths" | grep -Fx -- "crates/kronika-bdd/src/main.rs" >/dev/null \
    || fail "runtime paths must include kronika-bdd source"
}

test_builder_paths_are_deps_only() {
  local paths
  paths=$("$SCRIPT" builder-paths)
  if printf '%s\n' "$paths" | grep -Fx -- "scripts/bdd-image.sh" >/dev/null; then
    fail "builder paths must not include scripts/bdd-image.sh"
  fi
  if printf '%s\n' "$paths" | grep -Fx -- "Makefile" >/dev/null; then
    fail "builder paths must not include Makefile"
  fi
  if printf '%s\n' "$paths" | grep -E '/src/.*\.rs$' >/dev/null; then
    fail "builder paths must not include Rust source files"
  fi
  printf '%s\n' "$paths" | grep -Fx -- "crates/kronika-source-log/Cargo.toml" >/dev/null \
    || fail "builder paths must include crate manifests"
}

test_builder_context_tar_has_stable_dummy_targets() {
  local context
  context=$(mktemp -d "$TEST_TMP/builder-context.XXXXXX")
  "$SCRIPT" builder-context-tar | tar -C "$context" -xf -
  grep -Fx -- '#![allow(missing_docs)]' "$context/crates/kronika-format/src/lib.rs" >/dev/null \
    || fail "builder context must contain dummy crate lib target"
  grep -Fx -- 'fn main() {}' "$context/crates/kronika-bdd/src/main.rs" >/dev/null \
    || fail "builder context must contain dummy BDD bin target"
  grep -Fx -- 'fn main() {}' "$context/xtask/src/main.rs" >/dev/null \
    || fail "builder context must contain dummy xtask target"
}

test_runtime_key_ignores_host_only_files() {
  local before after probe
  probe="$ROOT/scripts/.cache-key-host-only-probe"
  before=$("$SCRIPT" image-key)
  printf 'host helper only\n' > "$probe"
  after=$("$SCRIPT" image-key)
  rm -f "$probe"
  assert_eq "$after" "$before"
}

test_rust_source_changes_runtime_but_not_deps_or_builder() {
  local before_deps before_image before_builder_paths before_builder_context
  local after_deps after_image after_builder_paths after_builder_context probe
  probe="$ROOT/crates/kronika-source-log/src/cache_key_probe.rs"
  before_deps=$("$SCRIPT" deps-key)
  before_image=$("$SCRIPT" image-key)
  before_builder_paths=$("$SCRIPT" builder-paths | hash_stdin)
  before_builder_context=$(builder_context_content_key)
  printf 'pub(crate) fn cache_key_probe() {}\n' > "$probe"
  after_deps=$("$SCRIPT" deps-key)
  after_image=$("$SCRIPT" image-key)
  after_builder_paths=$("$SCRIPT" builder-paths | hash_stdin)
  after_builder_context=$(builder_context_content_key)
  rm -f "$probe"
  assert_eq "$after_deps" "$before_deps"
  assert_eq "$after_builder_paths" "$before_builder_paths"
  assert_eq "$after_builder_context" "$before_builder_context"
  if [ "$after_image" = "$before_image" ]; then
    fail "runtime image key must change when Rust source changes"
  fi
}

test_dependency_manifest_changes_deps_key() {
  local before after manifest backup
  manifest="$ROOT/crates/kronika-source-log/Cargo.toml"
  backup="$TEST_TMP/kronika-source-log.Cargo.toml.bak"
  cp "$manifest" "$backup"
  before=$("$SCRIPT" deps-key)
  printf '\n# cache key dependency-input probe\n' >> "$manifest"
  after=$("$SCRIPT" deps-key)
  cp "$backup" "$manifest"
  rm -f "$backup"
  if [ "$after" = "$before" ]; then
    fail "deps key must change when a Cargo.toml input changes"
  fi
}

test_runtime_key_changes_for_feature_inputs() {
  local before after probe
  probe="$ROOT/crates/kronika-bdd/features/cache_key_probe.feature"
  before=$("$SCRIPT" image-key)
  printf 'Feature: cache key probe\n' > "$probe"
  after=$("$SCRIPT" image-key)
  rm -f "$probe"
  if [ "$after" = "$before" ]; then
    fail "runtime image key must change when a BDD feature changes"
  fi
}

for test in \
  test_local_exact_builder_skips_pull_and_build \
  test_exact_hit_pulls_and_does_not_build \
  test_branch_cache_digest_used_for_miss \
  test_main_cache_is_fallback_after_branch_cache_miss \
  test_branch_cache_can_be_disabled \
  test_push_updates_exact_but_not_branch_cache_by_default \
  test_exact_hit_updates_branch_cache_when_enabled \
  test_branch_cache_updates_only_when_enabled \
  test_exact_tag_is_not_overwritten_if_it_appears_before_push \
  test_branch_slug_is_tag_safe \
  test_run_passes_args_and_debug_to_container \
  test_runtime_reuse_local_skips_build \
  test_runtime_build_uses_filtered_stdin_tar \
  test_runtime_paths_exclude_host_only_helpers \
  test_builder_paths_are_deps_only \
  test_builder_context_tar_has_stable_dummy_targets \
  test_runtime_key_ignores_host_only_files \
  test_rust_source_changes_runtime_but_not_deps_or_builder \
  test_dependency_manifest_changes_deps_key \
  test_runtime_key_changes_for_feature_inputs
do
  "$test"
done

echo "scripts/test-bdd-image.sh: ok"
