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
    status=$?
    if [ "$status" -ne 0 ]; then
        "$runtime" logs --tail 30 "$prefix-origin" >&2 || true
        "$runtime" logs --tail 30 "$prefix-renderer" >&2 || true
    fi
    "$runtime" rm -f "$prefix-renderer" "$prefix-origin" >/dev/null 2>&1 || true
    "$runtime" network rm "$prefix" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
mkdir "$work/origin"
cp "$fixtures/small.png" "$work/origin/"
"$runtime" network create "$prefix" >/dev/null
"$runtime" run -d --name "$prefix-origin" --network "$prefix" \
    -v "$work/origin:/fixtures:ro" -w /fixtures \
    "${IMGX_SMOKE_ORIGIN_IMAGE:-python:3-alpine@sha256:0f9552bb28711333da16915ec146f5e6bb44a7fdac9ced15dc17c2e0206346dd}" \
    python3 -m http.server 9000 >/dev/null
"$runtime" exec "$prefix-origin" python3 -c '
import time
import urllib.request

deadline = time.monotonic() + 10
while True:
    try:
        with urllib.request.urlopen("http://127.0.0.1:9000/small.png", timeout=1) as response:
            response.read()
        break
    except OSError:
        if time.monotonic() >= deadline:
            raise
        time.sleep(0.2)
'
"$runtime" run -d --name "$prefix-renderer" --network "$prefix" -p "127.0.0.1:$port:8080" \
    -e IMGX_ORIGIN_BASE_URL="http://$prefix-origin:9000" \
    -e IMGX_TRANSFORM_STATIC_LADDER="${IMGX_TRANSFORM_STATIC_LADDER:-false}" \
    -e IMGX_TRANSFORM_FAILURE=error "$image" >/dev/null

address=127.0.0.1:$port
# Docker may publish the port before imgx has bound the listener.
curl --noproxy 127.0.0.1 --fail --silent --show-error --max-time 3 \
    --retry 10 --retry-delay 1 --retry-all-errors \
    "http://$address/health" >/dev/null
for format in jpeg png webp gif avif json thumbhash; do
    case "$format" in
        json) media=application/json ;;
        thumbhash) media='text/plain; charset=utf-8' ;;
        *) media="image/$format" ;;
    esac
    actual=$(curl --noproxy 127.0.0.1 --fail --silent --show-error --output "$work/$format" --write-out '%{content_type}' \
        "http://$address/image/width=12,format=$format/small.png")
    test "$actual" = "$media"
    test -s "$work/$format"
done

# Render a WebP source through the width-only and card recipes.
cp "$work/webp" "$work/origin/master.webp"
for options in 'width=8,format=webp' 'width=24,height=12,fit=cover,format=jpeg'; do
    curl --fail --silent --show-error "http://$address/image/$options/master.webp" > "$work/card"
    test -s "$work/card"
done
