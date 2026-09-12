#!/usr/bin/env bash
# Live end-to-end demo: real sellers (x402, The Graph evidence), real Hedera
# testnet settlement through Blocky402, real HCS receipts.
#
# Requirements: .env exported (see .env.example), sellers installed
# (cd sellers && npm install), protoc installed, cargo.
#
# Usage:
#   set -a; source .env; set +a
#   ./scripts/demo-live.sh [--scenario normal|refusal] [--fault drop-response-after-settle]
set -euo pipefail

cd "$(dirname "$0")/.."

SCENARIO=normal
FAULT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --scenario) SCENARIO="$2"; shift 2;;
    --fault) FAULT="$2"; shift 2;;
    *) echo "unknown argument: $1" >&2; exit 2;;
  esac
done

: "${MANDATE_ACCOUNT_ID:?set MANDATE_ACCOUNT_ID (see .env.example)}"
: "${MANDATE_PRIVATE_KEY:?set MANDATE_PRIVATE_KEY (see .env.example)}"
: "${GRAPH_API_KEY:?set GRAPH_API_KEY (see .env.example)}"

SELLER_PORT="${SELLER_PORT:-4021}"
SELLERS_URL="http://localhost:${SELLER_PORT}"

echo "==> building"
cargo build -p mandate-cli

echo "==> starting sellers on :${SELLER_PORT}"
SELLER_FAULTS="$FAULT" GRAPH_API_KEY="$GRAPH_API_KEY" SELLER_PORT="$SELLER_PORT" \
  (cd sellers && npm start >/tmp/mandate-sellers.log 2>&1) &
SELLER_PID=$!
trap 'kill $SELLER_PID 2>/dev/null || true' EXIT

for i in $(seq 1 20); do
  curl -sf "$SELLERS_URL/healthz" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -sf "$SELLERS_URL/healthz" >/dev/null || { echo "sellers did not start; /tmp/mandate-sellers.log:" >&2; tail -20 /tmp/mandate-sellers.log >&2; exit 1; }
echo "==> sellers ready"

echo "==> running mandate run --live --scenario $SCENARIO"
MIRROR_NODE_URL="${MIRROR_NODE_URL:-https://testnet.mirrornode.hedera.com/api/v1}" \
cargo run -q -p mandate-cli -- run \
  --mandate-path mandate-demo/mandate.json \
  --manifest-path mandate-demo/manifest.json \
  --ledger-path mandate-demo/ledger.json \
  --transcript-path mandate-demo/transcript.json \
  --scenario "$SCENARIO" \
  --live --sellers-url "$SELLERS_URL"

echo
echo "==> ledger"
cargo run -q -p mandate-cli -- ledger dev-mandate --ledger-path mandate-demo/ledger.json

echo
echo "==> receipts (sequence + HCS topic)"
cargo run -q -p mandate-cli -- receipts dev-mandate --ledger-path mandate-demo/ledger.json

echo
echo "==> reconcile"
cargo run -q -p mandate-cli -- reconcile dev-mandate \
  --mandate-path mandate-demo/mandate.json \
  --ledger-path mandate-demo/ledger.json || true
