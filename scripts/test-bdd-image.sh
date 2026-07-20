#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$ROOT/scripts/bdd-image.sh"
TEST_TMP=$(mktemp -d)
unset GITHUB_RUN_ID GITHUB_RUN_ATTEMPT

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  local file=$1 text=$2
  grep -F -- "$text" "$file" >/dev/null || fail "expected log to contain: $text"
}

assert_not_contains() {
  local file=$1 text=$2
  if grep -F -- "$text" "$file" >/dev/null; then
    fail "expected log not to contain: $text"
  fi
}

assert_eq() {
  local actual=$1 expected=$2
  [ "$actual" = "$expected" ] || fail "expected '$expected', got '$actual'"
}

assert_ne() {
  local actual=$1 unexpected=$2
  [ "$actual" != "$unexpected" ] || fail "did not expect '$unexpected'"
}

hash_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

make_repo_copy() {
  local copy
  copy=$(mktemp -d "$TEST_TMP/repo-copy.XXXXXX")
  (
    cd "$ROOT"
    git ls-files -z | tar --null -T - -cf -
  ) | tar -C "$copy" -xf -
  (
    cd "$copy"
    git init -q
    git add -A
  )
  printf '%s\n' "$copy"
}

run_bdd_image_script() {
  local script=$1
  shift
  (
    cd "$(dirname "$script")/.."
    "$script" "$@"
  )
}

builder_context_content_key() {
  local script=${1:-$SCRIPT} context
  context=$(mktemp -d "$TEST_TMP/builder-context-key.XXXXXX")
  run_bdd_image_script "$script" builder-context-tar | tar -C "$context" -xf -
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
  if [ "$#" -eq 1 ]; then
    echo info
    exit 0
  fi
  case "${3:-}" in
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
    *exact-hit*|*exact-appeared*) exit 0 ;;
    *) exit 1 ;;
  esac
fi

if [ "$1" = "run" ]; then
  case " $* " in
    *' -i '*) cat >/dev/null ;;
  esac
  exit 0
fi

case "$1" in
  buildx|load|pull|push|tag)
    exit 0
    ;;
esac

exit 0
EOF
  chmod +x "$dir/docker"
}

run_case() {
  local name=$1 tmp
  shift
  tmp=$(mktemp -d "$TEST_TMP/$name.XXXXXX")
  make_mock_docker "$tmp"
  export MOCK_DOCKER_LOG="$tmp/docker.log"
  export BDD_DOCKER="$tmp/docker"
  export BDD_PLATFORM=linux/amd64
  : > "$MOCK_DOCKER_LOG"
  "$@"
  echo "$MOCK_DOCKER_LOG"
}

test_exact_builder_hit_pulls_only_exact_builder() {
  local log image
  image=ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-exact-hit
  log=$(run_case exact-hit env BDD_BUILDER_IMAGE="$image" BDD_BUILDER_PULL=1 "$SCRIPT" build-builder)
  assert_contains "$log" "manifest inspect $image"
  assert_contains "$log" "pull $image"
  assert_not_contains "$log" "buildx"
  assert_not_contains "$log" "tag "
  assert_not_contains "$log" "push "
  assert_not_contains "$log" "branch"
}

test_local_exact_builder_skips_pull_and_build() {
  local log image
  image=ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-local-hit
  log=$(run_case local-hit env BDD_BUILDER_IMAGE="$image" BDD_BUILDER_PULL=1 "$SCRIPT" build-builder)
  assert_contains "$log" "image inspect $image"
  assert_not_contains "$log" "manifest inspect"
  assert_not_contains "$log" "pull "
  assert_not_contains "$log" "buildx"
}

