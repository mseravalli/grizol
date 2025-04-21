#!/usr/bin/env bats

# To be run from main directory

GRIZOL_LOG="/tmp/grizol_log"
SYNCTHING_LOG="/tmp/syncthing_log"

if [[ -z "${GRIZOL_CONFIG_TYPE}" ]] ; then
  echo "GRIZOL_CONFIG_TYPE was not set. To set it run e.g. 'source ./scripts/dev_dbg.sh'"
  exit 1
fi

setup_file() {
  source scripts/dev_dbg.sh

  echo "Running config type: '${GRIZOL_CONFIG_TYPE}'"
  source "scripts/${GRIZOL_CONFIG_TYPE}.sh"
  export RUST_LOG=info,grizol=debug

  cargo build --release
  cp target/release/grizol tests/util/grizol

  rm -rf tests/util/syncthing_home/index-v0.14.0.db

  scripts/reset_db.sh

  export ORIG_DIR="tests/util/orig_dir"
  export STAGING_AREA="tests/util/dest_dir"

  rm -rf ${ORIG_DIR}
  rm -rf ${STAGING_AREA}

  mkdir -p ${ORIG_DIR}
  mkdir -p ${STAGING_AREA}

  yes z | xargs echo -n 2>/dev/null | head -c 900 | fmt > ${ORIG_DIR}/z.txt

  tests/util/grizol --config tests/util/config/config-test.textproto > ${GRIZOL_LOG} 2>&1 &
  export GRIZOL_PID=$!
  echo "Started grizol with pid ${GRIZOL_PID}"

  tests/util/syncthing --no-upgrade --home tests/util/syncthing_home > ${SYNCTHING_LOG} 2>&1 &
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
  STAGING_AREA=$2
  res=""
  # check only the files not the directories 
  for f in $(fdfind ".*" ${orig_dir} | rg -v "/$"); do
    f=$(echo $f | sd "${orig_dir}/" "")
    orig_dir_name=$(echo "${orig_dir}" | rg -o "/([^/.]+)$" -r '$1')
    d=$(diff "${orig_dir}/${f}" "${STAGING_AREA}/${orig_dir_name}/${f}" 2>&1 || true)
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

  echo "aaaaaaaaaaaaaaaaaaaa" > ${ORIG_DIR}/a.txt
  echo "bbbbbbbbbbbbbbbbbbbb" > ${ORIG_DIR}/b.txt
  mkdir ${ORIG_DIR}/c_dir
  yes c | xargs echo -n 2>/dev/null | head -c 9000000 | fmt > ${ORIG_DIR}/c_dir/c1.txt
  echo "c2c2c2c2c2c2c2c2c2c2" > ${ORIG_DIR}/c_dir/c2.txt

  trigger_syncthing_rescan 2
  total_files=$(fdfind ".*" tests/util/orig_dir/ | rg -v "/$" | wc -l)
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne "${total_files}" ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${STAGING_AREA}
  assert_success
}

@test "Syncthing: add more data" {
  yes d | xargs echo -n 2>/dev/null | head -c 900 | fmt > ${ORIG_DIR}/d.txt
  yes e | xargs echo -n 2>/dev/null | head -c 90000 | fmt > ${ORIG_DIR}/e.txt
  mkdir ${ORIG_DIR}/f_dir
  yes f | xargs echo -n 2>/dev/null | head -c 9000000 | fmt > ${ORIG_DIR}/f_dir/f.txt
  trigger_syncthing_rescan
  total_files=$(fdfind ".*" tests/util/orig_dir/ | rg -v "/$" | wc -l)
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne "${total_files}" ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${STAGING_AREA}
  assert_success
}

@test "Syncthing: change file content" {
  file_name=a.txt
  yes a | head -c 100 | base32  > "${ORIG_DIR}/${file_name}"
  trigger_syncthing_rescan
  total_files=$(fdfind ".*" tests/util/orig_dir/ | rg -v "/$" | wc -l)
  # we have total_files + 1 because we performed 1 additional operation on the files
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne $(echo "${total_files} + 1" | bc) ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${STAGING_AREA}
  assert_success
}

