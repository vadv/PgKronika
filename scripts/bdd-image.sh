#!/usr/bin/env bash
set -euo pipefail

NIX_BASE_IMAGE=${BDD_NIX_BASE_IMAGE:-docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894}
DOCKER=${BDD_DOCKER:-docker}
readonly BDD_BUILDER_KEY_SCHEMA=builder-context-v3
readonly BDD_BUILDER_COMPRESSION=zstd
readonly BDD_BUILDER_COMPRESSION_LEVEL=6
readonly BDD_BUILDER_OCI_MEDIA_TYPES=true

BDD_DEPS_PATHS=(
  Dockerfile.bdd-builder
  flake.nix
  flake.lock
  rust-toolchain.toml
  Cargo.toml
  Cargo.lock
  '.cargo/**'
  'crates/*/Cargo.toml'
  'bins/*/Cargo.toml'
  xtask/Cargo.toml
)

BDD_BUILDER_CONTEXT_PATHS=(
  "${BDD_DEPS_PATHS[@]}"
)

BDD_RUNTIME_SOURCE_PATHS=(
  "${BDD_DEPS_PATHS[@]}"
  'crates/*/build.rs'
  'crates/*/src/**'
  'crates/*/benches/**'
  'bins/*/build.rs'
  'bins/*/src/**'
  'bins/*/benches/**'
  'bins/*/static/**'
  'crates/kronika-bdd/features/**'
  xtask/build.rs
  'xtask/src/**'
)

