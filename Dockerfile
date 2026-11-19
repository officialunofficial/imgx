# CI and release share the signed Alpine packages and the native ABI.
FROM alpine:3.24@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf AS codecs

# vips-heif loads libheif's codec plugins at runtime. Keep the AOM
# encoder for imgx's existing AVIF output.
RUN apk add --no-cache \
    vips=8.18.2-r0 vips-heif=8.18.2-r0 libheif-aom \
    libjpeg-turbo=3.1.3-r0 libpng=1.6.59-r0 lcms2=2.19-r0 \
    libwebp=1.6.0-r0 libwebpdemux=1.6.0-r0 libwebpmux=1.6.0-r0

FROM rust:1.98.1-alpine@sha256:7cc1c22d77d9432f7fe012a70e6d3e555af54c2a6832700ed7d553f1769ae89f AS rust

FROM codecs AS build-deps

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo
RUN apk add --no-cache vips-dev=8.18.2-r0 musl-dev pkgconfig cmake make g++

FROM build-deps AS ci

LABEL org.opencontainers.image.source="https://github.com/officialunofficial/imgx"
RUN apk add --no-cache bash curl git github-cli jq tar xz zstd \
    && rustup component add rustfmt clippy llvm-tools-preview

FROM build-deps AS build

ARG TARGETARCH

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo/ .cargo/
COPY crates/ crates/
COPY test/ test/

# Cache mounts persist independently of layer invalidation (unlike a
# plain COPY+RUN layer, which invalidates on almost every commit since
# crates/ changes constantly) -- id is scoped per architecture since
# docker.yml builds amd64/arm64 concurrently and the mounts would
# otherwise corrupt each other. The compiled binary is copied out to a
# non-cache-mounted path before the mount unmounts: anything left inside
# a cache mount is invisible to the later `COPY --from=build`.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-${TARGETARCH} \
    --mount=type=cache,target=/app/target,id=cargo-target-${TARGETARCH} \
    cargo build --release -p imgx && \
    cp target/release/imgx /usr/local/bin/imgx

FROM codecs AS runtime

COPY --from=build /usr/local/bin/imgx /usr/local/bin/imgx

EXPOSE 8080

ENTRYPOINT ["imgx"]
