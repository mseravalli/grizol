#!/usr/bin/env bats

# To be run from main directory

GRIZOL_LOG="/tmp/grizol_log"
SYNCTHING_LOG="/tmp/syncthing_log"

setup_file() {
  source scripts/env.sh
  export RUST_LOG=info,grizol=debug

  cargo build --release
  cp target/debug/grizol tests/util/grizol

  rm -rf tests/util/syncthing_home/index-v0.14.0.db

  scripts/reset_db.sh

  export ORIG_DIR="tests/util/orig_dir"
  export DEST_DIR="tests/util/dest_dir"

  rm -rf ${ORIG_DIR}
  rm -rf ${DEST_DIR}

  mkdir -p ${ORIG_DIR}
  mkdir -p ${DEST_DIR}

  yes z | xargs echo -n 2>/dev/null | head -c 900 | fmt > ${ORIG_DIR}/z.txt

  tests/util/grizol --config tests/util/config.textproto > ${GRIZOL_LOG} 2>&1 &
  export GRIZOL_PID=$!
  echo "Started grizol with pid ${GRIZOL_PID}"

  tests/util/syncthing --home tests/util/syncthing_home > ${SYNCTHING_LOG} 2>&1 &
  export SYNCTHING_PID=$!
  echo "Started syncthing with pid ${SYNCTHING_PID}" 
}

teardown_file() {
  echo "Killing grizol (${GRIZOL_PID}) and syncthing (${SYNCTHING_PID})"

  kill ${GRIZOL_PID} ${SYNCTHING_PID}
}

setup() {
  load '/usr/lib/bats/bats-support/load.bash'
  load '/usr/lib/bats/bats-assert/load.bash'
}

trigger_syncthing_rescan() {
  # TODO: given the initial setup of syncthing can take some time, we need to find a better way to see when syncthing is ready
  sleep_time="${VARIABLE:-0}"
  sleep ${sleep_time}

  while [[ $(rg 'Device .* client is .grizol' ${SYNCTHING_LOG} | wc -l) -lt 1 ]]; do sleep 1; done
  curl 'http://localhost:8384/rest/db/scan' \
    -X 'POST' \
    -H "X-API-Key: un66wPwNPteiJ6Rm26UrMhwCgmetimvj" \
    --compressed
}

run_diff() {
  orig_dir=$1
  dest_dir=$2
  res=""
  # check only the files not the directories 
  for f in $(fdfind ".*" ${orig_dir} | rg -v "/$"); do
    f=$(echo $f | sd "${orig_dir}/" "")
    orig_dir_name=$(echo "${orig_dir}" | rg -o "/([^/.]+)$" -r '$1')
    d=$(diff "${orig_dir}/${f}" "${dest_dir}/${orig_dir_name}/${f}" 2>&1 || true)
    if [[ ! -z "$d" ]]; then
      res="affected: ${f} - ${d}; ${res}"
    fi
  done
  if [[ -z ${res} ]]; then 
    # Success: No diffs found
    exit 0
  else
    echo ${res}
    # Failures: Diffs found
    exit 1
  fi
}

@test "Syncthing: add data" {
while [[ -z $(rg 'Ready to synchronize "orig_dir"' ${SYNCTHING_LOG}) ]]; do sleep 1; done

  yes a | xargs echo -n 2>/dev/null | head -c 900 | fmt > ${ORIG_DIR}/a.txt
  yes b | xargs echo -n 2>/dev/null | head -c 90000 | fmt > ${ORIG_DIR}/b.txt
  mkdir ${ORIG_DIR}/c_dir
  yes c | xargs echo -n 2>/dev/null | head -c 9000000 | fmt > ${ORIG_DIR}/c_dir/c1.txt
  yes c | xargs echo -n 2>/dev/null | head -c 9000000 | fmt > ${ORIG_DIR}/c_dir/c2.txt

  trigger_syncthing_rescan 2
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne 5 ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${DEST_DIR}

  assert_success
}