usage() {
  cat <<'EOF'
Usage: scripts/bdd-image.sh <command>

Commands:
  deps-key       Print the dependency key for the BDD builder image.
  deps-paths     Print files included in the BDD builder dependency key.
  builder-paths  Print repository files used to seed the BDD builder context.
  builder-context-tar
                 Print the filtered BDD builder Docker context tar.
  runtime-image  Print the ephemeral local BDD runtime tag.
  runtime-paths  Print repository files copied into the local runtime build.
  platform       Print the Docker platform used for the builder image.
  platform-slug  Print the platform as a Docker tag fragment.
  build-builder  Build or pull the BDD builder image.
  build-runtime  Build image.tar with the builder image and load it into Docker.
  check-runtime [image]
                 Verify PostgreSQL 15-18 and pg_store_plans files.
  run [image] [args...]
                 Run the BDD image. Extra args are passed to kronika-bdd.

Environment:
  BDD_IMAGE_PREFIX   Registry prefix, default ghcr.io/vadv.
  BDD_PLATFORM       Docker platform. Defaults to the local Docker server platform.
  BDD_BUILDER_IMAGE  Builder image tag. Defaults to <prefix>/pgkronika-bdd-builder:builder-<platform>-<deps-key>.
  BDD_NIX_BASE_IMAGE Pinned Nix image used to build a missing exact builder.
  BDD_RUNTIME_IMAGE  Ephemeral local runtime tag. Defaults to a GitHub run-scoped tag or pgkronika-bdd:local.
  BDD_BUILDER_PULL   Set to 1 to pull an existing builder image before building.
  BDD_BUILDER_PUSH   Set to 1 to push the builder image after building.
  BDD_OUTPUT_TAR     Tarball path for build-runtime, default image.tar.
  DEBUG              Passed through to the BDD container when set.
EOF
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

hash_git_paths() {
  local root
  root=$(repo_root)
  (
    cd "$root"
    git ls-files -co --exclude-standard -z -- "$@" \
      | LC_ALL=C sort -z \
      | while IFS= read -r -d '' path; do
          [ -f "$path" ] || continue
          printf '%s\0' "$path"
          cat "$path"
          printf '\0'
        done
  ) | sha256_stream
}

print_git_paths() {
  local root
  root=$(repo_root)
  (
    cd "$root"
    git ls-files -co --exclude-standard -- "$@" | LC_ALL=C sort
  )
}

cargo_target_paths() {
  local root manifest dir path
  root=$(repo_root)
  (
    cd "$root"
    for manifest in crates/*/Cargo.toml bins/*/Cargo.toml xtask/Cargo.toml; do
      [ -f "$manifest" ] || continue
      dir=${manifest%/Cargo.toml}
      for path in "$dir"/build.rs "$dir"/src/{lib,main}.rs "$dir"/src/bin/*.rs "$dir"/src/bin/*/main.rs "$dir"/{tests,examples,benches}/*.rs "$dir"/{tests,examples,benches}/*/main.rs; do
        [ -f "$path" ] && printf '%s\n' "$path"
      done
      :
    done | LC_ALL=C sort
  )
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

write_dummy_builder_sources() {
  local context=$1 path target
  cargo_target_paths | while IFS= read -r path; do
    target="$context/$path"
    mkdir -p "${target%/*}"
    case "$path" in
      */src/lib.rs)
        printf '#![allow(missing_docs)]\n' > "$target"
        ;;
      */tests/*.rs|*/tests/*/main.rs)
        : > "$target"
        ;;
      *) printf 'fn main() {}\n' > "$target" ;;
    esac
  done
  mkdir -p "$context/crates/kronika-bdd/features"
}

write_builder_context() {
  local context=$1
  source_tar "${BDD_BUILDER_CONTEXT_PATHS[@]}" | tar -C "$context" -xf -
  write_dummy_builder_sources "$context"
}

builder_context_tar() {
  local context
  context=$(mktemp -d)
  if ! write_builder_context "$context"; then
    rm -rf "$context"
    return 1
  fi
  tar -C "$context" -cf - .
  rm -rf "$context"
}

deps_key() {
  {
    printf '%s\0%s\0%s\0%s\0%s\0%s\0' \
      "$BDD_BUILDER_KEY_SCHEMA" \
      "$NIX_BASE_IMAGE" \
      "$BDD_BUILDER_COMPRESSION" \
      "$BDD_BUILDER_COMPRESSION_LEVEL" \
      "$BDD_BUILDER_OCI_MEDIA_TYPES" \
      "$(hash_git_paths "${BDD_DEPS_PATHS[@]}")"
    cargo_target_paths
  } | sha256_stream
}

short_key() {
  local key=$1
  printf '%.16s' "$key"
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
    x86_64)
      arch=amd64
      ;;
    aarch64)
      arch=arm64
      ;;
  esac
  printf '%s/%s' "$os" "$arch"
}

platform_slug() {
  platform | tr '/_' '-'
}

image_prefix() {
  printf '%s' "${BDD_IMAGE_PREFIX:-ghcr.io/vadv}"
}

builder_image() {
  if [ -n "${BDD_BUILDER_IMAGE:-}" ]; then
    printf '%s' "$BDD_BUILDER_IMAGE"
    return
  fi
  printf '%s/pgkronika-bdd-builder:builder-%s-%s' "$(image_prefix)" "$(platform_slug)" "$(short_key "$(deps_key)")"
}

runtime_image() {
  if [ -n "${BDD_RUNTIME_IMAGE:-}" ]; then
    printf '%s' "$BDD_RUNTIME_IMAGE"
    return
  fi
  if [ -n "${GITHUB_RUN_ID:-}" ]; then
    printf 'pgkronika-bdd:run-%s-%s' "$GITHUB_RUN_ID" "${GITHUB_RUN_ATTEMPT:-1}"
  else
    printf 'pgkronika-bdd:local'
  fi
}

append_summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '%s\n' "$*" >> "$GITHUB_STEP_SUMMARY"
  fi
}

build_builder() {
  local root context image registry_output
  root=$(repo_root)
  image=$(builder_image)

  append_summary "## BDD builder"
  append_summary ""
  append_summary "- dependency key: \`$(deps_key)\`"
  append_summary "- exact: \`$image\`"

  if docker_cmd image inspect "$image" >/dev/null 2>&1; then
    append_summary "- local exact hit: yes"
    return
  fi

  if [ "${BDD_BUILDER_PULL:-0}" = "1" ] && docker_cmd manifest inspect "$image" >/dev/null 2>&1; then
    append_summary "- exact hit: yes"
    docker_cmd pull "$image"
    return
  fi

  append_summary "- exact hit: no"
  append_summary "- base: \`$NIX_BASE_IMAGE\`"

  local args=(
    -f "$root/Dockerfile.bdd-builder"
    --target bdd-builder
    --platform "$(platform)"
    --build-arg "BDD_BUILDER_BASE=$NIX_BASE_IMAGE"
    --provenance=false
  )

  context=$(mktemp -d)
  if ! write_builder_context "$context"; then
    rm -rf "$context"
    return 1
  fi
  if [ "${BDD_BUILDER_PUSH:-0}" = "1" ]; then
    if ! docker_cmd buildx build "${args[@]}" --output type=cacheonly "$context"; then
      rm -rf "$context"
      return 1
    fi
    if docker_cmd manifest inspect "$image" >/dev/null 2>&1; then
      append_summary "- pushed exact: no, tag appeared before push"
      docker_cmd pull "$image"
      rm -rf "$context"
      return
    fi
    registry_output="type=registry,name=$image,oci-mediatypes=$BDD_BUILDER_OCI_MEDIA_TYPES,compression=$BDD_BUILDER_COMPRESSION,compression-level=$BDD_BUILDER_COMPRESSION_LEVEL,force-compression=true"
    if ! docker_cmd buildx build "${args[@]}" --output "$registry_output" "$context"; then
      rm -rf "$context"
      return 1
    fi
    docker_cmd pull "$image"
    append_summary "- pushed exact: yes"
    append_summary "- registry compression: \`$BDD_BUILDER_COMPRESSION\` level \`$BDD_BUILDER_COMPRESSION_LEVEL\`, OCI media types"
  elif ! docker_cmd buildx build "${args[@]}" --load -t "$image" "$context"; then
    rm -rf "$context"
    return 1
  fi
  rm -rf "$context"
}

build_runtime() {
  local builder runtime output
  builder=$(builder_image)
  runtime=$(runtime_image)
  output=${BDD_OUTPUT_TAR:-image.tar}

  append_summary "## BDD source build"
  append_summary ""
  append_summary "- local runtime: \`$runtime\`"
  append_summary "- published: no"

  source_tar "${BDD_RUNTIME_SOURCE_PATHS[@]}" | docker_cmd run --rm -i --network none "$builder" sh -ceu '
    mkdir -p /tmp/src
    tar -C /tmp/src -xf -
    cd /tmp/src
    nix build --offline .#image --out-link /tmp/img
    /tmp/img
  ' > "$output"

  docker_cmd load -i "$output"
  docker_cmd tag pgkronika-bdd:latest "$runtime"
}

check_runtime() {
  local runtime=${1:-$(runtime_image)}
  docker_cmd run --rm --entrypoint /bin/sh "$runtime" -ceu '
    old_ifs=$IFS; IFS=";"; set -- $KRONIKA_PG_MATRIX; IFS=$old_ifs; seen=
    for entry do
      major=${entry%%=*}; bin=${entry#*=}; root=${bin%/bin}
      case "$major" in 15|16|17|18) ;; *) echo "unexpected PG$major" >&2; exit 1;; esac
      [ -x "$bin/postgres" ] && [ -f "$root/lib/pg_store_plans.so" ] &&
        [ -f "$root/share/postgresql/extension/pg_store_plans.control" ] ||
        { echo "incomplete PG$major runtime" >&2; exit 1; }
      sql=false
      for path in "$root"/share/postgresql/extension/pg_store_plans--*.sql; do
        [ -f "$path" ] && sql=true && break
      done
      $sql || { echo "missing PG$major extension SQL" >&2; exit 1; }
      seen="$seen $major"
    done
    for major in 15 16 17 18; do
      case " $seen " in *" $major "*) ;; *) echo "missing PG$major" >&2; exit 1;; esac
    done
  '
}

run_runtime() {
  local image
  if [ "$#" -gt 0 ] && [[ "$1" != -* ]]; then
    image=$1
    shift
  else
    image=$(runtime_image)
  fi

  local args=(--rm)
  if [ -n "${DEBUG:-}" ]; then
    args+=(-e "DEBUG=$DEBUG")
  fi

  docker_cmd run "${args[@]}" "$image" "$@"
}

cmd=${1:-}
case "$cmd" in
  deps-key)
    deps_key
    ;;
  deps-paths)
    print_git_paths "${BDD_DEPS_PATHS[@]}"
    ;;
  builder-paths)
    print_git_paths "${BDD_BUILDER_CONTEXT_PATHS[@]}"
    ;;
  builder-context-tar)
    builder_context_tar
    ;;
  runtime-image)
    runtime_image
    printf '\n'
    ;;
  runtime-paths)
    print_git_paths "${BDD_RUNTIME_SOURCE_PATHS[@]}"
    ;;
  platform)
    platform
    printf '\n'
    ;;
  platform-slug)
    platform_slug
    printf '\n'
    ;;
  build-builder)
    build_builder
    ;;
  build-runtime)
    build_runtime
    ;;
  check-runtime)
    shift
    check_runtime "${1:-}"
    ;;
  run)
    shift
    run_runtime "$@"
    ;;
  -h|--help|help|'')
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
