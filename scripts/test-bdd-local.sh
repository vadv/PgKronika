#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DOCKER=${BDD_DOCKER:-docker}
TAGS=${TAGS:-}

fail() {
  echo "ERROR: $*" >&2
  exit 2
}

if [ -z "$TAGS" ]; then
  fail "TAGS is required. Example: DEBUG=1 make test-bdd TAGS=@pg_log"
fi

case "$TAGS" in
  *$'\n'*|*$'\r'*)
    fail "TAGS must be a single-line Cucumber tag expression."
    ;;
esac

if ! printf '%s\n' "$TAGS" | grep -Eq '(^|[[:space:]()])@[A-Za-z0-9_-]+'; then
  fail "TAGS must contain at least one Cucumber tag, for example @pg_log."
fi

if ! command -v "$DOCKER" >/dev/null 2>&1; then
  fail "Docker command '$DOCKER' was not found. Set BDD_DOCKER or install Docker."
fi

if ! "$DOCKER" info >/dev/null 2>&1; then
  fail "Docker daemon is not reachable. Start Docker and retry."
fi

if ! "$DOCKER" buildx version >/dev/null 2>&1; then
  fail "Docker Buildx is required for the BDD image path."
fi

export BDD_BUILDER_PULL=${BDD_BUILDER_PULL:-1}

runtime_image=${BDD_RUNTIME_IMAGE:-}
if [ -z "$runtime_image" ]; then
  runtime_image=$("$ROOT/scripts/bdd-image.sh" runtime-image)
fi
export BDD_RUNTIME_IMAGE=$runtime_image
export BDD_RUNTIME_REUSE_LOCAL=${BDD_RUNTIME_REUSE_LOCAL:-1}

cleanup_output=
if [ -z "${BDD_OUTPUT_TAR:-}" ]; then
  BDD_OUTPUT_TAR=$(mktemp "${TMPDIR:-/tmp}/pgkronika-bdd.XXXXXX.tar")
  cleanup_output=1
fi
export BDD_OUTPUT_TAR

cleanup() {
  if [ -n "$cleanup_output" ]; then
    rm -f "$BDD_OUTPUT_TAR"
  fi
}
trap cleanup EXIT

if "$DOCKER" image inspect "$runtime_image" >/dev/null 2>&1; then
  echo "Reusing BDD runtime image $runtime_image"
else
  "$ROOT/scripts/bdd-image.sh" build-builder
  "$ROOT/scripts/bdd-image.sh" build-runtime
fi

cucumber_args=(--tags "$TAGS")
if [ -n "${DEBUG:-}" ] && [ "$DEBUG" != "0" ]; then
  cucumber_args=(-vvv "${cucumber_args[@]}")
fi

"$ROOT/scripts/bdd-image.sh" run "$runtime_image" "${cucumber_args[@]}"
