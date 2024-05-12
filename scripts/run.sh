#!/usr/bin/bash

# to be run from the main directory i.e. by invoking: ./scripts/run.sh

if [[ -z "${GRIZOL_CONFIG_TYPE}" ]] ; then
  echo "GRIZOL_CONFIG_TYPE was not set. To set it run e.g. 'source ./scripts/dev_dbg.sh'"
  exit 1
fi

echo "Running config type: '${GRIZOL_CONFIG_TYPE}'"
source "scripts/${GRIZOL_CONFIG_TYPE}.sh"

cmd=$1

if [[ ${cmd} == "check" ]]; then 
  cargo watch -x "check 2>&1 | tee /tmp/grizol_log"
elif [[ ${cmd} == "run" ]]; then
  cargo watch -x "run -- --config ${GRIZOL_CONFIG_PATH} 2>&1 | tee /tmp/grizol_log"
else
  echo "Allowed values are only 'check' and 'run', but '${cmd}' was provided instead"
fi


