#!/usr/bin/bash -e

dest_dir=$1

if [[ -z ${dest_dir} ]]; then
  echo 'Need to provide a destination dir'
  exit 1
fi

pushd ${dest_dir}

for i in $(seq 1 3); do
  file=$(cat /dev/urandom | head -c 2000000 | base32)
  echo ${file} > $(echo ${file}| head -c 20)
done

popd
