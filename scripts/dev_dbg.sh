#!/usr/bin/bash

export GRIZOL_CONFIG_TYPE="dev_dbg"

export RUSTFLAGS=-Awarnings
export RUST_LOG=warn,grizol=debug,fuser=info

export RUST_BACKTRACE=1
export DATABASE_URL="sqlite:tests/util/grizol.db"
export GRIZOL_CONFIG_PATH="tests/util/config/config-test.textproto"
