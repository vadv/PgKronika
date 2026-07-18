#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$ROOT/scripts/bdd-image.sh"
TEST_TMP=$(mktemp -d)

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

assert_eq() {
  [ "$1" = "$2" ] || fail "expected '$2', got '$1'"
}

assert_contains() {
  grep -F -- "$2" "$1" >/dev/null || fail "expected '$2' in $1"
}

assert_not_contains() {
  if grep -F -- "$2" "$1" >/dev/null; then
    fail "did not expect '$2' in $1"
  fi
}

make_repo_copy() {
  local copy
  copy=$(mktemp -d "$TEST_TMP/repo.XXXXXX")
  (
    cd "$ROOT"
    git ls-files -co --exclude-standard -z | tar --null -T - -cf -
  ) | tar -C "$copy" -xf -
  (
    cd "$copy"
    git init -q
    git add -A
  )
  printf '%s\n' "$copy"
}

run_script() {
  local script=$1
  shift
  (
    cd "$(dirname "$script")/.."
    BDD_PLATFORM=linux/amd64 "$script" "$@"
  )
}

mutate_and_key() {
  local file=$1 kind=$2 repo script before after
  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  before=$(run_script "$script" deps-key)
  printf '\n# dependency-contract-test\n' >> "$repo/$file"
  after=$(run_script "$script" deps-key)
  if [ "$kind" = changes ]; then
    [ "$before" != "$after" ] || fail "$file must change dependency key"
  else
    assert_eq "$after" "$before"
  fi
}

test_keys_are_full_and_machine_readable() {
  local deps source json
  deps=$(run_script "$SCRIPT" deps-key)
  source=$(run_script "$SCRIPT" source-key)
  [[ "$deps" =~ ^[0-9a-f]{64}$ ]] || fail "dependency key is not full SHA-256"
  [[ "$source" =~ ^[0-9a-f]{64}$ ]] || fail "source key is not full SHA-256"
  json=$(run_script "$SCRIPT" keys-json)
  python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["schema"] == 2; assert d["postgresql_majors"] == [15,16,17,18]' <<< "$json"
}

