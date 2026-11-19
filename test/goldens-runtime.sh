#!/bin/sh
# Compile the golden harness with the matching Alpine toolchain, then
# run it with the native libraries and server from the runtime image.
set -eu

runtime=${CONTAINER_RUNTIME:-docker}
toolchain=${1:?usage: goldens-runtime.sh TOOLCHAIN_IMAGE RUNTIME_IMAGE}
image=${2:?usage: goldens-runtime.sh TOOLCHAIN_IMAGE RUNTIME_IMAGE}
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "$root/target/runtime-goldens"

"$runtime" run --rm -i -v "$root:/src" -w /src "$toolchain" sh -es <<'BUILD'
    export CARGO_TARGET_DIR=/src/target/runtime-goldens CARGO_PROFILE_TEST_DEBUG=0
    cargo test --locked -p imgx --test ladder_golden --no-run --message-format=json > "$CARGO_TARGET_DIR/build.json"
    binary=$(jq -er "select(.reason == \"compiler-artifact\" and .target.name == \"ladder_golden\" and .executable) | .executable" "$CARGO_TARGET_DIR/build.json")
    cp "$binary" "$CARGO_TARGET_DIR/ladder-golden"
BUILD
"$runtime" run --rm -v "$root:/src:ro" -w /src \
    -e IMGX_PINNED_GOLDENS=1 \
    -e IMGX_GOLDEN_SERVER=/usr/local/bin/imgx \
    --entrypoint /src/target/runtime-goldens/ladder-golden \
    "$image" --nocapture --test-threads=1

"$runtime" run --rm -v "$root:/src:ro" -w /src \
    -e LIBHEIF_PLUGIN_PATH=/no-heif-plugins \
    --entrypoint /src/target/runtime-goldens/ladder-golden \
    "$image" --ignored --exact heic_without_decoder_is_unsupported