@test "Syncthing: remove file content" {
  file_name=a.txt
  head -c 1 "${ORIG_DIR}/${file_name}" > "${ORIG_DIR}/${file_name}" 
  trigger_syncthing_rescan
  total_files=$(fdfind ".*" tests/util/orig_dir/ | rg -v "/$" | wc -l)
  # we have total_files + 2 because we performed 2 additional operations on the files
  while [[ $(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l) -ne $(echo "${total_files} + 2" | bc) ]]; do sleep 1; done
  run run_diff ${ORIG_DIR} ${STAGING_AREA}
  assert_success
}

@test "Syncthing: delete file - only remotely" {
  file_name=a.txt
  rm "${ORIG_DIR}/${file_name}"
  trigger_syncthing_rescan
  while [[ $(rg 'File .* was deleted on device' ${GRIZOL_LOG} | wc -l) -ne 1 ]]; do sleep 1; done
  if [[ ! -f ${ORIG_DIR}/{file_name} && -f ${STAGING_AREA}/${file_name} ]]; then
      echo "Success: file was deleted remotely but not locally"
  fi
  run run_diff ${ORIG_DIR} ${STAGING_AREA}
  assert_success
}

@test "Fuse: ls files" {
  expected_ls=". ./a.txt ./b.txt ./c_dir ./c_dir/c1.txt ./c_dir/c2.txt ./d.txt ./e.txt ./f_dir ./f_dir/f.txt ./z.txt"
  cd tests/util/fuse_mountpoint/orig_dir/
  run diff <(echo ${expected_ls}) <(find . | sort | paste -d ' ' -s ) 
  assert_success
}

@test "Syncthing: add data for mv" {
  while [[ -z $(rg 'Ready to synchronize "orig_dir"' ${SYNCTHING_LOG}) ]]; do sleep 1; done

  total_files_before=$(fdfind ".*" tests/util/orig_dir/ | rg -v "/$" | wc -l)
  storage_triggers_before=$(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l)

  # create several subdirectories to be able to better test mv
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2a/c_dir_sub_3a
  echo "1a_2a_3a_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2a/c_dir_sub_3a/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2a/c_dir_sub_3b
  echo "1a_2a_3b_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2a/c_dir_sub_3b/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2b/c_dir_sub_3a
  echo "1a_2b_3a_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2b/c_dir_sub_3a/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2b/c_dir_sub_3b
  echo "1a_2b_3b_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1a/c_dir_sub_2b/c_dir_sub_3b/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2a/c_dir_sub_3a
  echo "1b_2a_3a_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2a/c_dir_sub_3a/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2a/c_dir_sub_3b
  echo "1b_2a_3b_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2a/c_dir_sub_3b/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3a
  echo "1b_2b_3a_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3a/c.txt
  mkdir -p ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3b
  echo "1b_2b_3b_c" > ${ORIG_DIR}/c_dir/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3b/c.txt

  trigger_syncthing_rescan 2
  total_files_after=$(fdfind ".*" tests/util/orig_dir/ | rg -v "/$" | wc -l)
  while true; do
    storage_triggers_after=$(rg 'Stored whole file' ${GRIZOL_LOG} | wc -l)
    diff=$(echo "${total_files_after} - ${total_files_before} - (${storage_triggers_after} - ${storage_triggers_before})" | bc)
    if [[ diff -eq "0" ]] ; then
      break
    fi
    sleep 1;
  done
  run run_diff ${ORIG_DIR} ${STAGING_AREA}
  assert_success
}

@test "Fuse: mv file: rename in top dir" {
  mv tests/util/fuse_mountpoint/orig_dir/b{,_moved}.txt
  expected_ls="tests/util/fuse_mountpoint/orig_dir/b_moved.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/b*) 
  assert_success

  have=$(cat tests/util/fuse_mountpoint/orig_dir/b_moved.txt)
  want="bbbbbbbbbbbbbbbbbbbb"
  assert_equal ${have} ${want}
}

