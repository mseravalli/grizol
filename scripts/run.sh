#!/usr/bin/bash

# to be run from the main directory i.e. by invoking: ./scripts/run.sh

source ./scripts/env.sh

cmd=$1

if [[ ${cmd} == "check" ]]; then 
  cargo watch -x "check 2>&1 | tee /tmp/grizol_log"
elif [[ ${cmd} == "run" ]]; then
  cargo watch -x "run -- --config tests/util/config.textproto 2>&1 | tee /tmp/grizol_log"
else
  echo "Allowed values are only 'watch' and 'run', but '${cmd}' was provided instead"
fi


