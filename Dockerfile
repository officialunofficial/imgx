# --- Build stage ---
FROM rust:alpine AS build

RUN apk add --no-cache vips-dev musl-dev pkgconfig

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo/ .cargo/
COPY crates/ crates/
COPY test/ test/

RUN cargo build --release -p imgx

# --- Runtime stage ---
FROM alpine

RUN apk add --no-cache vips

COPY --from=build /app/target/release/imgx /usr/local/bin/imgx

EXPOSE 8080

ENTRYPOINT ["imgx"]
