#!/usr/bin/env bash
# install.sh — build aiolos and install it to /opt/aiolos/
#
# Installs binaries (always) + default config (only if absent — never clobbers operator edits) +
# the systemd unit. Does NOT start the service or stop any existing cooling daemon; cutover is a
# deliberate, separate, operator-gated step.

set -euo pipefail

# Colors
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

DEST="/opt/aiolos"
BIN="${DEST}/bin"
ETC="${DEST}/etc"

echo -e "${GREEN}=== aiolos install ===${NC}  (dest: ${DEST})"

echo "--- building release binaries ---"
run cargo build --release

echo "--- creating directories ---"
run sudo mkdir -p "${BIN}" "${ETC}"

echo "--- installing binaries (overwrite) ---"
for b in aiolos nvidia asrock16-2t nvme ipmi-temps; do
  run sudo install -m 0755 "target/release/${b}" "${BIN}/${b}"
done

echo "--- installing default config (only if absent) ---"
install_if_absent() {
  local src="$1" dst="$2"
  if [ -e "$dst" ]; then
    echo -e "${YELLOW}  keep ${dst} (already present)${NC}"
  else
    run sudo install -m 0644 "$src" "$dst"
  fi
}
install_if_absent packaging/aiolos.conf            "${ETC}/aiolos.conf"
install_if_absent packaging/nvidia.curve.json      "${ETC}/nvidia.curve.json"
install_if_absent packaging/asrock16-2t.curve.json "${ETC}/asrock16-2t.curve.json"

echo "--- installing systemd unit ---"
run sudo install -m 0644 systemd/aiolos.service /etc/systemd/system/aiolos.service
run sudo systemctl daemon-reload

echo -e "${GREEN}=== done ===${NC}"
cat <<EOF
Installed. aiolos is NOT started and the existing cooling daemon (e.g. nvfd) is untouched.

Cutover (operator-gated):
  1. Review ${ETC}/aiolos.conf and the *.curve.json files.
  2. Stop the current controller (e.g. 'sudo systemctl stop nvfd') ONLY when ready.
  3. sudo systemctl enable --now aiolos
  4. Watch:   journalctl -u aiolos -f      and   http://<host>:9876/
EOF
