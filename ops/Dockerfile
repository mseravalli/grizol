FROM rust:latest as builder
WORKDIR /usr/src/grizol
COPY . .
RUN apt-get update
RUN apt-get install -y protobuf-compiler
ENV DATABASE_URL="sqlite:tests/util/grizol.db"
RUN cargo build --release

FROM ubuntu:latest 

MAINTAINER Marco Seravalli <grizol@marcoseravalli.com>

RUN apt-get update
RUN apt-get install -y protobuf-compiler python3-protobuf

# Contains the grizl binaries.
RUN mkdir -p /opt/grizol/bin/
# Contains read only config.
RUN mkdir -p /opt/grizol/config/
# Contains the data that will be synchronized.
RUN mkdir -p /opt/grizol/data/
# Contains the scripts and other utils.
RUN mkdir -p /opt/grizol/ops/
# Contains grizol's state.
RUN mkdir -p /opt/grizol/state/

ENV RUST_LOG=info

EXPOSE 23456

WORKDIR "/opt/grizol"

CMD ["bash", "-c","/opt/grizol/bin/grizol --config <(ops/merge_grizol_config.py ops/default_config.textproto config/config.textproto)"]

COPY ./tests/util/rclone /usr/bin/rclone

COPY ./ops/docker_default_grizol_config.textproto ops/default_config.textproto
COPY ./ops/merge_grizol_config.py                 ops/merge_grizol_config.py

COPY ./src/proto/config.proto /opt/grizol/ops/config.proto
RUN protoc -I=ops --python_out=ops ops/config.proto

COPY --from=builder /usr/src/grizol/target/release/grizol /opt/grizol/bin/grizol
