#!/usr/bin/bash -e

# To be run from main directory

# trap ctrl-c and call controlled_exit()
trap controlled_exit INT

function controlled_exit() {
  echo "Killing ${GRIZOL_PID} ${SYNCTHING_PID}"

  kill ${GRIZOL_PID} ${SYNCTHING_PID}
}

function run_diff() {
  orig_dir=$1
  dest_dir=$2
  res=""
  for f in $(ls ${orig_dir}); do
    d=$(diff "${orig_dir}/${f}" "${dest_dir}/${f}" || true)
    if [[ ! -z "$d" ]]; then
      res="${f} ${res}"
    fi
  done
  if [[ -z ${res} ]]; then 
    echo "Success: No diffs found"
  else
    echo "Failures: Diffs found in ${res}"
  fi
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

while [[ -z $(rg 'Ready to synchronize "orig_dir"' /tmp/syncthing) ]]; do sleep 1; done

echo "# 1: Test adding data"
scripts/create_random_files.sh tests/util/orig_dir/ 3
while [[ $(rg 'Stored whole file' /tmp/grizol | wc -l) -ne 3 ]]; do sleep 1; done
run_diff tests/util/orig_dir tests/util/dest_dir

echo "# 2: Test adding more data"
scripts/create_random_files.sh tests/util/orig_dir/ 3
while [[ $(rg 'Stored whole file' /tmp/grizol | wc -l) -ne 6 ]]; do sleep 1; done
run_diff tests/util/orig_dir tests/util/dest_dir

# # 3: Test modifying data
# file_name=$(ls tests/util/orig_dir/ | sort | head -n 1)
# head -c 100 "tests/util/orig_dir/${file_name}" > "tests/util/orig_dir/${file_name}" 
# run_diff tests/util/orig_dir tests/util/dest_dir

# Ensure not to be waiting forever
sleep 10000
kill ${GRIZOL_PID} ${SYNCTHING_PID}
