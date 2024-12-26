#!/usr/bin/env bash

export DATABASE_URL="sqlite:tests/util/grizol.db"

if [[ -z "${DATABASE_URL}" ]] ; then
  echo "DATABASE_URL was not set. To set it run e.g. 'source ./scripts/dev_dbg.sh'"
  exit 1
fi

sqlx db reset -y

sqlx migrate run
