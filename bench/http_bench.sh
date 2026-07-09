#!/bin/bash
# HTTP benchmark for imgx
#
# Prerequisites:
#   1. imgx running with an HTTP origin that has test images
#   2. ab (ApacheBench) installed
#
# Usage:
#   ./bench/http_bench.sh [host:port] [image_path]
#
# Example:
#   ./bench/http_bench.sh localhost:8080 photos/test.jpg

set -euo pipefail

HOST="${1:-localhost:8080}"
IMAGE="${2:-test.jpg}"
REQUESTS=500
# imgx runs on axum/tokio (no fixed thread-per-connection cap, unlike the
# prior Zig implementation), so a higher default concurrency is meaningful.
CONCURRENCY=4

echo ""
echo "imgx HTTP benchmark"
echo "===================="
echo "Target: http://${HOST}"
echo "Image:  ${IMAGE}"
echo "Reqs:   ${REQUESTS} (concurrency ${CONCURRENCY})"
echo ""

# Check server is up
if ! curl -sf "http://${HOST}/health" > /dev/null 2>&1; then
    echo "Error: server not responding at http://${HOST}/health"
    exit 1
fi

run_bench() {
    local label="$1"
    local path="$2"

    printf "%-45s " "${label}"

    # Run ab, extract key metrics
    local output
    output=$(ab -n "${REQUESTS}" -c "${CONCURRENCY}" -q "http://${HOST}/${path}" 2>&1) || {
        echo "FAILED"
        return
    }

    local rps=$(echo "$output" | grep "Requests per second" | awk '{print $4}')
    local mean=$(echo "$output" | grep "Time per request.*\(mean\)" | head -1 | awk '{print $4}')
    local p50=$(echo "$output" | grep "50%" | awk '{print $2}')
    local p99=$(echo "$output" | grep "99%" | awk '{print $2}')
    local failed=$(echo "$output" | grep "Failed requests" | awk '{print $3}')

    printf "%8s req/s  %8s ms avg  p50=%s  p99=%s" "${rps}" "${mean}" "${p50}" "${p99}"
    if [ "${failed:-0}" != "0" ]; then
        printf "  (%s failed)" "${failed}"
    fi
    echo ""
}

echo "Endpoint                                      Throughput      Latency"
echo "────────────────────────────────────────────  ─────────────  ──────────────────────"

# Health/ready (baseline, no image processing)
run_bench "/health (baseline)" "health"
run_bench "/ready (baseline)" "ready"
run_bench "/metrics" "metrics"

echo ""

# Image requests — first hit is cache miss, subsequent are cache hits
# Prime the cache first
curl -sf "http://${HOST}/${IMAGE}" > /dev/null 2>&1 || true
curl -sf "http://${HOST}/${IMAGE}/w=800,h=600,f=jpeg,q=80" > /dev/null 2>&1 || true
curl -sf "http://${HOST}/${IMAGE}/w=800,h=600,f=webp,q=80" > /dev/null 2>&1 || true
curl -sf "http://${HOST}/${IMAGE}/w=400,h=300,f=webp,q=80" > /dev/null 2>&1 || true
curl -sf "http://${HOST}/${IMAGE}/w=200,h=150,f=webp,q=80" > /dev/null 2>&1 || true

echo "Cache hits (primed):"
run_bench "original (no transform)" "${IMAGE}"
run_bench "800x600 JPEG q80" "${IMAGE}/w=800,h=600,f=jpeg,q=80"
run_bench "800x600 WebP q80" "${IMAGE}/w=800,h=600,f=webp,q=80"
run_bench "400x300 WebP q80" "${IMAGE}/w=400,h=300,f=webp,q=80"
run_bench "200x150 WebP q80 (thumbnail)" "${IMAGE}/w=200,h=150,f=webp,q=80"

echo ""
echo "Done."
