#!/usr/bin/env bash

RAW_DATA=/tmp/grizol.perf.raw

sudo perf record -g -F max -p $(ps aux | rg target/debug/grizol | rg -v "rg target" | awk '{print $2}') -o ${RAW_DATA}

sudo chown marco ${RAW_DATA}

perf script -F +pid -i ${RAW_DATA} > bench/grizol.perf

rm ${RAW_DATA}
