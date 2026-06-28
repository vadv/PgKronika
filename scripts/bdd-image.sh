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
  build-builder  Build or pull the BDD builder image.
  build-runtime  Build image.tar with the builder image and load it into Docker.
  run [image]    Run the BDD image.

Environment:
  BDD_IMAGE_PREFIX   Registry prefix, default ghcr.io/vadv/pgkronika.
  BDD_PLATFORM       Docker platform. Defaults to the local Docker server platform.
  BDD_BUILDER_IMAGE  Builder image tag. Defaults to <prefix>/pgkronika-bdd-builder:deps-<platform>-<deps-key>.
  BDD_BUILDER_SEED_IMAGE
                     Seed image tag. Defaults to <prefix>/pgkronika-bdd-builder:deps-<platform>-seed-v1.
  BDD_NIX_BASE_IMAGE Pinned Nix image used when no seed is available.
  BDD_RUNTIME_IMAGE  Runtime image tag. Defaults to pgkronika-bdd:latest.
  BDD_CACHE_FROM     Optional buildx cache source, for example type=registry,ref=...
  BDD_CACHE_TO       Optional buildx cache target, for example type=registry,ref=...,mode=max.
  BDD_BUILDER_PULL   Set to 1 to pull an existing builder image before building.
  BDD_BUILDER_PUSH   Set to 1 to push the builder image after building.
  BDD_BUILDER_USE_SEED
                     Set to 0 to build a missing builder from BDD_NIX_BASE_IMAGE.
  BDD_BUILDER_UPDATE_SEED
                     Set to 1 to retag a pushed builder as the seed image.
  BDD_RUNTIME_PUSH   Set to 1 to push BDD_RUNTIME_IMAGE after building.
  BDD_OUTPUT_TAR     Tarball path for build-runtime, default image.tar.
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

builder_seed_image() {
  if [ -n "${BDD_BUILDER_SEED_IMAGE:-}" ]; then
    printf '%s' "$BDD_BUILDER_SEED_IMAGE"
    return
  fi
  printf '%s/pgkronika-bdd-builder:deps-%s-seed-v1' "$(image_prefix)" "$(platform_slug)"
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

update_builder_seed() {
  local image=$1 seed=$2
  if [ "${BDD_BUILDER_UPDATE_SEED:-0}" = "1" ]; then
    docker_cmd tag "$image" "$seed"
    docker_cmd push "$seed"
    append_summary "- updated seed: yes"
  else
    append_summary "- updated seed: no"
  fi
}

builder_base_image() {
  local seed seed_digest
  if [ "${BDD_BUILDER_USE_SEED:-1}" != "1" ]; then
    printf '%s' "$NIX_BASE_IMAGE"
    return
  fi

  seed=$(builder_seed_image)
  if docker_cmd manifest inspect "$seed" >/dev/null 2>&1; then
    seed_digest=$(resolve_image_digest_ref "$seed" || true)
    if [ -n "$seed_digest" ]; then
      printf '%s' "$seed_digest"
      return
    fi
    echo "Seed image exists but its digest could not be resolved; using $NIX_BASE_IMAGE" >&2
  fi

  printf '%s' "$NIX_BASE_IMAGE"
}

build_builder() {
  local root image seed base
  root=$(repo_root)
  image=$(builder_image)
  seed=$(builder_seed_image)

  append_summary "## BDD builder"
  append_summary ""
  append_summary "- exact: \`$image\`"
  append_summary "- seed: \`$seed\`"

  if [ "${BDD_BUILDER_PULL:-0}" = "1" ] && docker_cmd manifest inspect "$image" >/dev/null 2>&1; then
    append_summary "- exact hit: yes"
    docker_cmd pull "$image"
    update_builder_seed "$image" "$seed"
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
      update_builder_seed "$image" "$seed"
      return
    fi
    docker_cmd push "$image"
    append_summary "- pushed exact: yes"
    update_builder_seed "$image" "$seed"
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
  local image=${1:-$(runtime_image)}
  docker_cmd run --rm "$image"
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
