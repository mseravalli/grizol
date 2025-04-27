#!/usr/bin/env -S bash -e

export SQLX_OFFLINE_DIR="${PWD}/.sqlx"
export SQLX_OFFLINE=true                                  

cargo sqlx prepare
cargo publish
