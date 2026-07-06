#!/usr/bin/env bash
set -euo pipefail

NIX_BASE_IMAGE=${BDD_NIX_BASE_IMAGE:-docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894}
DOCKER=${BDD_DOCKER:-docker}

BDD_DEPS_PATHS=(
  Dockerfile.bdd-builder
  flake.nix
  flake.lock
  rust-toolchain.toml
  Cargo.toml
  Cargo.lock
  'crates/*/Cargo.toml'
  'bins/*/Cargo.toml'
  xtask/Cargo.toml
)

BDD_BUILDER_CONTEXT_PATHS=(
  "${BDD_DEPS_PATHS[@]}"
)

BDD_RUNTIME_KEY_PATHS=(
  "${BDD_DEPS_PATHS[@]}"
  'crates/*/src/**'
  'bins/*/src/**'
  'crates/kronika-bdd/features/**'
)

BDD_RUNTIME_SOURCE_PATHS=(
  "${BDD_RUNTIME_KEY_PATHS[@]}"
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
  runtime-image  Print the default BDD runtime image tag.
  image-key      Print the key for the final BDD image.
  image-paths    Print files included in the BDD runtime image key.
  platform       Print the Docker platform used for the builder image.
  platform-slug  Print the platform as a Docker tag fragment.
  branch-slug [name]
                 Print a branch name as a Docker tag fragment.
  build-builder  Build or pull the BDD builder image.
  build-runtime  Build image.tar with the builder image and load it into Docker.
  run [image] [args...]
                 Run the BDD image. Extra args are passed to kronika-bdd.

Environment:
  BDD_IMAGE_PREFIX   Registry prefix, default ghcr.io/vadv/pgkronika.
  BDD_PLATFORM       Docker platform. Defaults to the local Docker server platform.
  BDD_BRANCH_NAME    Branch name used for the mutable branch cache.
  BDD_BUILDER_IMAGE  Builder image tag. Defaults to <prefix>/pgkronika-bdd-builder:deps-<platform>-<deps-key>.
  BDD_BUILDER_BRANCH_IMAGE
                     Mutable builder cache for BDD_BRANCH_NAME.
  BDD_BUILDER_MAIN_IMAGE
                     Mutable builder cache for main.
  BDD_NIX_BASE_IMAGE Pinned Nix image used when no branch cache is available.
  BDD_RUNTIME_IMAGE  Runtime image tag. Defaults to pgkronika-bdd:<platform>-sha-<image-key>.
  BDD_CACHE_FROM     Optional buildx cache source, for example type=registry,ref=...
  BDD_CACHE_TO       Optional buildx cache target, for example type=registry,ref=...,mode=max.
  BDD_BUILDER_PULL   Set to 1 to pull an existing builder image before building.
  BDD_BUILDER_PUSH   Set to 1 to push the builder image after building.
  BDD_BUILDER_USE_BRANCH_CACHE
                     Set to 0 to build a missing builder from BDD_NIX_BASE_IMAGE.
  BDD_BUILDER_UPDATE_BRANCH_CACHE
                     Set to 1 to retag a pulled or pushed builder as the branch cache.
  BDD_RUNTIME_PUSH   Set to 1 to push BDD_RUNTIME_IMAGE after building.
  BDD_RUNTIME_REUSE_LOCAL
                     Set to 0 to force rebuilding an existing local BDD_RUNTIME_IMAGE.
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
  local context=$1 dir

  for dir in "$context"/crates/*; do
    [ -f "$dir/Cargo.toml" ] || continue
    mkdir -p "$dir/src"
    case "${dir##*/}" in
      kronika-bdd)
        printf 'fn main() {}\n' > "$dir/src/main.rs"
        ;;
      *)
        printf '#![allow(missing_docs)]\n' > "$dir/src/lib.rs"
        ;;
    esac
  done

  for dir in "$context"/bins/* "$context"/xtask; do
    [ -f "$dir/Cargo.toml" ] || continue
    mkdir -p "$dir/src"
    printf 'fn main() {}\n' > "$dir/src/main.rs"
  done
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
  hash_git_paths "${BDD_DEPS_PATHS[@]}"
}

image_key() {
  hash_git_paths "${BDD_RUNTIME_KEY_PATHS[@]}"
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

branch_name() {
  if [ -n "${BDD_BRANCH_NAME:-}" ]; then
    printf '%s' "$BDD_BRANCH_NAME"
    return
  fi

  local branch
  branch=$(git branch --show-current 2>/dev/null || true)
  printf '%s' "${branch:-main}"
}

branch_slug() {
  local raw=${1:-$(branch_name)}
  local slug hash

  slug=$(printf '%s' "$raw" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//; s/-+/-/g')
  if [ -z "$slug" ]; then
    slug=branch
  fi

  if [ "${#slug}" -gt 80 ]; then
    hash=$(printf '%s' "$raw" | sha256_stream)
    slug="${slug:0:67}-${hash:0:12}"
    slug=$(printf '%s' "$slug" | sed -E 's/-+$//')
  fi

  printf '%s' "$slug"
}

image_prefix() {
  printf '%s' "${BDD_IMAGE_PREFIX:-ghcr.io/vadv/pgkronika}"
}

builder_image() {
  if [ -n "${BDD_BUILDER_IMAGE:-}" ]; then
    printf '%s' "$BDD_BUILDER_IMAGE"
    return
  fi
  printf '%s/pgkronika-bdd-builder:deps-%s-%s' "$(image_prefix)" "$(platform_slug)" "$(short_key "$(deps_key)")"
}

builder_branch_image() {
  if [ -n "${BDD_BUILDER_BRANCH_IMAGE:-}" ]; then
    printf '%s' "$BDD_BUILDER_BRANCH_IMAGE"
    return
  fi
  printf '%s/pgkronika-bdd-builder:deps-%s-branch-%s' "$(image_prefix)" "$(platform_slug)" "$(branch_slug)"
}

builder_main_image() {
  if [ -n "${BDD_BUILDER_MAIN_IMAGE:-}" ]; then
    printf '%s' "$BDD_BUILDER_MAIN_IMAGE"
    return
  fi
  printf '%s/pgkronika-bdd-builder:deps-%s-branch-main' "$(image_prefix)" "$(platform_slug)"
}

runtime_image() {
  if [ -n "${BDD_RUNTIME_IMAGE:-}" ]; then
    printf '%s' "$BDD_RUNTIME_IMAGE"
    return
  fi
  printf 'pgkronika-bdd:%s-sha-%s' "$(platform_slug)" "$(short_key "$(image_key)")"
}

image_repository() {
  local ref=${1%@*}
  local last=${ref##*/}
  if [[ "$last" == *:* ]]; then
    printf '%s' "${ref%:*}"
  else
    printf '%s' "$ref"
  fi
}

