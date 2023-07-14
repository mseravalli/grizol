#!/usr/bin/bash

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=debug

cargo watch -x "run -- --certs /home/marco/downloads/cert.pem --key /home/marco/downloads/key.pem --port 23456 --require-auth=false --proto 'bep/1.0' 2>&1 | tee /tmp/mydama"
# cargo watch -x "run -- --certs /home/marco/downloads/cert.pem --key /home/marco/downloads/key.pem --port 23456 --require-auth=false --proto 'bep/1.0' > /tmp/mydama 2>&1"
