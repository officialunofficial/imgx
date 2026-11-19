#!/bin/sh
# Exercise the runtime image with a local fixture origin.
set -eu

runtime=${CONTAINER_RUNTIME:-docker}
image=${1:?usage: smoke-runtime.sh IMAGE}
port=${IMGX_SMOKE_PORT:-18080}
fixtures=$(CDPATH= cd -- "$(dirname -- "$0")/fixtures/golden" && pwd)
prefix=imgx-smoke-$$
work=$(mktemp -d)

cleanup() {
    "$runtime" rm -f "$prefix-renderer" "$prefix-origin" >/dev/null 2>&1 || true
    "$runtime" network rm "$prefix" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
"$runtime" network create "$prefix" >/dev/null
"$runtime" run -d --name "$prefix-origin" --network "$prefix" \
    -v "$fixtures:/fixtures:ro" -w /fixtures \
    "${IMGX_SMOKE_ORIGIN_IMAGE:-python:3-alpine}" python3 -m http.server 9000 >/dev/null
"$runtime" run -d --name "$prefix-renderer" --network "$prefix" -p "127.0.0.1:$port:8080" \
    -e IMGX_ORIGIN_BASE_URL="http://$prefix-origin:9000" \
    -e IMGX_TRANSFORM_FAILURE=error "$image" >/dev/null

address=127.0.0.1:$port
curl --fail --silent --show-error --retry 10 --retry-delay 1 --retry-connrefused \
    "http://$address/health" >/dev/null
for format in jpeg png webp gif avif json thumbhash; do
    case "$format" in
        json) media=application/json ;;
        thumbhash) media='text/plain; charset=utf-8' ;;
        *) media="image/$format" ;;
    esac
    actual=$(curl --fail --silent --show-error --output "$work/$format" --write-out '%{content_type}' \
        "http://$address/image/width=12,format=$format/small.png")
    test "$actual" = "$media"
    test -s "$work/$format"
done