resolve_image_digest_ref() {
  local ref=$1 digest
  digest=$(docker_cmd buildx imagetools inspect "$ref" --format '{{.Manifest.Digest}}' 2>/dev/null || true)
  if [ -z "$digest" ] || [ "$digest" = "<no value>" ]; then
    return 1
  fi
  printf '%s@%s' "$(image_repository "$ref")" "$digest"
}

append_summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '%s\n' "$*" >> "$GITHUB_STEP_SUMMARY"
  fi
}

update_builder_branch_cache() {
  local image=$1 branch_cache=$2
  if [ "${BDD_BUILDER_UPDATE_BRANCH_CACHE:-0}" = "1" ]; then
    docker_cmd tag "$image" "$branch_cache"
    docker_cmd push "$branch_cache"
    append_summary "- updated branch cache: yes"
  else
    append_summary "- updated branch cache: no"
  fi
}

builder_base_image() {
  local branch_cache main_cache image digest previous=
  if [ "${BDD_BUILDER_USE_BRANCH_CACHE:-1}" != "1" ]; then
    printf '%s' "$NIX_BASE_IMAGE"
    return
  fi

  branch_cache=$(builder_branch_image)
  main_cache=$(builder_main_image)

  for image in "$branch_cache" "$main_cache"; do
    if [ "$image" = "$previous" ]; then
      continue
    fi
    previous=$image

    if docker_cmd manifest inspect "$image" >/dev/null 2>&1; then
      digest=$(resolve_image_digest_ref "$image" || true)
      if [ -n "$digest" ]; then
        printf '%s' "$digest"
        return
      fi
      echo "Builder cache image exists but its digest could not be resolved; using the next cache source" >&2
    fi
  done

  printf '%s' "$NIX_BASE_IMAGE"
}

