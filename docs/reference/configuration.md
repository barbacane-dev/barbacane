# Configuration & environment variables

Barbacane is configured through CLI flags (see the [CLI reference](cli.md)) and a
small set of environment variables. The security-related variables below were
introduced by the security hardening pass and several change default behavior in
breaking but deliberate ways — Barbacane fails closed rather than running
insecurely.

## Security environment variables

| Variable | Component | Default | Effect |
|----------|-----------|---------|--------|
| `BARBACANE_CONTROL_ADMIN_TOKEN` | Control plane | _unset_ | **Required.** Bearer token that must accompany every control-plane API request (except `GET /health` and the data-plane WebSocket). The server refuses to start if unset. |
| `BARBACANE_CONTROL_ALLOWED_ORIGINS` | Control plane | _unset (no cross-origin)_ | Comma-separated CORS allowlist of browser origins permitted to call the API, e.g. `https://ui.example.com`. |
| `BARBACANE_TRUSTED_PUBKEY` | Data plane | _unset_ | Hex-encoded Ed25519 public key. When set, the data plane requires every loaded `.bca` artifact to carry a valid signature produced by the matching private key; load fails otherwise. When unset, the artifact's content hashes are still verified, but signature checking is skipped (a startup warning is logged). |
| `BARBACANE_SIGNING_KEY` | Compiler | _unset_ | Path to a PKCS#8 Ed25519 private key. When set, `barbacane compile` signs the artifact's content hash. When unset, the artifact is built unsigned. |
| `BARBACANE_SECRETS_DIR` | Data plane | _unset_ | Base directory that `file://` secret references are confined to. **Required to use `file://` secrets** — references are rejected when it is unset, and any path resolving outside this directory (after symlink/`..` resolution) is rejected. |
| `BARBACANE_ALLOW_INTERNAL_EGRESS` | Data plane | `false` | Set to `1`/`true` to disable the plugin SSRF guard and allow plugin egress (HTTP calls, Kafka/NATS broker connections, **and** WebSocket upstreams) to internal/loopback/link-local/cloud-metadata addresses. The HTTP guard also pins the vetted IP at connect time (DNS-rebinding safe). Leave off unless you have legitimate internal upstreams or brokers. |
| `BARBACANE_MAX_UPSTREAM_RESPONSE_BYTES` | Data plane | `16777216` (16 MiB) | Maximum size of an upstream response body that the buffered plugin HTTP-call path will read into host memory. Bodies larger than this are rejected, bounding host memory against a hostile or compromised upstream. Streaming dispatchers are unaffected. |
| `BARBACANE_MAX_CONNECTIONS` | Data plane | `10000` | Maximum number of concurrently served ingress connections. Beyond this, new connections are dropped (load shed) rather than letting file descriptors and tasks grow without bound under a connection flood. |

## Breaking-by-design defaults

These changes are intentional secure defaults. Adopt them as follows:

1. **The control plane will not start without `BARBACANE_CONTROL_ADMIN_TOKEN`.**
   Generate a strong random token and pass it to every client as
   `Authorization: Bearer <token>`. Previously the API was unauthenticated.

2. **`file://` secrets require `BARBACANE_SECRETS_DIR`.** If you reference
   secrets like `file:///run/secrets/api-key`, set
   `BARBACANE_SECRETS_DIR=/run/secrets`. `env://` references are unaffected.

3. **MCP clients must initialize a session.** Non-`initialize` MCP requests
   (`tools/list`, `tools/call`, …) without a valid `Mcp-Session-Id` are now
   rejected; call `initialize` first and reuse the returned session id.

4. **Plugin egress to internal addresses is blocked by default.** This covers
   plugin HTTP calls, Kafka/NATS broker connections, and WebSocket upstreams. If
   a plugin legitimately reaches an internal upstream or broker, set
   `BARBACANE_ALLOW_INTERNAL_EGRESS=1` (or prefer an explicit allowlist when one
   is available).

5. **Plugins may only use their declared capabilities.** A plugin whose WASM
   imports a host function outside the capabilities declared in its
   `plugin.toml` fails to load. Official plugins already declare the correct
   capabilities; custom plugins must list theirs under
   `[capabilities] host_functions = [...]`.

## Admin endpoints (loopback by default)

The data plane serves `/health`, `/metrics`, and `/provenance` on a dedicated
admin port (`--admin-bind`). These endpoints are **unauthenticated** so metrics
scrapers can reach them; `/provenance` and `/metrics` expose build and
operational metadata. Keep the admin port bound to loopback (the default) or
behind a trusted network boundary. Binding it to a non-loopback address (e.g.
`--admin-bind 0.0.0.0:...`) logs a startup warning because it exposes that
metadata off-host.

## Artifact signing quickstart

```bash
# 1. Generate a dev keypair (PKCS#8 Ed25519). Any tool that emits PKCS#8 works;
#    keep the private key secret and distribute only the public key.

# 2. Sign at compile time:
BARBACANE_SIGNING_KEY=/path/to/ed25519.pk8 \
  barbacane compile -m barbacane.yaml -o api.bca

# 3. Require verification on the data plane (pin the public key):
BARBACANE_TRUSTED_PUBKEY=<hex-public-key> \
  barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

The signature covers the artifact's content hash, which binds every spec, route,
and plugin WASM checksum, so any tampering with the artifact fails verification
on load.
