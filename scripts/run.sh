#!/usr/bin/bash

# to be run from the main directory i.e. by invoking: ./scripts/run.sh

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=info,grizol=debug
export DATABASE_URL="sqlite:tests/util/grizol.db"

cargo watch -x "run -- --config tests/util/config.textproto 2>&1 | tee /tmp/grizol_log"
