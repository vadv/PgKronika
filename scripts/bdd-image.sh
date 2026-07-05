#!/usr/bin/env bash
set -euo pipefail

NIX_BASE_IMAGE=${BDD_NIX_BASE_IMAGE:-docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894}
DOCKER=${BDD_DOCKER:-docker}

usage() {
  cat <<'EOF'
Usage: scripts/bdd-image.sh <command>

Commands:
  deps-key       Print the dependency key for the BDD builder image.
  image-key      Print the key for the final BDD image.
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
  BDD_RUNTIME_IMAGE  Runtime image tag. Defaults to pgkronika-bdd:latest.
  BDD_CACHE_FROM     Optional buildx cache source, for example type=registry,ref=...
  BDD_CACHE_TO       Optional buildx cache target, for example type=registry,ref=...,mode=max.
  BDD_BUILDER_PULL   Set to 1 to pull an existing builder image before building.
  BDD_BUILDER_PUSH   Set to 1 to push the builder image after building.
  BDD_BUILDER_USE_BRANCH_CACHE
                     Set to 0 to build a missing builder from BDD_NIX_BASE_IMAGE.
  BDD_BUILDER_UPDATE_BRANCH_CACHE
                     Set to 1 to retag a pulled or pushed builder as the branch cache.
  BDD_RUNTIME_PUSH   Set to 1 to push BDD_RUNTIME_IMAGE after building.
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

deps_key() {
  hash_git_paths \
    Dockerfile.bdd-builder \
    flake.nix \
    flake.lock \
    rust-toolchain.toml \
    Cargo.toml \
    Cargo.lock \
    crates/*/Cargo.toml \
    bins/*/Cargo.toml \
    xtask/Cargo.toml
}

image_key() {
  hash_git_paths \
    Dockerfile.bdd-builder \
    scripts/bdd-image.sh \
    flake.nix \
    flake.lock \
    rust-toolchain.toml \
    Cargo.toml \
    Cargo.lock \
    crates/*/Cargo.toml \
    bins/*/Cargo.toml \
    xtask/Cargo.toml \
    crates/*/src/** \
    bins/*/src/** \
    crates/kronika-bdd/features/**
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
  printf '%s' "${BDD_RUNTIME_IMAGE:-pgkronika-bdd:latest}"
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
  local root image branch_cache main_cache base
  root=$(repo_root)
  image=$(builder_image)
  branch_cache=$(builder_branch_image)
  main_cache=$(builder_main_image)

  append_summary "## BDD builder"
  append_summary ""
  append_summary "- exact: \`$image\`"
  append_summary "- branch cache: \`$branch_cache\`"
  append_summary "- main cache: \`$main_cache\`"

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

  docker_cmd buildx build "${args[@]}" "$root"

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
  local root builder runtime output
  root=$(repo_root)
  builder=$(builder_image)
  runtime=$(runtime_image)
  output=${BDD_OUTPUT_TAR:-image.tar}

  docker_cmd run --rm -v "$root":/src:ro "$builder" sh -ceu '
    mkdir -p /tmp/src
    tar \
      --exclude=.git \
      --exclude=.direnv \
      --exclude=target \
      --exclude=result \
      --exclude=result-* \
      --exclude=image.tar \
      -C /src -cf - . | tar -C /tmp/src -xf -
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
  image-key)
    image_key
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
