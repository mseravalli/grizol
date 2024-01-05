#!/usr/bin/bash

# to be run from the main directory i.e. by invoking: ./scripts/test.sh

source ./scripts/env.sh

cargo watch -x "test -- --nocapture"
# cargo watch -x "test core::tests::process_incoming_data__cluster_single_block__succeeds -- --exact --nocapture 2>&1 | tee /tmp/mydama_test"

