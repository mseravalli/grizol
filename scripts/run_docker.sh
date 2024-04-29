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

mkdir -p /tmp/grizol-testing
GRIZOL_TMP_DIR=$(mktemp -d -p /tmp/grizol-testing)

echo "Grizol temp dir is ${GRIZOL_TMP_DIR}"

mkdir -p ${GRIZOL_TMP_DIR}/config
mkdir -p ${GRIZOL_TMP_DIR}/staging_area
mkdir -p ${GRIZOL_TMP_DIR}/state
mkdir -p ${GRIZOL_TMP_DIR}/fuse_mountpoint
mkdir -p ${GRIZOL_TMP_DIR}/read_cache_dir

echo "${CONFIG}" > ${GRIZOL_TMP_DIR}/config/config.textproto

export DATABASE_URL="sqlite:${GRIZOL_TMP_DIR}/state/grizol.db"
sqlx db reset -y
sqlx migrate run

cp $PWD/tests/util/cert.pem         ${GRIZOL_TMP_DIR}/config/cert.pem
cp $PWD/tests/util/key.pem          ${GRIZOL_TMP_DIR}/config/key.pem
cp $PWD/ops/rclone.conf.key         ${GRIZOL_TMP_DIR}/config/rclone.conf
cp $PWD/ops/gcp-sa-storage.json.key ${GRIZOL_TMP_DIR}/config/gcp-sa-storage.json

# Image built with following command
# docker build -f ops/Dockerfile -t registry.gitlab.com/com.marcoseravalli/grizol:latest .

docker run \
  --rm \
  --name grizol \
  -it \
  -p 23456:23456 \
  --device /dev/fuse \
  --cap-add SYS_ADMIN \
  --security-opt apparmor:unconfined \
  --volume=${GRIZOL_TMP_DIR}/config:/opt/grizol/config:ro \
  --volume=${GRIZOL_TMP_DIR}/staging_area:/opt/grizol/staging_area \
  --volume=${GRIZOL_TMP_DIR}/state:/opt/grizol/state \
  --volume=${GRIZOL_TMP_DIR}/fuse_mountpoint:/opt/grizol/fuse_mountpoint \
  --volume=${GRIZOL_TMP_DIR}/read_cache_dir:/opt/grizol/read_cache_dir \
  registry.gitlab.com/com.marcoseravalli/grizol:latest
