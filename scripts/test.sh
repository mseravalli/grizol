#!/usr/bin/bash

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=debug

# cargo watch -x "test -- --nocapture"
cargo watch -x "test -- --nocapture 2>&1 | tee /tmp/mydama_test"

