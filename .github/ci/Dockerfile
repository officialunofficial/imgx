FROM ubuntu:26.04

LABEL org.opencontainers.image.source="https://github.com/officialunofficial/imgx"

# Recommended packages stay on, as in the apt install the CI jobs ran before.
# libheif-plugin-aomenc is the AV1 encoder AVIF output needs. It is only a
# Recommends dependency of libheif-dev, so name it here.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        git \
        libheif-plugin-aomenc \
        libvips-dev \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*
