#!/usr/bin/bash -e

# To be run from main directory

# trap ctrl-c and call controlled_exit()
trap controlled_exit INT

function controlled_exit() {
  echo "Killing ${GRIZOL_PID} ${SYNCTHING_PID}"

  kill ${GRIZOL_PID} ${SYNCTHING_PID}
}

function trigger_syncthing_rescan() {
  
  # TODO: given the initial setup of syncthing can take some time, we need to find a better way to see when syncthing is ready
  sleep_time="${VARIABLE:-0}"
  sleep ${sleep_time}

  while [[ $(rg 'Device .* client is .grizol' /tmp/syncthing | wc -l) -lt 1 ]]; do sleep 1; done
  curl 'http://localhost:8384/rest/db/scan' \
    -X 'POST' \
    -H 'X-CSRF-Token-RFJWU2I: DRzPJNiGgMhcPchiAEbZPnptv6qsAwXa' \
    --compressed
}

function run_diff() {
  orig_dir=$1
  dest_dir=$2
  res=""
  # check only the files not the directories 
  for f in $(fdfind ".*" ${orig_dir} | rg -v "/$"); do
    f=$(echo $f | sd "${orig_dir}/" "")
    orig_dir_name=$(echo "${orig_dir}" | rg -o "/([^/.]+)$" -r '$1')
    d=$(diff "${orig_dir}/${f}" "${dest_dir}/${orig_dir_name}/${f}" || true)
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

source scripts/env.sh
export RUST_LOG=info,grizol=debug

cargo build
cp target/debug/grizol tests/util/grizol

rm -rf tests/util/syncthing_home/index-v0.14.0.db

scripts/reset_db.sh

ORIG_DIR="tests/util/orig_dir"
DEST_DIR="tests/util/dest_dir"

rm -rf ${ORIG_DIR}
rm -rf ${DEST_DIR}

mkdir -p ${ORIG_DIR}
mkdir -p ${DEST_DIR}

tests/util/grizol --config tests/util/config.textproto > /tmp/grizol 2>&1 &
export GRIZOL_PID=$!
echo "started grizol with pid ${GRIZOL_PID}"

tests/util/syncthing --home tests/util/syncthing_home > /tmp/syncthing 2>&1 &
export SYNCTHING_PID=$!
echo "started syncthing with pid ${SYNCTHING_PID}"

while [[ -z $(rg 'Ready to synchronize "orig_dir"' /tmp/syncthing) ]]; do sleep 1; done
echo "# Test adding data"
scripts/create_random_files.sh ${ORIG_DIR}/ 3
trigger_syncthing_rescan 2
while [[ $(rg 'Stored whole file' /tmp/grizol | wc -l) -ne 3 ]]; do sleep 1; done
run_diff ${ORIG_DIR} ${DEST_DIR}

echo "# Test adding more data"
scripts/create_random_files.sh ${ORIG_DIR}/ 3
trigger_syncthing_rescan
while [[ $(rg 'Stored whole file' /tmp/grizol | wc -l) -ne 6 ]]; do sleep 1; done
run_diff ${ORIG_DIR} ${DEST_DIR}

echo "# Test modifying data, change file content"
file_name=$(ls ${ORIG_DIR}/ | sort | head -n 1)
cat /dev/urandom | head -c 100 | base32  > "${ORIG_DIR}/${file_name}"
trigger_syncthing_rescan
while [[ $(rg 'Stored whole file' /tmp/grizol | wc -l) -ne 7 ]]; do sleep 1; done
run_diff ${ORIG_DIR} ${DEST_DIR}

echo "# Test modifying data, remove file content"
file_name=$(ls ${ORIG_DIR}/ | sort | head -n 1)
head -c 1 "${ORIG_DIR}/${file_name}" > "${ORIG_DIR}/${file_name}" 
trigger_syncthing_rescan
while [[ $(rg 'Stored whole file' /tmp/grizol | wc -l) -ne 8 ]]; do sleep 1; done
run_diff ${ORIG_DIR} ${DEST_DIR}

echo "# Test deleting data"
file_name=$(ls ${ORIG_DIR}/ | sort | head -n 1)
rm "${ORIG_DIR}/${file_name}"
trigger_syncthing_rescan
while [[ $(rg 'File .* was deleted on device' /tmp/grizol | wc -l) -ne 1 ]]; do sleep 1; done
if [[ ! -f ${ORIG_DIR}/{file_name} && -f ${DEST_DIR}/${file_name} ]]; then
    echo "Success: file was deleted remotely but not locally"
fi
run_diff ${ORIG_DIR} ${DEST_DIR}

# Ensure not to be waiting forever
# sleep 10000
kill ${GRIZOL_PID} ${SYNCTHING_PID}
