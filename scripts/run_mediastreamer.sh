#!/usr/bin/env bash


run_navidome() {
  docker run --rm \
    --name navidrome \
    --user $(id -u):$(id -g) \
    -v ${PWD}/tests/util/fuse_mountpoint/:/music:ro \
    -v /tmp/data:/data \
    -p 4533:4533 \
    -e ND_LOGLEVEL=info \
    deluan/navidrome:latest
}

run_jellyfin() {
  docker pull jellyfin/jellyfin:latest  # or docker pull ghcr.io/jellyfin/jellyfin:latest
  mkdir -p /tmp/jellyfin/{config,cache}
  docker run --rm \
    -v /tmp/jellyfin/config:/config \
    -v /tmp/jellyfin/cache:/cache \
    -v ${PWD}/tests/util/fuse_mountpoint:/media:ro \
    -p 8096:8096 \
    jellyfin/jellyfin:latest
    # --net=host \
}

run_jellyfin
