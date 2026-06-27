#!/usr/bin/env bash
set -euo pipefail

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
  BDD_RUNTIME_IMAGE  Runtime image tag. Defaults to pgkronika-bdd:latest.
  BDD_CACHE_FROM     Optional buildx cache source, for example type=registry,ref=...
  BDD_CACHE_TO       Optional buildx cache target, for example type=registry,ref=...,mode=max.
  BDD_BUILDER_PULL   Set to 1 to pull an existing builder image before building.
  BDD_BUILDER_PUSH   Set to 1 to push the builder image after building.
  BDD_RUNTIME_PUSH   Set to 1 to push BDD_RUNTIME_IMAGE after building.
  BDD_OUTPUT_TAR     Tarball path for build-runtime, default image.tar.
EOF
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
  os=$(docker info --format '{{.OSType}}')
  arch=$(docker info --format '{{.Architecture}}')
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

runtime_image() {
  printf '%s' "${BDD_RUNTIME_IMAGE:-pgkronika-bdd:latest}"
}

build_builder() {
  local root image
  root=$(repo_root)
  image=$(builder_image)

  if [ "${BDD_BUILDER_PULL:-0}" = "1" ] && docker manifest inspect "$image" >/dev/null 2>&1; then
    docker pull "$image"
    return
  fi

  local args=(
    -f "$root/Dockerfile.bdd-builder"
    --target bdd-builder
    --platform "$(platform)"
    --load
    -t "$image"
  )

  if [ -n "${BDD_CACHE_FROM:-}" ]; then
    args+=(--cache-from "$BDD_CACHE_FROM")
  fi
  if [ -n "${BDD_CACHE_TO:-}" ]; then
    args+=(--cache-to "$BDD_CACHE_TO")
  fi

  docker buildx build "${args[@]}" "$root"

  if [ "${BDD_BUILDER_PUSH:-0}" = "1" ]; then
    docker push "$image"
  fi
}

build_runtime() {
  local root builder runtime output
  root=$(repo_root)
  builder=$(builder_image)
  runtime=$(runtime_image)
  output=${BDD_OUTPUT_TAR:-image.tar}

  docker run --rm -v "$root":/src:ro "$builder" sh -ceu '
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

  docker load -i "$output"
  docker tag pgkronika-bdd:latest "$runtime"

  if [ "${BDD_RUNTIME_PUSH:-0}" = "1" ]; then
    docker push "$runtime"
  fi
}

run_runtime() {
  local image=${1:-$(runtime_image)}
  docker run --rm "$image"
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
