#!/usr/bin/env bash

export DATABASE_URL="sqlite:tests/util/grizol.db"

# sqlx db create
sqlx db reset -y

sqlx migrate run
