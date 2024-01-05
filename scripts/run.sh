#!/usr/bin/bash

# to be run from the main directory i.e. by invoking: ./scripts/run.sh

source ./scripts/env.sh

cargo watch -x "check 2>&1 | tee /tmp/grizol_log"
# cargo watch -x "run -- --config tests/util/config.textproto 2>&1 | tee /tmp/grizol_log"
