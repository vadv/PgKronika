#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DOCKER=${BDD_DOCKER:-docker}
BDD_IMAGE=$ROOT/scripts/bdd-image.sh
IMAGE_TAG=${DEMO_IMAGE:-pgkronika-demo:local}
CONTAINER=${DEMO_CONTAINER:-pgkronika-demo-stand}
DATA_DIR=${DEMO_DATA_DIR:-$ROOT/demo-data}
PG_PORT=${DEMO_PG_PORT:-15432}
WEB_PORT=${DEMO_WEB_PORT:-18081}
STOP_TIMEOUT=${DEMO_STOP_TIMEOUT:-300}
READY_TIMEOUT=${DEMO_READY_TIMEOUT:-300}

usage() {
  cat <<'EOF'
Usage: scripts/demo-stand.sh <command>

Commands:
  build    Build the demo-stand image with the BDD builder toolchain.
  up       Start the stand as a service: PG 17 under load + collector + web.
  down     Stop the stand; shutdown seals segments and writes report.json.
  run      One-shot bounded run (DEMO_DURATION_MIN, default 30) with a report.
  measure  Re-run the size measurement over existing segments.
  report   Print the last report.json.
  status   Show the stand container and endpoints.
  logs     Follow the stand container logs.
  clean    Remove everything in the data directory (files belong to the
           container user, so this runs through the image).

The PostgreSQL cluster inside the stand is throwaway: every start wipes
pgdata and the tablespaces. Sealed segments, fact files, and report.json in
the data directory survive restarts.

Environment:
  DEMO_IMAGE         Local image tag, default pgkronika-demo:local.
  DEMO_CONTAINER     Container name, default pgkronika-demo-stand.
  DEMO_DATA_DIR      Host directory mounted at /data, default ./demo-data.
  DEMO_PG_PORT       Host port published to PostgreSQL, default 15432.
  DEMO_WEB_PORT      Host port published to the web viewer, default 18081.
  DEMO_STOP_TIMEOUT  Seconds `down` waits for seal+measure, default 300.
  DEMO_READY_TIMEOUT Seconds `up` waits for the ready marker, default 300.
  DEMO_BACKENDS, DEMO_TPS, DEMO_TABLES, DEMO_INDEXES, DEMO_LARGE_SCAN_ROWS,
  DEMO_CHART_SERIES, DEMO_DURATION_MIN (run only),
  KRONIKA_SEGMENT_MAX_AGE_S, KRONIKA_INTERVAL_S
                     Passed through to the stand when set.
EOF
}

builder_image() {
  if [ -n "${BDD_BUILDER_IMAGE:-}" ]; then
    printf '%s' "$BDD_BUILDER_IMAGE"
    return
  fi
  local prefix slug key
  prefix=${BDD_IMAGE_PREFIX:-ghcr.io/vadv}
  slug=$("$BDD_IMAGE" platform-slug)
  key=$("$BDD_IMAGE" deps-key)
  printf '%s/pgkronika-bdd-builder:builder-%s-%.16s' "$prefix" "$slug" "$key"
}

OUTPUT_TAR=${DEMO_OUTPUT_TAR:-$ROOT/demo-image.tar}

build_image() {
  export BDD_BUILDER_PULL=${BDD_BUILDER_PULL:-1}
  "$BDD_IMAGE" build-builder
  local builder
  builder=$(builder_image)
  trap 'rm -f "$OUTPUT_TAR"' EXIT
  (
    cd "$ROOT"
    "$BDD_IMAGE" runtime-paths | tar -T - -cf -
  ) | "$DOCKER" run --rm -i --network none "$builder" sh -ceu '
    mkdir -p /tmp/src
    tar -C /tmp/src -xf -
    cd /tmp/src
    nix build --offline .#demoImage --out-link /tmp/img
    /tmp/img
  ' > "$OUTPUT_TAR"
  "$DOCKER" load -i "$OUTPUT_TAR"
  "$DOCKER" tag pgkronika-demo:latest "$IMAGE_TAG"
}

# Stand tunables forwarded into the container. `up` skips DEMO_DURATION_MIN:
# a service must not stop itself after a bounded load phase.
pass_env_args() {
  local skip=${1:-} var
  for var in DEMO_DURATION_MIN DEMO_BACKENDS DEMO_TPS DEMO_TABLES DEMO_INDEXES \
    DEMO_LARGE_SCAN_ROWS DEMO_CHART_SERIES KRONIKA_SEGMENT_MAX_AGE_S KRONIKA_INTERVAL_S; do
    if [ "$var" = "$skip" ]; then
      continue
    fi
    if [ -n "${!var:-}" ]; then
      printf -- '-e\n%s=%s\n' "$var" "${!var}"
    fi
  done
}

