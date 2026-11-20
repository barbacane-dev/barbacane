#!/usr/bin/env bash
# Soak the gateway, then overflow its plugin instance pool.
#
# Steady phase: two routes (a mock and a proxied one, each behind two
# middlewares) under load for MINUTES. Resident memory must level off, the
# pool must hold every instance, no instance may outlive its request, and
# every response must be a 2xx.
#
# Overflow phase: a pool of 32 instances under 256 connections. Every
# response must still be a 2xx, instances past the pool are counted, and
# none is alive once the load stops.
#
# Needs wrk, curl, the gateway binary, the mock, http-upstream, cors and
# correlation-id plugins built as plugins/<name>/<name>.wasm, and an upstream
# answering GET /mock on 127.0.0.1:18082. Writes its samples and a Markdown
# report to OUT; exits 1 when a check fails.
#
# Run from repo root: ./scripts/soak.sh BINARY MINUTES OUT
set -euo pipefail

bin=$(realpath "$1")
minutes=$2
out=$3
mkdir -p "$out"
out=$(realpath "$out")
repo=$(pwd)

# The steady phase's checks; the bounds are generous next to what a run
# measures, and catch a leak or a slot that is never given back.
warmup_seconds=60
max_peak_rss_mib=256
growth_ratio=1.05
growth_slack_mib=4

failures=0
fail() {
    echo "::error::$*"
    echo "- **failed:** $*" >> "$out/report.md"
    failures=$((failures + 1))
}

cat > "$out/barbacane.yaml" <<EOF
plugins:
  mock:
    path: $repo/plugins/mock/mock.wasm
  http-upstream:
    path: $repo/plugins/http-upstream/http-upstream.wasm
  cors:
    path: $repo/plugins/cors/cors.wasm
  correlation-id:
    path: $repo/plugins/correlation-id/correlation-id.wasm
EOF
cat > "$out/api.yaml" <<'EOF'
openapi: "3.1.0"
info:
  title: Soak
  version: "1.0.0"
x-barbacane-middlewares:
  - name: correlation-id
    config: {}
  - name: cors
    config:
      allowed_origins: ["https://example.com"]
paths:
  /mock:
    get:
      operationId: mock
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"ok","items":[1,2,3,4,5,6,7,8]}'
      responses:
        "200":
          description: OK
  /proxy:
    get:
      operationId: proxy
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "http://127.0.0.1:18082"
          path: "/mock"
      responses:
        "200":
          description: OK
EOF
"$bin" compile --spec "$out/api.yaml" --manifest "$out/barbacane.yaml" \
    --output "$out/api.bca" --allow-plaintext > /dev/null

pid=
serve() {
    BARBACANE_ALLOW_INTERNAL_EGRESS=1 "$bin" serve --artifact "$out/api.bca" \
        --listen 127.0.0.1:18080 --admin-bind 127.0.0.1:18081 --allow-unsigned \
        --allow-plaintext-upstream --plugin-instance-pool "$1" --log-level warn \
        > "$out/gateway-$1.log" 2>&1 &
    pid=$!
    for _ in $(seq 1 150); do
        curl -fsS -o /dev/null http://127.0.0.1:18080/mock && return 0
        sleep 0.2
    done
    cat "$out/gateway-$1.log" >&2
    exit 1
}
stop() {
    kill "$pid"
    wait "$pid" || true
}
metric() {
    curl -fsS http://127.0.0.1:18081/metrics | awk -v name="$1" '$1 == name {print $2}'
}
rss_mib() {
    awk '/^VmRSS/ {print int($2 / 1024)}' "/proc/$pid/status"
}
peak_mib() {
    awk '/^VmHWM/ {print int($2 / 1024)}' "/proc/$pid/status"
}
load() {
    # wrk CONNECTIONS DURATION ROUTE NAME
    wrk -t2 -c"$1" -d"$2" -H 'Origin: https://example.com' "http://127.0.0.1:18080/$3" > "$out/wrk-$4.txt"
}
# Non-2xx responses and socket errors in a wrk report.
errors() {
    awk '/Non-2xx/ {n += $NF} /Socket errors/ {gsub(",", ""); n += $4 + $6 + $8 + $10} END {print n + 0}' "$1"
}

