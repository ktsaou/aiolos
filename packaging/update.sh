#!/usr/bin/env bash
# update.sh — rebuild and replace the aiolos binaries in place.
#
# Only touches binaries; never overwrites config. Restarts the service only if it is currently
# active (so this is safe to run before cutover, when aiolos isn't running yet).

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; GRAY='\033[0;90m'; NC='\033[0m'

run() {
  printf >&2 "${GRAY}$(pwd) >${NC} "
  printf >&2 "${YELLOW}"; printf >&2 "%q " "$@"; printf >&2 "${NC}\n"
  if ! "$@"; then
    local rc=$?
    echo -e >&2 "${RED}[ERROR] command failed (exit ${rc}):${NC} $*"
    return $rc
  fi
}

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

BIN="/opt/aiolos/bin"

echo -e "${GREEN}=== aiolos update ===${NC}"
run cargo build --release

WAS_ACTIVE=0
if systemctl is-active --quiet aiolos; then
  WAS_ACTIVE=1
  echo "--- aiolos is active: stopping (modules restore devices on shutdown) ---"
  run sudo systemctl stop aiolos
fi

echo "--- replacing binaries ---"
for b in aiolos nvidia asrock16-2t nvme ipmi-temps nut gpu-powercap; do
  run sudo install -m 0755 "target/release/${b}" "${BIN}/${b}"
done

if [ "$WAS_ACTIVE" -eq 1 ]; then
  echo "--- restarting aiolos ---"
  run sudo systemctl start aiolos
else
  echo -e "${YELLOW}aiolos was not running; binaries updated, service left stopped.${NC}"
fi

echo -e "${GREEN}=== done ===${NC}"
