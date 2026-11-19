#!/usr/bin/env bash
# Starts a PostgreSQL that serves TLS, for the control plane's `sslmode` tests
# (crates/barbacane-control/src/db/pool.rs), and prints the two variables they
# read. The server certificate names localhost and comes from a CA made for the
# run. Needs docker and openssl.
#
# Run from repo root: ./scripts/postgres-tls.sh [PORT] >> "$GITHUB_ENV"
set -euo pipefail

port="${1:-5433}"
dir="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/postgres-tls"
mkdir -p "$dir"
cd "$dir"

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=Barbacane test CA" \
  -keyout ca.key -out ca.crt 2> /dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr 2> /dev/null
printf 'subjectAltName=DNS:localhost\n' > san.ext
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 \
  -extfile san.ext -out server.crt 2> /dev/null

# PostgreSQL reads its key only when the server user owns it, so the container
# copies the files in before it starts.
docker run -d --name postgres-tls -p "$port:5432" \
  -e POSTGRES_USER=barbacane -e POSTGRES_PASSWORD=barbacane -e POSTGRES_DB=barbacane \
  -v "$dir:/certs:ro" --entrypoint bash postgres:16 -c '
    install -o postgres -m 600 /certs/server.key /tmp/server.key &&
    install -o postgres -m 644 /certs/server.crt /tmp/server.crt &&
    exec docker-entrypoint.sh postgres -c ssl=on \
      -c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key' > /dev/null

for _ in $(seq 1 60); do
  docker exec postgres-tls pg_isready -U barbacane -h localhost > /dev/null 2>&1 && break
  sleep 1
done
docker exec postgres-tls pg_isready -U barbacane -h localhost >&2

echo "BARBACANE_TEST_POSTGRES_TLS_URL=postgres://barbacane:barbacane@localhost:$port/barbacane"
echo "BARBACANE_TEST_POSTGRES_TLS_CA=$dir/ca.crt"