echo "## Soak: $minutes minutes, then an overflowed pool" > "$out/report.md"

# --- Steady phase -----------------------------------------------------------
serve 1000
printf 'seconds\trss_mib\tinstances\n' > "$out/steady.tsv"
(
    start=$(date +%s)
    while kill -0 "$pid" 2> /dev/null; do
        printf '%s\t%s\t%s\n' "$(($(date +%s) - start))" "$(rss_mib)" "$(metric barbacane_wasm_instances)" \
            >> "$out/steady.tsv"
        sleep 10
    done
) &
sampler=$!
load 32 "${minutes}m" mock steady-mock &
mock_load=$!
load 32 "${minutes}m" proxy steady-proxy &
proxy_load=$!
wait "$mock_load" "$proxy_load"
kill "$sampler"
sleep 5

steady_alive=$(metric barbacane_wasm_instances)
steady_unpooled=$(metric barbacane_wasm_instances_unpooled_total)
steady_peak=$(peak_mib)
steady_rss=$(rss_mib)
stop

read -r first_half second_half < <(awk -F'\t' -v warmup="$warmup_seconds" '
    NR > 1 && $1 >= warmup { t[n] = $1; r[n] = $2; n++ }
    END {
        for (i = 0; i < n; i++) {
            if (i < n / 2) { if (r[i] > a) a = r[i] } else { if (r[i] > b) b = r[i] }
        }
        print a + 0, b + 0
    }' "$out/steady.tsv")

{
    echo
    echo "### Steady: 64 connections over /mock and /proxy, pool of 1000"
    echo
    for route in mock proxy; do
        echo "- /$route: $(awk '/Requests\/sec/ {print $2}' "$out/wrk-steady-$route.txt") req/s," \
            "$(errors "$out/wrk-steady-$route.txt") errors"
    done
    echo "- resident memory after the warm-up: highest $first_half MiB in the first half, $second_half MiB in the second"
    echo "- peak resident memory $steady_peak MiB; $steady_rss MiB 5 s after the load"
    echo "- instances alive 5 s after the load: $steady_alive; allocated past the pool: $steady_unpooled"
} >> "$out/report.md"

for route in mock proxy; do
    [ "$(errors "$out/wrk-steady-$route.txt")" -eq 0 ] || fail "/$route answered errors under steady load"
done
[ "$steady_alive" = 0 ] || fail "$steady_alive instances alive after the steady load"
[ "$steady_unpooled" = 0 ] || fail "$steady_unpooled instances past a pool the load fits"
[ "$steady_peak" -le "$max_peak_rss_mib" ] || fail "peak resident memory $steady_peak MiB, above $max_peak_rss_mib MiB"
awk -v a="$first_half" -v b="$second_half" -v ratio="$growth_ratio" -v slack="$growth_slack_mib" \
    'BEGIN { exit !(b <= a * ratio + slack) }' ||
    fail "resident memory kept growing: $first_half MiB, then $second_half MiB"

# --- Overflow phase ---------------------------------------------------------
serve 32
load 256 60s proxy overflow
sleep 5
overflow_alive=$(metric barbacane_wasm_instances)
overflow_unpooled=$(metric barbacane_wasm_instances_unpooled_total)
overflow_peak=$(peak_mib)
overflow_rss=$(rss_mib)
stop

{
    echo
    echo "### Overflow: 256 connections over /proxy, pool of 32"
    echo
    echo "- $(awk '/Requests\/sec/ {print $2}' "$out/wrk-overflow.txt") req/s, $(errors "$out/wrk-overflow.txt") errors"
    echo "- allocated past the pool: $overflow_unpooled; alive 5 s after the load: $overflow_alive"
    echo "- peak resident memory $overflow_peak MiB; $overflow_rss MiB 5 s after the load"
} >> "$out/report.md"

[ "$(errors "$out/wrk-overflow.txt")" -eq 0 ] || fail "requests failed past the pool"
[ "${overflow_unpooled%.*}" -gt 0 ] || fail "no instance went past a pool of 32 under 256 connections"
[ "$overflow_alive" = 0 ] || fail "$overflow_alive instances alive after the overflow load"

echo
cat "$out/report.md"
[ "$failures" -eq 0 ]
