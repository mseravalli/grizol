#!/usr/bin/bash -e

dest_dir=$1
file_amount=${2:-1}

if [[ -z ${dest_dir} ]]; then
  echo 'Need to provide a destination dir'
  exit 1
fi

pushd ${dest_dir}

for i in $(seq 1 ${file_amount}); do
  file_size=$(echo "scale = 0; ($RANDOM % 10) * 1000000 + $RANDOM" | bc)
  file_content=$(cat /dev/urandom | head -c ${file_size} | base32)
  file_name=$(echo ${file_content}| head -c 20)

  if [[ $((i%2)) -eq 0 ]]; then
    mkdir -p "dir_${file_name}" 
    echo ${file_content} > "dir_${file_name}/${file_name}"
  else
    echo ${file_content} > ${file_name}
  fi
done

popd
