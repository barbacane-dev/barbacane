# Configuration & environment variables

Barbacane is configured through CLI flags (see the [CLI reference](cli.md)) and a
small set of environment variables. Barbacane fails closed rather than running
insecurely: where a variable is required, the component refuses to start
without it.

## Security environment variables

| Variable | Component | Default | Effect |
|----------|-----------|---------|--------|
| `BARBACANE_CONTROL_ADMIN_TOKEN` | Control plane | _unset_ | **Required.** Bearer token that must accompany every control-plane API request (except `GET /health` and the data-plane WebSocket). The server refuses to start if unset. |
| `BARBACANE_CONTROL_ALLOWED_ORIGINS` | Control plane | _unset (no cross-origin)_ | Comma-separated CORS allowlist of browser origins permitted to call the API, e.g. `https://ui.example.com`. |
| `BARBACANE_TRUSTED_PUBKEY` | Data plane | _unset_ | **Required by `barbacane serve`**, unless `--dev` or `--allow-unsigned` is passed. The hex-encoded Ed25519 public key (64 characters, as `barbacane key public` prints it) whose private key signs your artifacts. Every artifact the gateway loads, hot reloads included, must carry a valid signature by it; an unsigned or differently signed artifact is refused, whatever the flags. Without a pinned key, `--dev` and `--allow-unsigned` load artifacts unsigned, and each load logs a warning; content hashes are checked either way. |
| `BARBACANE_SIGNING_KEY` | Compiler, control plane | _unset_ | Path to a PKCS#8 Ed25519 private key, DER or PEM, from `barbacane key generate` or `openssl genpkey -algorithm ed25519`. When set, `barbacane compile` and the control plane's compilations sign the artifact's content hash. When unset or empty, artifacts are built unsigned. |
| `BARBACANE_SECRETS_DIR` | Data plane | _unset_ | Base directory that `file://` secret references are confined to. **Required to use `file://` secrets** — references are rejected when it is unset, and any path resolving outside this directory (after symlink/`..` resolution) is rejected. |
| `BARBACANE_ALLOW_INTERNAL_EGRESS` | Data plane | `false` | Set to `1`/`true` to disable the plugin SSRF guard and allow plugin egress (HTTP calls, Kafka/NATS broker connections, LDAP directory connections **and** WebSocket upstreams) to internal/loopback/link-local/cloud-metadata addresses. Guarded clients connect to the address vetted by the guard rather than resolving again (DNS-rebinding safe), except a NATS `tls://` broker, which keeps its hostname so the TLS handshake carries SNI. Leave off unless you have legitimate internal upstreams or brokers. |
| `BARBACANE_MAX_UPSTREAM_RESPONSE_BYTES` | Data plane | `16777216` (16 MiB) | Maximum size of an upstream response body that the buffered plugin HTTP-call path will read into host memory. Bodies larger than this are rejected, bounding host memory against a hostile or compromised upstream. Streaming dispatchers are unaffected. |
| `BARBACANE_MAX_CONNECTIONS` | Data plane | `10000` | Maximum number of concurrently served ingress connections. Beyond this, new connections are dropped (load shed) rather than letting file descriptors and tasks grow without bound under a connection flood. |
| `BARBACANE_CONTROL_API_KEY` | Data plane | _unset_ | The API key a data plane presents to the control plane (`barbacane serve --api-key`), created with the project's data plane API keys. Required with `--control-plane`. |
| `BARBACANE_CONTROL_URL` | CLI | `http://localhost:9090` | Base URL of the control-plane API for `barbacane control` (`--url`). |

## Secure defaults

These defaults are deliberate. Adopt them as follows:

1. **The control plane will not start without `BARBACANE_CONTROL_ADMIN_TOKEN`.**
   Generate a strong random token and pass it to every client as
   `Authorization: Bearer <token>`.

2. **`file://` secrets require `BARBACANE_SECRETS_DIR`.** If you reference
   secrets like `file:///run/secrets/api-key`, set
   `BARBACANE_SECRETS_DIR=/run/secrets`. `env://` references are unaffected.

3. **Plugin egress to internal addresses is blocked by default.** This covers
   plugin HTTP calls, Kafka/NATS broker connections, and WebSocket upstreams. If
   a plugin legitimately reaches an internal upstream or broker, set
   `BARBACANE_ALLOW_INTERNAL_EGRESS=1` (or prefer an explicit allowlist when one
   is available).

4. **The data plane serves only signed artifacts.** `barbacane serve` refuses
   to start until `BARBACANE_TRUSTED_PUBKEY` pins the key your artifacts are
   signed with. Generate one with `barbacane key generate`, compile with
   `BARBACANE_SIGNING_KEY` set to it, and pin the public key it prints (see the
   quickstart below). `barbacane dev` and `serve --dev` are unaffected;
   `serve --allow-unsigned` loads unsigned artifacts outside development, with
   a warning at every load.

5. **Plugins may only use their declared capabilities.** A plugin whose WASM
   imports a host function outside the capabilities declared in its
   `plugin.toml` fails to load, whether the artifact was compiled with
   `barbacane compile` or by the control plane. Official plugins already declare
   the correct capabilities; custom plugins must list theirs under
   `[capabilities] host_functions = [...]`.

## Request headers reaching your upstream

A request carries only the headers its operation admits. Everything else is
dropped at ingress, before any plugin or the dispatcher sees it, so the spec
describes what an upstream receives and not merely what a client may send.

An operation admits:

