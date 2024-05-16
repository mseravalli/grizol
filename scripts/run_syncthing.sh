#!/usr/bin/env -S bash -x

# To be run from a local directory containing a clone of the syncthing repo

export PATH=$PATH:/usr/local/go/bin

rm -rf /tmp/syncthing_home
rm -rf ~/Sync/*

cat /dev/urandom | head -c 200 | base32 > ~/Sync/random_000
cat /dev/urandom | head -c 200 | base32 > ~/Sync/random_001
cat /dev/urandom | head -c 200 | base32 > ~/Sync/random_002

bash -c "ps aux | rg  syncthing | awk '{print $2}' | xargs kill" || true

rm ./bin/syncthing

go run build.go

./bin/syncthing --no-upgrade --home /tmp/syncthing_home 2>&1 | tee /tmp/syncthing_log