collect_run_args() {
  local skip=${1:-}
  RUN_ARGS=(-v "$DATA_DIR:/data:z")
  local line
  while IFS= read -r line; do
    [ -n "$line" ] && RUN_ARGS+=("$line")
  done < <(pass_env_args "$skip")
}

prepare_data_dir() {
  mkdir -p "$DATA_DIR"
  chmod 0777 "$DATA_DIR"
}

wait_ready() {
  local waited=0
  while [ "$waited" -lt "$READY_TIMEOUT" ]; do
    if [ "$("$DOCKER" inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null)" != "true" ]; then
      echo "ERROR: stand container exited during startup; last log lines:" >&2
      "$DOCKER" logs --tail 40 "$CONTAINER" >&2 2>&1 || true
      "$DOCKER" rm "$CONTAINER" >/dev/null 2>&1 || true
      exit 1
    fi
    if "$DOCKER" logs "$CONTAINER" 2>&1 | grep -q '^stand: ready'; then
      return 0
    fi
    sleep 2
    waited=$((waited + 2))
  done
  echo "ERROR: stand did not become ready in ${READY_TIMEOUT}s; see: $0 logs" >&2
  exit 1
}

up() {
  if "$DOCKER" container inspect "$CONTAINER" >/dev/null 2>&1; then
    echo "ERROR: container $CONTAINER already exists; run 'down' first" >&2
    exit 2
  fi
  prepare_data_dir
  collect_run_args DEMO_DURATION_MIN
  "$DOCKER" run -d --name "$CONTAINER" \
    --stop-timeout "$STOP_TIMEOUT" \
    -p "127.0.0.1:$PG_PORT:5432" \
    -p "127.0.0.1:$WEB_PORT:8080" \
    "${RUN_ARGS[@]}" \
    -e DEMO_DURATION_MIN=0 \
    "$IMAGE_TAG" stand >/dev/null
  echo "waiting for the stand to boot and seed (up to ${READY_TIMEOUT}s)..."
  wait_ready
  echo "stand is up:"
  echo "  postgres  host=127.0.0.1 port=$PG_PORT user=postgres (trust)"
  echo "  web       http://127.0.0.1:$WEB_PORT"
  echo "  data      $DATA_DIR"
  echo "stop with: make demo-down (seals segments, writes report.json)"
}

down() {
  if ! "$DOCKER" container inspect "$CONTAINER" >/dev/null 2>&1; then
    echo "stand is not running"
    exit 0
  fi
  echo "stopping $CONTAINER (up to ${STOP_TIMEOUT}s for seal + measure)..."
  "$DOCKER" stop -t "$STOP_TIMEOUT" "$CONTAINER" >/dev/null
  "$DOCKER" logs "$CONTAINER" > "$DATA_DIR/stand.log" 2>&1 || true
  tail -n 25 "$DATA_DIR/stand.log" || true
  "$DOCKER" rm "$CONTAINER" >/dev/null
  echo "full stand log: $DATA_DIR/stand.log"
  if [ -f "$DATA_DIR/report.json" ]; then
    echo "report: $DATA_DIR/report.json"
  fi
}

one_shot() {
  local subcommand=$1
  prepare_data_dir
  collect_run_args
  "$DOCKER" run --rm "${RUN_ARGS[@]}" "$IMAGE_TAG" "$subcommand"
}

case "${1:-}" in
  build)
    build_image
    ;;
  up)
    up
    ;;
  down)
    down
    ;;
  run)
    one_shot stand
    ;;
  measure)
    one_shot measure
    ;;
  report)
    if [ ! -f "$DATA_DIR/report.json" ]; then
      echo "no report yet: $DATA_DIR/report.json is absent (run 'down' or 'run' first)" >&2
      exit 1
    fi
    cat "$DATA_DIR/report.json"
    ;;
  status)
    "$DOCKER" ps --filter "name=$CONTAINER" --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}'
    ;;
  clean)
    if "$DOCKER" container inspect "$CONTAINER" >/dev/null 2>&1; then
      echo "ERROR: stop the stand first ('down'), then clean" >&2
      exit 2
    fi
    if [ -d "$DATA_DIR" ]; then
      "$DOCKER" run --rm -v "$DATA_DIR:/data:z" "$IMAGE_TAG" clean
      rmdir "$DATA_DIR" 2>/dev/null || true
      echo "cleaned $DATA_DIR"
    fi
    ;;
  logs)
    "$DOCKER" logs -f "$CONTAINER"
    ;;
  -h|--help|help|'')
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