@test "Syncthing: add more data" {
  yes d | xargs echo -n 2>/dev/null | head -c 900 | fmt > ${ORIG_DIR}/d.txt
  yes e | xargs echo -n 2>/dev/null | head -c 90000 | fmt > ${ORIG_DIR}/e.txt
  mkdir ${ORIG_DIR}/f_dir
  yes f | xargs echo -n 2>/dev/null | head -c 9000000 | fmt > ${ORIG_DIR}/f_dir/f.txt
  trigger_syncthing_rescan
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne 8 ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${DEST_DIR}
  assert_success
}

@test "Syncthing: change file content" {
  file_name=a.txt
  yes a | head -c 100 | base32  > "${ORIG_DIR}/${file_name}"
  trigger_syncthing_rescan
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne 9 ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${DEST_DIR}
  assert_success
}

@test "Syncthing: remove file content" {
  file_name=a.txt
  head -c 1 "${ORIG_DIR}/${file_name}" > "${ORIG_DIR}/${file_name}" 
  trigger_syncthing_rescan
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne 10 ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${DEST_DIR}
  assert_success
}

@test "Syncthing: delete file - only remotely" {
  file_name=a.txt
  rm "${ORIG_DIR}/${file_name}"
  trigger_syncthing_rescan
  while [[ $(rg 'File .* was deleted on device' ${GRIZOL_LOG} | wc -l) -ne 1 ]]; do sleep 1; done
  if [[ ! -f ${ORIG_DIR}/{file_name} && -f ${DEST_DIR}/${file_name} ]]; then
      echo "Success: file was deleted remotely but not locally"
  fi
  run run_diff ${ORIG_DIR} ${DEST_DIR}
  assert_success
}

@test "Fuse: ls files" {
  expected_ls=". ./a.txt ./b.txt ./c_dir ./c_dir/c1.txt ./c_dir/c2.txt ./d.txt ./e.txt ./f_dir ./f_dir/f.txt ./z.txt"
  cd tests/util/fuse_mountpoint/orig_dir/
  run diff <(echo ${expected_ls}) <(find . | sort | paste -d ' ' -s ) 
  assert_success
}

@test "Fuse: mv files top dir to top dir" {
  mv tests/util/fuse_mountpoint/orig_dir/a{,_moved}.txt
  expected_ls="tests/util/fuse_mountpoint/orig_dir/a_moved.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/a*) 
  assert_success
}

@test "Fuse: mv files sub dir to sub dir" {
  mv tests/util/fuse_mountpoint/orig_dir/c_dir/c1{,_moved}.txt
  expected_ls="tests/util/fuse_mountpoint/orig_dir/c_dir/c1_moved.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/c_dir/c1*) 
  assert_success
}

@test "Fuse: mv files sub dir to top dir" {
  mv tests/util/fuse_mountpoint/orig_dir/c_dir/c2.txt tests/util/fuse_mountpoint/orig_dir/c2.txt
  expected_ls="c1_moved.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/c_dir/ | sort | paste -d ' ' -s) 
  assert_success
  expected_ls="a_moved.txt b.txt c2.txt c_dir d.txt e.txt f_dir z.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/ | sort | paste -d ' ' -s) 
  assert_success
}

@test "Fuse: mv files top dir to sub dir" {
  mv tests/util/fuse_mountpoint/orig_dir/c2.txt tests/util/fuse_mountpoint/orig_dir/c_dir/c2.txt
  expected_ls="c1_moved.txt c2.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/c_dir/ | sort | paste -d ' ' -s) 
  assert_success
  expected_ls="a_moved.txt b.txt c_dir d.txt e.txt f_dir z.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/ | sort | paste -d ' ' -s) 
  assert_success
}

@test "Fuse: cat files" {
  run cat tests/util/fuse_mountpoint/orig_dir/c_dir/c2.txt
  assert_success
}
