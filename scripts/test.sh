#!/usr/bin/bash

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=debug

cargo watch -x "test -- --nocapture"
# cargo watch -x "test core::tests::process_incoming_data__cluster_single_block__succeeds -- --exact --nocapture 2>&1 | tee /tmp/mydama_test"

