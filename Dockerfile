# --- Build stage ---
FROM rust:alpine AS build

RUN apk add --no-cache vips-dev musl-dev pkgconfig cmake make g++

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo/ .cargo/
COPY crates/ crates/
COPY test/ test/

RUN cargo build --release -p imgx

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

COPY --from=build /app/target/release/imgx /usr/local/bin/imgx

EXPOSE 8080

ENTRYPOINT ["imgx"]