| Source | Headers |
|---|---|
| Baseline, on every operation | framing and negotiation (`host`, `content-type`, `content-length`, `content-encoding`, `transfer-encoding`, `accept`, `accept-encoding`, `accept-language`, `accept-charset`, `user-agent`, `range`, the `if-*` conditionals, `cache-control`, `pragma`, `expect`), CORS (`origin`, `access-control-request-method`, `access-control-request-headers`), browser metadata a browser sends unasked (`referer`, the `sec-fetch-*` set, the `sec-ch-ua*` client hints, `dnt`, `sec-gpc`, `priority`), tracing (`traceparent`, `tracestate`, `x-request-id`), the WebSocket handshake (`upgrade`, `connection`, `sec-websocket-*`), and the proxy chain (`x-forwarded-for`, `x-forwarded-proto`, `x-forwarded-host`, `x-real-ip`, `forwarded`) |
| Declared parameters | every `in: header` parameter, and `cookie` when the operation declares an `in: cookie` parameter |
| Security schemes | the credential header the operation's `security` requirement names: an `apiKey` scheme's own name, or `authorization` for `http`, `oauth2` and `openIdConnect`. An AsyncAPI operation takes its requirement from the servers its channel is reached through, which is where AsyncAPI declares one |
| Plugins in the chain | the headers a plugin's own configuration names, such as `apikey-auth`'s `header_name`, a `rate-limit` `header:` partition, or `cache`'s `vary` list |

`authorization` is deliberately not in the baseline. It travels because the
operation says it is authenticated, and for no other reason. An operation that
runs an authentication middleware must therefore declare a `security`
requirement naming a scheme defined under `components.securitySchemes`.
Compiling one that does not is an error (`E1057`), since the credential would
be dropped and the middleware would reject every request. The scheme must also
be one the middleware reads: pairing `jwt-auth` with an `apiKey` scheme is an
error too (`E1032`). The other way round, an operation requiring a credential
with no authentication middleware in its chain compiles with a warning
(`E1033`), since the upstream may be the one checking it.

The `x-auth-*` namespace is never accepted from a client. It carries what an
auth plugin tells `acl` and the upstream about the caller, and declaring one in
a spec is a compile error (`E1056`).

The WAF is not filtered this way. It inspects what the client actually sent,
which is the point of it.

### Finding what to declare

`barbacane_request_headers_dropped_total` counts what was dropped. It carries no
header name, since a name is attacker-controlled and would let a caller grow the
series set without bound. To see the names, run with `--dev`, which logs each
dropped header at warn level on the request that carried it; without `--dev` the
same goes to debug.

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
# 1. Generate a signing key. It is written owner-readable only, never over an
#    existing file; the public key is printed.
barbacane key generate --out signing.pk8

# 2. Sign at compile time:
BARBACANE_SIGNING_KEY=signing.pk8 \
  barbacane compile -m barbacane.yaml -o api.bca

# 3. Pin the public key on the data plane:
BARBACANE_TRUSTED_PUBKEY=$(barbacane key public --key signing.pk8) \
  barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

Keep the private key where your builds run (a CI secret, or the control
plane's secret store) and give data planes only the public key. Each data plane
pins the key itself rather than learning it from the control plane, so whoever
can send it artifacts cannot also choose the key that vouches for them.

To rotate the key, generate a new one, sign new artifacts with it, and update
`BARBACANE_TRUSTED_PUBKEY` on each data plane as you deploy them: a data plane
pins one key at a time, so restart it with the new key and the newly signed
artifact together.

When a data plane refuses an artifact, the error says which case it is: no key
pinned (`no trusted signing key`), an unsigned artifact (`the artifact is
unsigned, but a trusted key is pinned`), or a signature by another key (`the
artifact is not signed by the key BARBACANE_TRUSTED_PUBKEY pins`).

The signature covers the artifact's content hash (`artifact_hash`), which binds
every spec, route, and plugin WASM checksum **plus the capability-enforcement
surface** (`capabilities_enforced`, each plugin's declared `host_functions` /
`body_access`), so any tampering with the artifact (including
attempts to weaken the sandbox) fails verification on load.

## Verifying release container images

Release images are signed keylessly with [Sigstore cosign](https://docs.sigstore.dev/)
using the GitHub Actions OIDC identity (no long-lived signing keys), and each image
carries a CycloneDX SBOM as a cosign attestation. Verify a published image before
running it:

```bash
# Signature
cosign verify \
  --certificate-identity-regexp 'https://github.com/barbacane-dev/barbacane/\.github/workflows/sign-and-sbom\.yml@refs/(tags/v|heads/main)' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/barbacane-dev/barbacane:1.0.0

# SBOM attestation (CycloneDX)
cosign verify-attestation --type cyclonedx \
  --certificate-identity-regexp 'https://github.com/barbacane-dev/barbacane/\.github/workflows/sign-and-sbom\.yml@refs/(tags/v|heads/main)' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/barbacane-dev/barbacane:1.0.0
```

The same commands work against Docker Hub (`docker.io/barbacane/barbacane:1.0.0`)
and the other images (`barbacane-control`, `barbacane-standalone`). The identity
regexp names the one workflow that signs, `sign-and-sbom.yml`, run from a
release tag or, for a manual re-signing, from `main`; no other workflow's
signature verifies. A successful verification
confirms the signature was checked against the Rekor transparency log and a
trusted Fulcio certificate authority.

## Verifying release binaries and plugins

Every file attached to a GitHub release (the binaries, the plugin `.wasm` files,
the checksum files and the image SBOMs) carries a build provenance attestation
signed by the release workflow run that produced it. Verify a download before
using it:

```bash
gh attestation verify barbacane-x86_64-unknown-linux-gnu.tar.gz \
  --repo barbacane-dev/barbacane
```

The command checks the Sigstore signature and that the attested subject digest
matches the file; it fails on a file that was altered or did not come from a
release run of this repository.
