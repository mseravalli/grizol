FROM ubuntu:latest 

MAINTAINER Marco Seravalli <grizol@marcoseravalli.com>

RUN mkdir -p /opt/grizol/bin/
RUN mkdir -p /opt/grizol/config/
RUN mkdir -p /opt/grizol/data/
RUN mkdir -p /opt/grizol/state/

ENV RUST_LOG=info

EXPOSE 23456

WORKDIR "/opt/grizol"

CMD ["/opt/grizol/bin/grizol", "--config", "config/config.textproto"]

COPY ./tests/util/rclone /usr/bin/rclone

COPY ./target/release/grizol /opt/grizol/bin/grizol
