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

CONFIG_PATH=$(mktemp)

echo "confing will be available at ${CONFIG_PATH}"

echo "${CONFIG}" > ${CONFIG_PATH}

docker run \
  --rm \
  --name grizol \
  -it \
  -p 23456:23456 \
  --volume=${CONFIG_PATH}:/opt/grizol/config/config.textproto \
  --volume=$PWD/tests/util/cert.pem:/opt/grizol/config/cert.pem \
  --volume=$PWD/tests/util/key.pem:/opt/grizol/config/key.pem \
  grizol/grizol:latest
