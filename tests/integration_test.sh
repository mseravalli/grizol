#!/usr/bin/bash -e

# To be run from main directory

# trap ctrl-c and call controlled_exit()
trap controlled_exit INT

function controlled_exit() {
  echo "Killing ${GRIZOL_PID} ${SYNCTHING_PID}"

  kill ${GRIZOL_PID} ${SYNCTHING_PID}
}

export RUST_BACKTRACE=1
export RUSTFLAGS=-Awarnings
export RUST_LOG=info,grizol=debug
export DATABASE_URL="sqlite:tests/util/grizol.db"
cargo build
cp target/debug/grizol tests/util/grizol

rm -rf tests/util/syncthing_home/index-v0.14.0.db

scripts/reset_db.sh

rm -rf tests/util/orig_dir
rm -rf tests/util/dest_dir

mkdir -p tests/util/orig_dir
mkdir -p tests/util/dest_dir

tests/util/grizol --config tests/util/config.textproto > /tmp/grizol 2>&1 &
export GRIZOL_PID=$!
echo "started grizol with pid ${GRIZOL_PID}"

tests/util/syncthing --home tests/util/syncthing_home > /tmp/syncthing 2>&1 &
export SYNCTHING_PID=$!
echo "started syncthing with pid ${SYNCTHING_PID}"

sleep 4

scripts/create_random_files.sh tests/util/orig_dir/ 3

# Ensure not to be waiting forever
sleep 10000
kill ${GRIZOL_PID} ${SYNCTHING_PID}
