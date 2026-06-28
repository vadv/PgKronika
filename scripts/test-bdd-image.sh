#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$ROOT/scripts/bdd-image.sh"
TEST_TMP=$(mktemp -d)
trap 'rm -rf "$TEST_TMP"' EXIT

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

for test in \
  test_exact_hit_pulls_and_does_not_build \
  test_branch_cache_digest_used_for_miss \
  test_main_cache_is_fallback_after_branch_cache_miss \
  test_branch_cache_can_be_disabled \
  test_push_updates_exact_but_not_branch_cache_by_default \
  test_exact_hit_updates_branch_cache_when_enabled \
  test_branch_cache_updates_only_when_enabled \
  test_exact_tag_is_not_overwritten_if_it_appears_before_push \
  test_branch_slug_is_tag_safe
do
  "$test"
done

echo "scripts/test-bdd-image.sh: ok"
