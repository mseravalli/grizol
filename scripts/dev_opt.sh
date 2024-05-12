#!/usr/bin/bash

export GRIZOL_CONFIG_TYPE="dev_opt"

export RUSTFLAGS=""
export RUST_LOG=warn

export RUST_BACKTRACE=1
export DATABASE_URL="sqlite:tests/util/grizol.db"
export GRIZOL_CONFIG_PATH="tests/util/config/config-test.textproto"
