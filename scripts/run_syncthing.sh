#!/usr/bin/env -S bash -xe

export PATH=$PATH:/usr/local/go/bin

# rm -rf /tmp/syncthing_home         

ps aux | rg  syncthing | awk '{print $2}' | xargs kill || true

go run build.go

./bin/syncthing --no-upgrade --home /tmp/syncthing_home 2>&1 | tee /tmp/syncthing_log

