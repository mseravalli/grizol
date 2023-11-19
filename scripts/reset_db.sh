#!/usr/bin/env bash

export DATABASE_URL="sqlite:target/grizol.db"

sqlx db reset -y
sqlx migrate run