test_builder_miss_uses_pinned_base_and_pushes_only_exact_tag() {
  local log image
  image=ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-new
  log=$(run_case builder-miss env BDD_BUILDER_IMAGE="$image" BDD_BUILDER_PUSH=1 "$SCRIPT" build-builder)
  assert_contains "$log" "buildx build"
  assert_contains "$log" "--build-arg BDD_BUILDER_BASE=docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894"
  assert_contains "$log" "push $image"
  assert_not_contains "$log" "branch"
  assert_not_contains "$log" "cache-from"
  assert_not_contains "$log" "cache-to"
}

test_exact_tag_is_not_overwritten_if_it_appears_before_push() {
  local log image
  image=ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-exact-appeared
  log=$(run_case exact-appeared env BDD_BUILDER_IMAGE="$image" BDD_BUILDER_PUSH=1 "$SCRIPT" build-builder)
  assert_contains "$log" "manifest inspect $image"
  assert_contains "$log" "pull $image"
  assert_not_contains "$log" "push $image"
}

test_runtime_tag_is_local_or_github_run_scoped() {
  assert_eq "$(BDD_PLATFORM=linux/amd64 "$SCRIPT" runtime-image)" "pgkronika-bdd:local"
  assert_eq "$(GITHUB_RUN_ID=123 GITHUB_RUN_ATTEMPT=4 BDD_PLATFORM=linux/amd64 "$SCRIPT" runtime-image)" \
    "pgkronika-bdd:run-123-4"
}

test_runtime_build_always_compiles_from_filtered_source_tar() {
  local log output
  output="$TEST_TMP/runtime-build.tar"
  log=$(run_case runtime-build env \
    BDD_BUILDER_IMAGE=pgkronika-bdd-builder:test \
    BDD_RUNTIME_IMAGE=pgkronika-bdd:run-123-1 \
    BDD_OUTPUT_TAR="$output" \
    "$SCRIPT" build-runtime)
  assert_contains "$log" "run --rm -i pgkronika-bdd-builder:test sh -ceu"
  assert_contains "$log" "load -i $output"
  assert_contains "$log" "tag pgkronika-bdd:latest pgkronika-bdd:run-123-1"
  assert_not_contains "$log" "image inspect pgkronika-bdd"
  assert_not_contains "$log" "manifest inspect pgkronika-bdd"
  assert_not_contains "$log" "push pgkronika-bdd"
}

test_local_runner_always_assembles_ephemeral_runtime() {
  local tmp log stdout
  tmp=$(mktemp -d "$TEST_TMP/local-runner.XXXXXX")
  make_mock_docker "$tmp"
  log="$tmp/docker.log"
  stdout="$tmp/stdout.log"
  : > "$log"
  (
    export MOCK_DOCKER_LOG="$log"
    export BDD_DOCKER="$tmp/docker"
    export BDD_PLATFORM=linux/amd64
    export BDD_BUILDER_IMAGE=ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-exact-hit
    DEBUG=1 TAGS=@pg_log "$ROOT/scripts/test-bdd-local.sh"
  ) > "$stdout" 2>&1
  assert_contains "$stdout" "Building ephemeral BDD runtime image pgkronika-bdd:local"
  assert_contains "$log" "pull ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-exact-hit"
  assert_contains "$log" "run --rm -i ghcr.io/acme/pgkronika-bdd-builder:builder-linux-amd64-exact-hit sh -ceu"
  assert_contains "$log" "tag pgkronika-bdd:latest pgkronika-bdd:local"
  assert_contains "$log" "run --rm -e DEBUG=1 pgkronika-bdd:local -vvv --tags @pg_log"
  assert_not_contains "$log" "buildx version"
  assert_not_contains "$log" "push "
}

