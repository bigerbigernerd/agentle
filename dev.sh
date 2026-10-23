#!/usr/bin/env bash
# Local preview: builds the release binary and serves it with a throwaway data dir.
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release -q
export AGENTLE_DATA="${AGENTLE_DATA:-.dev-data}"
export AGENTLE_ADDR="${AGENTLE_ADDR:-127.0.0.1:18990}"
export AGENTLE_ROOT=.
# local claim links must point at this machine; production uses the default https://agentle.pocketplay.win
export AGENTLE_BASE="${AGENTLE_BASE:-http://$AGENTLE_ADDR}"
export AGENTLE_REG_PER_IP="${AGENTLE_REG_PER_IP:-200}"
exec ./target/release/agentle