@test "Fuse: mv file: rename in sub dir" {
  mv tests/util/fuse_mountpoint/orig_dir/c_dir/c1{,_moved}.txt
  expected_ls="tests/util/fuse_mountpoint/orig_dir/c_dir/c1_moved.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/c_dir/c1*) 
  assert_success

  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir/c1_moved.txt)
  want="c2c2c2c2c2c2c2c2c2c2"
  assert_equal ${have} ${want}
}

@test "Fuse: mv file: sub dir to top dir" {
  mv tests/util/fuse_mountpoint/orig_dir/c_dir/c2.txt tests/util/fuse_mountpoint/orig_dir/c2.txt
  expected_ls="c1_moved.txt c_dir_sub_1a c_dir_sub_1b"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/c_dir/ | sort | paste -d ' ' -s) 
  assert_success
  expected_ls="a.txt b_moved.txt c2.txt c_dir d.txt e.txt f_dir z.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/ | sort | paste -d ' ' -s) 
  assert_success

  have=$(cat tests/util/fuse_mountpoint/orig_dir/c2.txt)
  want="c2c2c2c2c2c2c2c2c2c2"
  assert_equal ${have} ${want}
}

@test "Fuse: mv file: top dir to sub dir" {
  mv tests/util/fuse_mountpoint/orig_dir/c2.txt tests/util/fuse_mountpoint/orig_dir/c_dir/c2.txt
  expected_ls="c1_moved.txt c2.txt c_dir_sub_1a c_dir_sub_1b"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/c_dir/ | sort | paste -d ' ' -s) 
  assert_success
  expected_ls="a.txt b_moved.txt c_dir d.txt e.txt f_dir z.txt"
  run diff <(echo ${expected_ls}) <(ls tests/util/fuse_mountpoint/orig_dir/ | sort | paste -d ' ' -s) 
  assert_success

  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir/c2.txt)
  want="c2c2c2c2c2c2c2c2c2c2"
  assert_equal ${have} ${want}
}

@test "Fuse: mv dir: top dir to top dir" {
  want=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir/" "" | sort | paste -s -d ' ')
  run mv tests/util/fuse_mountpoint/orig_dir/{c_dir,c_dir_moved}
  assert_success

  have=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir_moved/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir_moved/" "" | sort | paste -s -d ' ')
  assert_equal "${have}" "${want}"

  want="1a_2b_3a_c"
  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir_moved/c_dir_sub_1a/c_dir_sub_2b/c_dir_sub_3a/c.txt)
  assert_equal "${have}" "${want}"

  want="1b_2b_3b_c"
  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir_moved/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3b/c.txt)
  assert_equal "${have}" "${want}"

  run mv tests/util/fuse_mountpoint/orig_dir/{c_dir_moved,c_dir}
  assert_success
}

@test "Fuse: mv dir: sub dir to top dir and back" {
  want=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/" "" | sort | paste -s -d ' ')
  run mv tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b
  assert_success

  have=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b/" "" | sort | paste -s -d ' ')
  assert_equal "${have}" "${want}"

  want="1b_2b_3b_c"
  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3b/c.txt)
  assert_equal "${have}" "${want}"

  want=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b/" "" | sort | paste -s -d ' ')
  run mv tests/util/fuse_mountpoint/orig_dir/c_dir_sub_1b tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b 
  assert_success

  have=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/" "" | sort | paste -s -d ' ')
  assert_equal "${have}" "${want}"

  want="1b_2b_3b_c"
  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/c_dir_sub_2b/c_dir_sub_3b/c.txt)
  assert_equal "${have}" "${want}"
}

@test "Fuse: mv dir: sub dir to sub dir" {
  want=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b/" "" | sort | paste -s -d ' ')
  run mv tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1c
  assert_success

  have=$(fdfind ".*" tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1c/ | sd "tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1c/" "" | sort | paste -s -d ' ')
  assert_equal "${have}" "${want}"

  want="1b_2b_3b_c"
  have=$(cat tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1c/c_dir_sub_2b/c_dir_sub_3b/c.txt)
  assert_equal "${have}" "${want}"

  run mv tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1c tests/util/fuse_mountpoint/orig_dir/c_dir/c_dir_sub_1b
  assert_success
}

@test "Fuse: mv dir: dir to new folder" {
  assert_equal "todo" "todo"
}