test_runtime_source_paths_are_complete_and_not_a_key() {
  local paths
  paths=$($SCRIPT runtime-paths)
  printf '%s\n' "$paths" | grep -Fx -- crates/kronika-bdd/features/pg_log.feature >/dev/null \
    || fail "runtime sources must include BDD features"
  printf '%s\n' "$paths" | grep -Fx -- crates/kronika-bdd/src/main.rs >/dev/null \
    || fail "runtime sources must include BDD runner source"
  printf '%s\n' "$paths" | grep -Fx -- bins/pg_kronika-collector/src/main.rs >/dev/null \
    || fail "runtime sources must include collector source"
  if printf '%s\n' "$paths" | grep -Fx -- scripts/bdd-image.sh >/dev/null; then
    fail "runtime sources must exclude host-only helper"
  fi
  if "$SCRIPT" image-key >/dev/null 2>&1; then
    fail "source-derived runtime image key command must not exist"
  fi
}

test_runtime_sources_include_build_scripts_when_present() {
  local repo script paths
  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  printf 'fn main() {}\n' > "$repo/crates/kronika-source-log/build.rs"
  paths=$(run_bdd_image_script "$script" runtime-paths)
  printf '%s\n' "$paths" | grep -Fx -- crates/kronika-source-log/build.rs >/dev/null \
    || fail "runtime sources must include package build scripts"
}

test_ordinary_source_and_features_do_not_change_dependency_identity() {
  local repo script base after_add after_edit after_remove after_feature context_before context_after
  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  base=$(run_bdd_image_script "$script" deps-key)
  context_before=$(builder_context_content_key "$script")
  printf 'pub(crate) fn cache_key_probe() {}\n' > "$repo/crates/kronika-source-log/src/cache_key_probe.rs"
  after_add=$(run_bdd_image_script "$script" deps-key)
  printf '\n// source edit probe\n' >> "$repo/crates/kronika-source-log/src/parser.rs"
  after_edit=$(run_bdd_image_script "$script" deps-key)
  rm "$repo/crates/kronika-source-log/src/parser.rs"
  after_remove=$(run_bdd_image_script "$script" deps-key)
  printf 'Feature: dependency identity probe\n' > "$repo/crates/kronika-bdd/features/cache_key_probe.feature"
  after_feature=$(run_bdd_image_script "$script" deps-key)
  context_after=$(builder_context_content_key "$script")
  assert_eq "$after_add" "$base"
  assert_eq "$after_edit" "$base"
  assert_eq "$after_remove" "$base"
  assert_eq "$after_feature" "$base"
  assert_eq "$context_after" "$context_before"
}

test_dependency_contract_files_change_dependency_key() {
  local path repo script before after
  for path in \
    Cargo.toml \
    Cargo.lock \
    rust-toolchain.toml \
    flake.nix \
    flake.lock \
    Dockerfile.bdd-builder \
    crates/kronika-source-log/Cargo.toml
  do
    repo=$(make_repo_copy)
    script="$repo/scripts/bdd-image.sh"
    before=$(run_bdd_image_script "$script" deps-key)
    printf '\n# dependency contract probe\n' >> "$repo/$path"
    after=$(run_bdd_image_script "$script" deps-key)
    assert_ne "$after" "$before"
  done

  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  before=$(run_bdd_image_script "$script" deps-key)
  after=$(BDD_NIX_BASE_IMAGE=docker.io/nixos/nix:test run_bdd_image_script "$script" deps-key)
  assert_ne "$after" "$before"
}

test_dependency_paths_cover_every_manifest() {
  local expected actual
  expected=$(
    cd "$ROOT"
    find . -path './.git' -prune -o -name Cargo.toml -print | sed 's#^./##' | LC_ALL=C sort
  )
  actual=$($SCRIPT deps-paths | grep 'Cargo.toml$' | LC_ALL=C sort)
  assert_eq "$actual" "$expected"
}

test_implicit_cargo_target_topology_is_dependency_contract() {
  local repo script base added edited
  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  base=$(run_bdd_image_script "$script" deps-key)
  mkdir -p "$repo/crates/kronika-source-log/src/bin"
  printf 'fn main() {}\n' > "$repo/crates/kronika-source-log/src/bin/key_probe.rs"
  added=$(run_bdd_image_script "$script" deps-key)
  assert_ne "$added" "$base"
  printf 'fn main() { println!("body"); }\n' > "$repo/crates/kronika-source-log/src/bin/key_probe.rs"
  edited=$(run_bdd_image_script "$script" deps-key)
  assert_eq "$edited" "$added"
}

