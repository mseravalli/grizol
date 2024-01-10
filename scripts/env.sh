#!/usr/bin/bash

# Config for quick and dirty 
# export RUSTFLAGS=-Awarnings
# export RUST_LOG=info,grizol=debug

# Config for better quality
export RUSTFLAGS=""
export RUST_LOG=info

export RUST_BACKTRACE=1
export DATABASE_URL="sqlite:tests/util/grizol.db"
