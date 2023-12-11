#!/usr/bin/env bash

pushd tests/util
wget https://github.com/syncthing/syncthing/releases/download/v1.27.0/syncthing-linux-amd64-v1.27.0.tar.gz
tar xvzf syncthing-linux-amd64-v1.27.0.tar.gz
cp syncthing-linux-amd64-v1.27.0/syncthing ../
rm syncthing-linux-amd64-v1.27.0.tar.gz
rm -rf syncthing-linux-amd64-v1.27.0

