#!/usr/bin/env bash

# TODO: these should probably be the default values 
read -r -d '' CONFIG << EOM
trusted_peers: [
  "BTTVHUR-CKWU5YX-JMAULFO-5CMEQ36-FZWEWAE-QFXROCM-WKMOJPZ-KEUOWAS",
  "RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY",
  "5CLQ5SC-YWEKDFL-5BVSUPT-DGAP4DL-K4DDBSN-PEIIWEW-FJWWGVG-XEBIEQ2"
]
remote_base_dir: "seravalli-grizol"
EOM

GRIZOL_TMP_DIR=$(mktemp -d)

echo "Grizol data will be available at ${GRIZOL_TMP_DIR}"

mkdir -p ${GRIZOL_TMP_DIR}/config
mkdir -p ${GRIZOL_TMP_DIR}/data
mkdir -p ${GRIZOL_TMP_DIR}/state

echo "${CONFIG}" > ${GRIZOL_TMP_DIR}/config/config.textproto

export DATABASE_URL="sqlite:${GRIZOL_TMP_DIR}/state/grizol.db"
sqlx db reset -y
sqlx migrate run

cp $PWD/tests/util/cert.pem        ${GRIZOL_TMP_DIR}/config/cert.pem
cp $PWD/tests/util/key.pem         ${GRIZOL_TMP_DIR}/config/key.pem
cp $PWD/tests/util/rclone.conf.key ${GRIZOL_TMP_DIR}/config/rclone.conf

docker run \
  --rm \
  --name grizol \
  -it \
  -p 23456:23456 \
  --volume=${GRIZOL_TMP_DIR}/config:/opt/grizol/config \
  --volume=${GRIZOL_TMP_DIR}/data:/opt/grizol/data\
  --volume=${GRIZOL_TMP_DIR}/state:/opt/grizol/state \
  grizol/grizol:latest
