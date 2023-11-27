#!/usr/bin/bash

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=info,grizol=debug
export DATABASE_URL="sqlite:target/grizol.db"

cargo watch -x "run -- --certs /home/marco/downloads/cert.pem --key /home/marco/downloads/key.pem --port 23456 --require-auth=false --proto 'bep/1.0' 2>&1 | tee /tmp/grizol_log"
# cargo watch -x "run -- --certs /home/marco/downloads/cert.pem --key /home/marco/downloads/key.pem --port 23456 --require-auth=false --proto 'bep/1.0' > /tmp/grizol_log 2>&1"
