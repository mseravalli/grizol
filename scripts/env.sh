#!/usr/bin/bash

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=info,grizol=debug
export DATABASE_URL="sqlite:tests/util/grizol.db"
