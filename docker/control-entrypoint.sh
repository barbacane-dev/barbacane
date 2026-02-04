#!/bin/bash
set -e

# Start nginx in background for UI
nginx -g 'daemon off;' &

# Start control plane API
exec /usr/local/bin/barbacane-control serve \
    --listen 0.0.0.0:9090 \
    --database-url "${DATABASE_URL}" \
    "$@"