test_builder_context_has_stable_dummy_targets() {
  local context
  context=$(mktemp -d "$TEST_TMP/builder-context.XXXXXX")
  "$SCRIPT" builder-context-tar | tar -C "$context" -xf -
  grep -Fx -- '#![allow(missing_docs)]' "$context/crates/kronika-format/src/lib.rs" >/dev/null \
    || fail "builder context must contain dummy crate lib target"
  grep -Fx -- 'fn main() {}' "$context/crates/kronika-bdd/src/main.rs" >/dev/null \
    || fail "builder context must contain dummy BDD bin target"
  grep -Fx -- 'fn main() {}' "$context/bins/pg_kronika-collector/src/main.rs" >/dev/null \
    || fail "builder context must contain dummy collector target"
}

test_workflow_has_only_exact_builder_cache_identity() {
  local workflow
  workflow="$ROOT/.github/workflows/ci.yml"
  assert_contains "$workflow" 'deps_hash=$(./scripts/bdd-image.sh deps-key)'
  assert_contains "$workflow" 'BDD builder exact hit'
  assert_contains "$workflow" 'BDD builder exact miss'
  assert_contains "$workflow" 'BDD_RUNTIME_IMAGE: pgkronika-bdd:run-${{ github.run_id }}-${{ github.run_attempt }}'
  assert_contains "$workflow" "echo \"BDD source build seconds: \${elapsed}\""
  assert_contains "$workflow" '- source build: always runs; clean hosted runners do not cache first-party compilation'
  assert_contains "$workflow" "if: steps.builder.outputs.exists != 'true'"
  assert_contains "$workflow" "if: steps.builder.outputs.exists != 'true' && steps.meta.outputs.can_push == 'true'"
  assert_contains "$workflow" "BDD_BUILDER_CAN_PUSH: \${{ github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository }}"
  assert_not_contains "$workflow" 'image-key'
  assert_not_contains "$workflow" 'image_hash'
  assert_not_contains "$workflow" 'Check cached BDD image'
  assert_not_contains "$workflow" 'Push BDD image'
  assert_not_contains "$workflow" 'builder_branch'
  assert_not_contains "$workflow" 'branch-main'
  assert_not_contains "$workflow" 'concurrency:'
  assert_not_contains "$workflow" 'ghcr.io/${owner}/pgkronika-bdd:'
  assert_not_contains "$workflow" 'BDD_RUNTIME_PUSH'
  assert_not_contains "$workflow" 'BDD_RUNTIME_REUSE_LOCAL'
}

for test in \
  test_exact_builder_hit_pulls_only_exact_builder \
  test_local_exact_builder_skips_pull_and_build \
  test_builder_miss_uses_pinned_base_and_pushes_only_exact_tag \
  test_exact_tag_is_not_overwritten_if_it_appears_before_push \
  test_runtime_tag_is_local_or_github_run_scoped \
  test_runtime_build_always_compiles_from_filtered_source_tar \
  test_local_runner_always_assembles_ephemeral_runtime \
  test_runtime_source_paths_are_complete_and_not_a_key \
  test_runtime_sources_include_build_scripts_when_present \
  test_ordinary_source_and_features_do_not_change_dependency_identity \
  test_dependency_contract_files_change_dependency_key \
  test_dependency_paths_cover_every_manifest \
  test_implicit_cargo_target_topology_is_dependency_contract \
  test_builder_context_has_stable_dummy_targets \
  test_workflow_has_only_exact_builder_cache_identity
do
  "$test"
done

echo "scripts/test-bdd-image.sh: ok"