build_builder() {
  local root context image branch_cache main_cache base
  root=$(repo_root)
  image=$(builder_image)
  branch_cache=$(builder_branch_image)
  main_cache=$(builder_main_image)

  append_summary "## BDD builder"
  append_summary ""
  append_summary "- exact: \`$image\`"
  append_summary "- branch cache: \`$branch_cache\`"
  append_summary "- main cache: \`$main_cache\`"

  if docker_cmd image inspect "$image" >/dev/null 2>&1; then
    append_summary "- local exact hit: yes"
    update_builder_branch_cache "$image" "$branch_cache"
    return
  fi

  if [ "${BDD_BUILDER_PULL:-0}" = "1" ] && docker_cmd manifest inspect "$image" >/dev/null 2>&1; then
    append_summary "- exact hit: yes"
    docker_cmd pull "$image"
    update_builder_branch_cache "$image" "$branch_cache"
    return
  fi

  append_summary "- exact hit: no"
  base=$(builder_base_image)
  append_summary "- base: \`$base\`"

  local args=(
    -f "$root/Dockerfile.bdd-builder"
    --target bdd-builder
    --platform "$(platform)"
    --build-arg "BDD_BUILDER_BASE=$base"
    --load
    -t "$image"
  )

  if [ -n "${BDD_CACHE_FROM:-}" ]; then
    args+=(--cache-from "$BDD_CACHE_FROM")
  fi
  if [ -n "${BDD_CACHE_TO:-}" ]; then
    args+=(--cache-to "$BDD_CACHE_TO")
  fi

  context=$(mktemp -d)
  if ! write_builder_context "$context"; then
    rm -rf "$context"
    return 1
  fi
  if ! docker_cmd buildx build "${args[@]}" "$context"; then
    rm -rf "$context"
    return 1
  fi
  rm -rf "$context"

  if [ "${BDD_BUILDER_PUSH:-0}" = "1" ]; then
    if docker_cmd manifest inspect "$image" >/dev/null 2>&1; then
      append_summary "- pushed exact: no, tag appeared before push"
      docker_cmd pull "$image"
      update_builder_branch_cache "$image" "$branch_cache"
      return
    fi
    docker_cmd push "$image"
    append_summary "- pushed exact: yes"
    update_builder_branch_cache "$image" "$branch_cache"
  fi
}

build_runtime() {
  local builder runtime output
  builder=$(builder_image)
  runtime=$(runtime_image)
  output=${BDD_OUTPUT_TAR:-image.tar}

  if [ "${BDD_RUNTIME_REUSE_LOCAL:-1}" = "1" ] && docker_cmd image inspect "$runtime" >/dev/null 2>&1; then
    append_summary "## BDD runtime"
    append_summary ""
    append_summary "- local exact hit: yes"
    append_summary "- image: \`$runtime\`"
    if [ "${BDD_RUNTIME_PUSH:-0}" = "1" ]; then
      docker_cmd push "$runtime"
    fi
    return
  fi

  source_tar "${BDD_RUNTIME_SOURCE_PATHS[@]}" | docker_cmd run --rm -i "$builder" sh -ceu '
    mkdir -p /tmp/src
    tar -C /tmp/src -xf -
    cd /tmp/src
    nix build .#image --out-link /tmp/img
    /tmp/img
  ' > "$output"

  docker_cmd load -i "$output"
  docker_cmd tag pgkronika-bdd:latest "$runtime"

  if [ "${BDD_RUNTIME_PUSH:-0}" = "1" ]; then
    docker_cmd push "$runtime"
  fi
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
  image-key)
    image_key
    ;;
  image-paths)
    print_git_paths "${BDD_RUNTIME_KEY_PATHS[@]}"
    ;;
  platform)
    platform
    printf '\n'
    ;;
  platform-slug)
    platform_slug
    printf '\n'
    ;;
  branch-slug)
    shift
    branch_slug "${1:-}"
    printf '\n'
    ;;
  build-builder)
    build_builder
    ;;
  build-runtime)
    build_runtime
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
