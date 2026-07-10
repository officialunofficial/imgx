# --- Build stage ---
FROM rust:alpine AS build

ARG TARGETARCH

RUN apk add --no-cache vips-dev musl-dev pkgconfig cmake make g++

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

# --- Runtime stage ---
FROM alpine

# vips-heif is a separate plugin package -- plain `vips` has no AVIF/HEIF
# support, which silently breaks negotiation for any client whose Accept
# header allows avif (avif is first in INV-7's priority order, and nearly
# every browser either sends it explicitly or via `*/*`). libheif itself
# loads codecs as runtime plugins too -- libheif-aom provides the actual
# AV1 encoder AVIF output needs (vips-heif alone only gets you HEIF
# container support with no usable encoder).
RUN apk add --no-cache vips vips-heif libheif-aom

COPY --from=build /usr/local/bin/imgx /usr/local/bin/imgx

EXPOSE 8080

ENTRYPOINT ["imgx"]
