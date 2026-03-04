#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HUB_LOG="${ROOT_DIR}/tmp/hub.log"
AGENT_LOG="${ROOT_DIR}/tmp/agent.log"
MOCK_FILE="${ARK_RDMA_MOCK_FILE:-examples/mock/rdma/events-pfc-storm.jsonl}"
mkdir -p "${ROOT_DIR}/tmp"

if [[ -x "/tmp/ark-cargo/bin/cargo" ]]; then
  export CARGO_HOME="/tmp/ark-cargo"
  export RUSTUP_HOME="/tmp/ark-rustup"
  CARGO_BIN="/tmp/ark-cargo/bin/cargo"
else
  CARGO_BIN="cargo"
fi

echo "[demo] building binaries..."
(cd "${ROOT_DIR}" && "${CARGO_BIN}" build -p ark -p ark-hub >/dev/null 2>&1)

cleanup() {
  if [[ -n "${HUB_PID:-}" ]]; then kill "${HUB_PID}" >/dev/null 2>&1 || true; fi
  if [[ -n "${AGENT_PID:-}" ]]; then kill "${AGENT_PID}" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

echo "[demo] starting hub..."
(cd "${ROOT_DIR}" && ./target/debug/ark-hub --ws-listen 127.0.0.1:8080 --http-listen 127.0.0.1:8081 >"${HUB_LOG}" 2>&1) &
HUB_PID=$!

echo "[demo] waiting hub http endpoint..."
for _ in {1..40}; do
  if NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
    curl -fsS "http://127.0.0.1:8081/api/v1/ps" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

echo "[demo] starting agent with rdma mock probe (${MOCK_FILE})..."
(cd "${ROOT_DIR}" && ARK_RDMA_MOCK_FILE="${MOCK_FILE}" ./target/debug/ark run --probe examples/ark-probe-rdma-mock.py --hub-url ws://127.0.0.1:8080 >"${AGENT_LOG}" 2>&1) &
AGENT_PID=$!
sleep 2

echo "[demo] query diagnose API"
NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  curl -fsS "http://127.0.0.1:8081/api/v1/diagnose?job_id=job-rdma-demo&window_s=300" || true
echo

echo "[demo] logs:"
echo "  hub:   ${HUB_LOG}"
echo "  agent: ${AGENT_LOG}"