test_default_images_use_flat_ghcr_packages() {
  local dependency pg runtime
  dependency=$(run_script "$SCRIPT" dependency-image)
  pg=$(run_script "$SCRIPT" pg-base-image)
  runtime=$(run_script "$SCRIPT" runtime-image)
  [[ "$dependency" == ghcr.io/vadv/pgkronika-bdd-builder:* ]] \
    || fail "dependency image must use the flat pgkronika-bdd-builder package"
  [[ "$pg" == ghcr.io/vadv/pgkronika-bdd:* ]] \
    || fail "PG image must use the flat pgkronika-bdd package"
  [[ "$runtime" == ghcr.io/vadv/pgkronika-bdd:* ]] \
    || fail "runtime image must use the flat pgkronika-bdd package"
  [[ "$dependency$pg$runtime" != *ghcr.io/vadv/pgkronika/* ]] \
    || fail "nested pgkronika GHCR packages are forbidden"
}

test_complete_dependency_inputs_change_key() {
  local file
  for file in \
    Cargo.lock \
    Cargo.toml \
    crates/kronika-source-pg/Cargo.toml \
    bins/pg_kronika-web/Cargo.toml \
    xtask/Cargo.toml \
    rust-toolchain.toml \
    .cargo/config.toml \
    flake.lock \
    flake.nix \
    Dockerfile.bdd-builder \
    scripts/bdd-image.sh
  do
    mutate_and_key "$file" changes
  done
}

test_source_body_changes_only_source_key() {
  local repo script before_deps after_deps before_source after_source file
  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  file="$repo/bins/pg_kronika-web/src/lib.rs"
  before_deps=$(run_script "$script" deps-key)
  before_source=$(run_script "$script" source-key)
  printf '\n#[cfg(test)] mod cache_contract_probe {}\n' >> "$file"
  after_deps=$(run_script "$script" deps-key)
  after_source=$(run_script "$script" source-key)
  assert_eq "$after_deps" "$before_deps"
  [ "$after_source" != "$before_source" ] || fail "Rust source must change source key"
}

test_target_topology_changes_dependency_key_without_hashing_body() {
  local repo script before after first second target
  repo=$(make_repo_copy)
  script="$repo/scripts/bdd-image.sh"
  target="$repo/bins/pg_kronika-web/src/bin/cache_probe.rs"
  before=$(run_script "$script" deps-key)
  mkdir -p "$(dirname "$target")"
  printf 'fn main() { println!("one"); }\n' > "$target"
  first=$(run_script "$script" deps-key)
  printf 'fn main() { println!("two"); }\n' > "$target"
  second=$(run_script "$script" deps-key)
  [ "$before" != "$first" ] || fail "new Cargo target must change dependency key"
  assert_eq "$second" "$first"
  after=$(run_script "$script" source-key)
  [ -n "$after" ]
}

test_dependency_context_uses_exact_dummy_topology() {
  local context real
  context=$(mktemp -d "$TEST_TMP/context.XXXXXX")
  BDD_PLATFORM=linux/amd64 "$SCRIPT" dependency-context-tar | tar -C "$context" -xf -
  assert_contains "$context/bins/pg_kronika-web/src/lib.rs" '#![allow(missing_docs)]'
  assert_contains "$context/bins/pg_kronika-web/src/main.rs" 'fn main() {}'
  assert_contains "$context/crates/kronika-bdd/src/main.rs" 'fn main() {}'
  assert_contains "$context/crates/kronika-reader/benches/serving.rs" 'fn main() {}'
  real=$(sha256sum "$ROOT/bins/pg_kronika-web/src/lib.rs" | awk '{print $1}')
  if [ "$(sha256sum "$context/bins/pg_kronika-web/src/lib.rs" | awk '{print $1}')" = "$real" ]; then
    fail "dependency context copied Rust source content"
  fi
}

test_pg_matrix_uses_exact_with_packages_closures() {
  local file="$ROOT/flake.nix"
  local major
  for major in 15 16 17 18; do
    grep -F -- "postgresql_${major}_plans = pkgs.postgresql_${major}.withPackages" "$file" >/dev/null \
      || fail "PG$major withPackages closure is missing"
    grep -F -- "path = postgresql_${major}_plans;" "$file" >/dev/null \
      || fail "PG$major is missing from bddPgMatrix"
  done
  grep -F -- 'pg-store-plans-vadv' "$file" >/dev/null || fail "vadv revision input missing"
  grep -F -- 'pg-store-plans-ossc' "$file" >/dev/null || fail "ossc revision input missing"
  assert_contains "$ROOT/Dockerfile.bdd-builder" 'nix build .#bddPgMatrix'
  assert_not_contains "$ROOT/Dockerfile.bdd-builder" '.#postgresql_15 '
}

test_nix_build_declares_musl_compiler() {
  local file="$ROOT/flake.nix"
  assert_contains "$file" 'nativeBuildInputs = [ pkgs.pkgsMusl.stdenv.cc ];'
  assert_contains "$file" 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER ='
  assert_contains "$file" 'CC_x86_64_unknown_linux_musl ='
}

test_source_only_plan_gate() {
  local clean="$TEST_TMP/clean-plan" bad="$TEST_TMP/bad-plan" token
  printf 'these derivations will be built: bdd-app-layer pgkronika-bins\n' > "$clean"
  "$SCRIPT" assert-source-only-plan "$clean"
  for token in pg_store_plans postgresql-and-plugins pgkronika-bdd-pg-matrix postgresql_18 pgkronika-bdd-deps-deps; do
    printf 'will build %s.drv\n' "$token" > "$bad"
    if "$SCRIPT" assert-source-only-plan "$bad" >/dev/null 2>&1; then
      fail "source-only plan accepted $token"
    fi
  done
}

make_mock_docker() {
  local dir=$1
  cat > "$dir/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$MOCK_DOCKER_LOG"

if [ "$1" = info ]; then
  case "${3:-}" in
    '{{.OSType}}') echo linux ;;
    '{{.Architecture}}') echo x86_64 ;;
    *) echo info ;;
  esac
  exit 0
fi

if [ "$1" = manifest ] && [ "$2" = inspect ]; then
  case "$3" in
    *exact-deps*|*exact-pg*|*exact-runtime*) exit 0 ;;
    *) exit 1 ;;
  esac
fi

if [ "$1" = buildx ] && [ "$2" = imagetools ] && [ "$3" = inspect ]; then
  echo sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000
  exit 0
fi

if [ "$1" = image ] && [ "$2" = inspect ]; then
  exit 1
fi

if [ "$1" = run ]; then
  cat >/dev/null || true
  case "${*: -1}" in
    plan) echo 'these derivations will be built: bdd-app-layer pgkronika-bins' ;;
    build) printf 'thin-app-layer' ;;
  esac
  exit 0
fi

case "$1" in
  buildx|pull|push|tag|logout) exit 0 ;;
esac
exit 0
EOF
  chmod +x "$dir/docker"
}

run_mock() {
  local name=$1
  shift
  local dir
  dir=$(mktemp -d "$TEST_TMP/$name.XXXXXX")
  make_mock_docker "$dir"
  export MOCK_DOCKER_LOG="$dir/docker.log"
  export BDD_DOCKER="$dir/docker"
  export BDD_PLATFORM=linux/amd64
  : > "$MOCK_DOCKER_LOG"
  "$@"
}

test_exact_dependency_hit_never_builds_or_mutates_branch_tags() {
  local out="$TEST_TMP/deps-hit.out"
  run_mock deps-hit env \
    BDD_DEPENDENCY_IMAGE=ghcr.io/acme/exact-deps \
    BDD_PG_BASE_IMAGE=ghcr.io/acme/exact-pg \
    "$SCRIPT" build-dependencies > "$out"
  assert_contains "$out" 'dependency_hit=true'
  assert_not_contains "$MOCK_DOCKER_LOG" 'buildx build'
  assert_not_contains "$MOCK_DOCKER_LOG" 'branch-'
  assert_not_contains "$MOCK_DOCKER_LOG" 'push'
}

test_public_consumer_fails_closed_without_dependency() {
  if run_mock missing "$SCRIPT" resolve-dependencies >/dev/null 2>&1; then
    fail "missing immutable dependency must fail closed"
  fi
  assert_not_contains "$MOCK_DOCKER_LOG" 'buildx build'
  assert_not_contains "$MOCK_DOCKER_LOG" 'push'
}

test_publish_requires_explicit_trust() {
  if run_mock untrusted env BDD_DEPENDENCY_PUSH=1 "$SCRIPT" build-dependencies >/dev/null 2>&1; then
    fail "untrusted dependency publication was accepted"
  fi
  assert_not_contains "$MOCK_DOCKER_LOG" 'buildx build'
  assert_not_contains "$MOCK_DOCKER_LOG" 'push'
}

test_app_build_uses_digest_and_reports_zero_pg_work() {
  local layer="$TEST_TMP/app-layer.tar"
  run_mock app-build env \
    BDD_DEPENDENCY_DIGEST_REF=ghcr.io/acme/deps@sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000 \
    BDD_PG_BASE_DIGEST_REF=ghcr.io/acme/pg@sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000 \
    BDD_APP_LAYER="$layer" \
    "$SCRIPT" build-app-layer
  assert_contains "$MOCK_DOCKER_LOG" 'ghcr.io/acme/deps@sha256:'
  assert_not_contains "$MOCK_DOCKER_LOG" 'build-dependencies'
  assert_eq "$(cat "$layer")" 'thin-app-layer'
}

test_runtime_assembly_is_one_layer_over_pg_digest() {
  local layer="$TEST_TMP/assembly-layer.tar" out="$TEST_TMP/runtime.out"
  printf 'thin-app-layer' > "$layer"
  run_mock assembly env \
    BDD_DEPENDENCY_DIGEST_REF=ghcr.io/acme/deps@sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000 \
    BDD_PG_BASE_DIGEST_REF=ghcr.io/acme/pg@sha256:111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000 \
    BDD_APP_LAYER="$layer" \
    BDD_RUNTIME_IMAGE=pgkronika-bdd:test \
    "$SCRIPT" assemble-runtime > "$out"
  assert_contains "$MOCK_DOCKER_LOG" '--build-arg BDD_PG_BASE=ghcr.io/acme/pg@sha256:'
  assert_contains "$ROOT/Dockerfile.bdd-app" 'ADD app-layer.tar /'
  assert_not_contains "$ROOT/Dockerfile.bdd-app" 'nix build'
}

test_workflow_enforces_trust_and_short_circuit() {
  local workflow="$ROOT/.github/workflows/ci.yml"
  assert_contains "$workflow" "if: github.event_name == 'workflow_dispatch' || github.event_name == 'push'"
  assert_contains "$workflow" 'packages: read'
  assert_contains "$workflow" 'packages: write'
  assert_contains "$workflow" "if: steps.final.outputs.hit != 'true'"
  assert_contains "$workflow" "if: steps.final.outputs.hit == 'true'"
  assert_contains "$workflow" 'BDD_TRUSTED_PUBLISH: "1"'
  assert_not_contains "$workflow" 'rustup target add x86_64-unknown-linux-musl'
  assert_contains "$ROOT/rust-toolchain.toml" 'targets = ["x86_64-unknown-linux-musl"]'
  assert_contains "$workflow" 'branches: [main, feat/incident-first-slice]'
  assert_contains "$workflow" 'BDD_DEPENDENCY_MISSING'
  assert_contains "$workflow" 'BDD_RUNTIME_UNATTESTED'
  assert_contains "$workflow" "if: (github.event_name == 'workflow_dispatch' || github.event_name == 'push')"
  assert_not_contains "$workflow" 'builder_branch_cache'
  assert_not_contains "$workflow" 'branch-main'
}

for test in \
  test_keys_are_full_and_machine_readable \
  test_default_images_use_flat_ghcr_packages \
  test_complete_dependency_inputs_change_key \
  test_source_body_changes_only_source_key \
  test_target_topology_changes_dependency_key_without_hashing_body \
  test_dependency_context_uses_exact_dummy_topology \
  test_pg_matrix_uses_exact_with_packages_closures \
  test_nix_build_declares_musl_compiler \
  test_source_only_plan_gate \
  test_exact_dependency_hit_never_builds_or_mutates_branch_tags \
  test_public_consumer_fails_closed_without_dependency \
  test_publish_requires_explicit_trust \
  test_app_build_uses_digest_and_reports_zero_pg_work \
  test_runtime_assembly_is_one_layer_over_pg_digest \
  test_workflow_enforces_trust_and_short_circuit
do
  "$test"
done

echo "scripts/test-bdd-image.sh: ok"
