#!/usr/bin/env bash

export DATABASE_URL="sqlite:target/grizol.db"

mkdir -p target
sqlx db create

# sqlx db reset -y
sqlx migrate run
