#!/usr/bin/env bash

# TODO: these should probably be the default values 
read -r -d '' CONFIG << EOM
cert: "config/cert.pem"
key: "config/key.pem"
trusted_peers: [
  "BTTVHUR-CKWU5YX-JMAULFO-5CMEQ36-FZWEWAE-QFXROCM-WKMOJPZ-KEUOWAS",
  "RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY",
  "5CLQ5SC-YWEKDFL-5BVSUPT-DGAP4DL-K4DDBSN-PEIIWEW-FJWWGVG-XEBIEQ2"
]
base_dir: "data"
db_url: "sqlite:state/grizol.db"
storage_strategy: 1
rclone_config: "config/rclone.conf"
remote_base_dir: "seravalli-grizol"
EOM

CONFIG_DIR=$(mktemp -d)

echo "confing will be available at ${CONFIG_DIR}"

echo "${CONFIG}" > ${CONFIG_DIR}/config.textproto

export DATABASE_URL="sqlite:${CONFIG_DIR}/grizol.db"
sqlx db reset -y
sqlx migrate run

docker run \
  --rm \
  --name grizol \
  -d \
  -p 23456:23456 \
  --volume=${CONFIG_DIR}/config.textproto:/opt/grizol/config/config.textproto \
  --volume=${CONFIG_DIR}/grizol.db:/opt/grizol/state/grizol.db \
  --volume=$PWD/tests/util/cert.pem:/opt/grizol/config/cert.pem \
  --volume=$PWD/tests/util/key.pem:/opt/grizol/config/key.pem \
  grizol/grizol:latest
