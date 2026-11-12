# Migrating from 0.12 to 1.0

This guide takes a 0.12 deployment to 1.0.0-rc.1 and later. Each section names what changes, what to do, and how to tell it is done. The [stability policy](../reference/stability.md) applies from 1.0.0 on; 0.12 receives security fixes until 1.0.0 is released.

Do the steps in this order: compile artifacts with the new compiler, then upgrade the control plane, then the data planes. A 1.0 data plane refuses a 0.12 artifact, and a 0.12 data plane cannot load a 1.0 one.

## Before you start

- [ ] Generate a signing key and decide where it lives (`BARBACANE_SIGNING_KEY` on every compiler and on the control plane).
- [ ] List every `barbacane.yaml` and spec you compile, and every custom plugin you build.
- [ ] List what parses the gateway's responses: dashboards on `urn:barbacane:error:*` types, probes on `/__barbacane/health`, alerts on plugin metrics.
- [ ] If you used the AI gateway plugins, the MCP endpoint or the control plane's web interface, read [Removed features](#removed-features) first.

## Data plane

### Artifacts must be signed, and the data plane must know the key

`barbacane serve` refuses to start unless `BARBACANE_TRUSTED_PUBKEY` names the Ed25519 public key your artifacts are signed with, and then loads only artifacts carrying that signature, at startup and on every hot reload.

```bash
barbacane key generate --out signing.pk8          # once; keep it out of the repository
barbacane key public --key signing.pk8            # the value for BARBACANE_TRUSTED_PUBKEY
BARBACANE_SIGNING_KEY=signing.pk8 barbacane compile -s api.yaml -m barbacane.yaml -o api.bca
BARBACANE_TRUSTED_PUBKEY=<public key> barbacane serve --artifact api.bca
```

On the control plane, set `BARBACANE_SIGNING_KEY` so compilations are signed. For a development machine, `--dev` or `--allow-unsigned` keeps serving unsigned artifacts, with a warning at every load.

### Recompile every artifact

The artifact format is 8. A 1.0 data plane refuses an artifact built by an earlier compiler, with a message saying to recompile. From 1.0.0 on, a data plane of the same major version loads every format from 8 up to the one it writes, so this is the last recompilation an upgrade requires.

An artifact compiled without a manifest, with `barbacane_compiler::compile` from your own code, carries no plugin capabilities and is refused: compile with `--manifest barbacane.yaml` or `compile_with_manifest`.

### What the gateway discloses changed

- The `Server` header is `barbacane`, with no version.
- `/__barbacane/health` answers `{"status":"healthy"}` and nothing else. The artifact version, the compiler version and the route count are on the admin API's `/health` (`--admin-bind`, loopback by default). A probe that read those fields from the public endpoint reads them there.
- `--no-introspection` makes every `/__barbacane/specs*` path answer 404, if you do not want the embedded specs served.

### Requests the gateway treats differently

- `HEAD` on a route declared for `GET` is answered like the `GET`, with no body, where it was 405.
- A client's `x-request-id` is kept only when it is at most 128 bytes of visible ASCII, and a `traceparent` yields the trace id only when well-formed; otherwise the gateway generates ids. Clients that sent longer ids see generated ones in `x-request-id`.
- Only a request shaped as a WebSocket handshake (a `GET` with `Upgrade: websocket`, `Connection: upgrade` and `Sec-WebSocket-Key`) skips body handling. Any other request carrying an `Upgrade` header has its body read, validated and inspected like every request.
- The request body must arrive within `--keepalive-timeout` seconds (60 by default), the deadline its headers already had; a body still incomplete at the deadline is answered `408` and the connection closed.
- An IPv4 client of a dual-stack listener is reported as its IPv4 address (`10.0.0.1`, not `::ffff:10.0.0.1`) to plugins, the WAF and logs. In `ip-restriction`, a rule like `::/0` no longer covers IPv4 clients; write `0.0.0.0/0`, and write `10.0.0.0/8` for what was `::ffff:10.0.0.0/104`.

### Shutdown and reload

- After `SIGTERM` the gateway accepts no new connection while requests in flight finish, and both health endpoints answer `503` `{"status":"draining"}` meanwhile. A probe that treated a 503 during shutdown as a failure should treat it as "going away".
- In connected mode, the artifact the control plane deployed is kept as `current.bca` next to the `--artifact` file, and a restart loads it in place of `--artifact`. Nothing to do; if the data plane's artifact directory is read-only, deployments fail as they did before, and the start-up log says which file was loaded.
- A hot reload is observed by the next request on every connection. Clients with keep-alive connections no longer need to reconnect to see a deployment.

### Connecting to the control plane

- The data plane reads its API key from `BARBACANE_CONTROL_API_KEY` (or `--api-key`). `BARBACANE_API_KEY` is not read.
- A `ws://` link to a control plane on a host other than loopback is refused at start, since the link carries the API key: use `wss://` behind TLS, or pass `--allow-plaintext-control-plane` on a network you trust.
- The data plane downloads deployed artifacts with its own API key. Nothing to configure; a 0.12 data plane could not download them at all.

## Specs and compilation

### `barbacane.yaml` refuses unknown keys

A key the format does not have, at the top level or in a plugin entry, fails compilation with an error naming it. A misspelled key (`spec:` for `specs:`) that was ignored now stops the build: fix or remove it. A plugin entry has `path` or `url`, not both, and `sha256` only with `url`.

### Middleware entries match by name

An operation's middleware entry matches the global one by plugin name, without the `@version` suffix. A spec that named `rate-limit@0.1.0` globally and `rate-limit` on an operation ran both; the operation entry now replaces the global one. Check operations that meant to add a second instance of a plugin already in the global chain.

`enabled: false` on an operation's entry drops that one global middleware and keeps the rest; `x-barbacane-middlewares: []` keeps dropping them all.

### Schemas the gateway cannot build are errors

No schema makes the compiler, the data plane or the control plane fetch a URL or read a file. A request schema the gateway cannot build (another `$schema` dialect, a reference outside the spec, an invalid `pattern`) fails compilation with `E1059` where it was accepted and failed at runtime. Keywords beside a `$ref` apply in OpenAPI 3.1 and later, as JSON Schema 2020-12 defines.

### YAML is read by a different parser

Specs are read by serde-saphyr, configured to read documents as libyaml did. The constructs a spec can meet that read differently, and what to do:

| Construct | 0.12 | 1.0 | What to do |
|---|---|---|---|
| a file starting with a byte order mark | refused when the top-level mapping has more than one key | read as without the mark | nothing |
| an alias written after its anchor name is reused (`&string` twice, `*string` later) | could resolve to another anchor's node | the last node with that name | check specs that reuse anchor names |
| a block scalar (`|`, `>`) ending the file without a final line break | no trailing line break | one trailing line break | end the file with a line break |
| keys spelled differently but equal as integers (`10` and `0xA`) | two keys | one key, the last kept | spell the keys as strings |
| a plain `null`, `~` or empty mapping key | read as its text | refused | quote the key: `"null"` |
| a quoted scalar continued on lines not indented past its node | read | refused | indent the continuation lines |
| `0X1F`, `0O17`, `0B101` | strings | integers | quote them |

The full list of twenty-six classes is in `barbacane_compiler::yaml::differential`.

### Resource limits and `--fetch-plugins`

- A plugin's `[limits]` in `plugin.toml`, and the operator's `limits` on the plugin entry of `barbacane.yaml` or on a project's plugin in the control plane, are checked at compile time: a value outside the ceilings is `E1044`.
- `barbacane init --fetch-plugins` downloads the plugins of the CLI's own version, so a release candidate gets its own plugins.

## Custom plugins

Rebuild every plugin against `barbacane-plugin-sdk` 1.0 and check these.

- **Imports.** A plugin may import the `barbacane` host functions and seven WASI functions (`random_get`, `clock_time_get`, `fd_write`, `sched_yield`, `environ_get`, `environ_sizes_get`, `proc_exit`). Anything else fails compilation with `E1043`, which names the import. A plugin built with `wasm32-wasip1` and a standard library that imports more needs the `wasm32-unknown-unknown` target.
- **`HttpRequest`** has `tls`, `endpoints` and `retries` fields and implements `Default`: a struct literal adds `..Default::default()`.
- **`init` runs once** per plugin and configuration, and each request runs in a fresh instance started from the memory `init` left. Nothing one request writes is seen by the next. State that must carry across requests goes through the host (`context`, `cache`, `rate_limit`). The [plugin guide](../contributing/plugins.md#instances-and-state) states the contract.
- **`body_access`** must be declared by a middleware that reads the request or response body; without it the host hands it no body.
- **A failure in `on_response`** stops the chain and answers 500, as a failure in `on_request` does. A plugin that let `on_response` fail silently now fails the request.
- **`host_http_stream`** forwards the upstream body to the client and no longer retains it: `host_http_read_result` returns the status and headers only. A plugin that read the streamed body afterwards (for token counting, say) uses `host_http_call` instead, within `max_response_bytes`.
- **Timeouts.** `timeout_ms` on an outbound call is cut to 60 seconds and a WebSocket `connect_timeout_ms` to 30.
- **`host_log`** messages are cut at 4 KiB and their control characters replaced.
- **Metrics** emitted through the `telemetry` capability are their own Prometheus families, `barbacane_plugin_<plugin>_<metric>`, with the plugin's labels as labels (see [Observability](#observability)).

### Official plugins

- `jwt-auth` refuses `public_key_pem` and `jwks_url`, which it never used. Supply `public_key_jwk`, or use `oidc-auth` for JWKS. ES512 is not accepted (the host verifies ES256 and ES384).
- `cache`, `opa-authz`, `http-log` and `observability` now receive bodies, so a cache hit carries the body, `include_body` works, and body sizes are reported.
- `ldap-auth` refuses a user in none of the `required_groups` with `urn:barbacane:error:authorization-failed` (see [Observability](#observability)).

## Control plane

### Image, port and credentials

- The `barbacane-control` image runs the API alone, as a non-root user, on port 9090. Port 80 and the `/api/` prefix are gone: clients call `http://control:9090/...`.
- `BARBACANE_CONTROL_ADMIN_TOKEN` is required and must be at least 32 bytes (`openssl rand -hex 32`). To rotate it, start with the new token and the old one in `BARBACANE_CONTROL_ADMIN_TOKEN_PREVIOUS`, move the clients, then restart without the previous one.
- `GET /openapi` and `GET /docs` need no token; everything else but `/health`, `/ready` and the data-plane WebSocket does.
- The web interface is gone; `barbacane control <resource> <command>` drives the API from the shell, with one command per operation, `push` and `deploy` for the common flow, and `--output json`.

### API changes a client sees

- The OpenAPI document describes every operation and is valid OpenAPI 3.1: a nullable field's type is `[T, 'null']` in place of `nullable: true`. Regenerate clients from `/openapi`.
- Every protected operation documents its `401`.
- `GET /projects/{id}/operations` answers 404 for a project that does not exist.
- A plugin registration refuses a name outside lower-case letters, digits and `-`, a version that is not semantic, or a file that is not a WebAssembly module. An API key refuses an unknown scope or an expiry in the past.
- `GET /specs/{id}/content` answers `application/json` for a JSON spec.
- The `application/vnd.barbacane.v1+json` content type, which only bodyless responses carried, is gone.

### Probes

`GET /health` is the liveness probe and makes no database query. `GET /ready` answers 200 when the database answers and 503 otherwise. A probe that used `/health` to wait for the database uses `/ready`.

### Data planes

- A data-plane registration is refused with one reason, `registration refused`; the cause is in the control plane's log.
- A registration's `name` is at most 128 characters, its `metadata` 4 KiB; a WebSocket message at most 64 KiB.
- An artifact id a data plane reports is recorded only when the artifact belongs to its project.
- Deleting a data plane ends its session at once.
- A deployment reaches the data planes connected to the control plane instance that received it. Run one control plane instance, or have every data plane connect to the same one.

## Observability

### Problem types

One type per meaning. Update dashboards and alerts that match the old types:

| Was | Is |
|---|---|
| `urn:barbacane:error:internal` (dispatchers, one data-plane path) | `urn:barbacane:error:internal-error` |
| `urn:barbacane:error:forbidden` from `ldap-auth` | `urn:barbacane:error:authorization-failed` |
| `urn:barbacane:error:circuit-open` from `http-upstream` | `urn:barbacane:error:upstream-unavailable` |

A request whose body did not arrive in time answers `urn:barbacane:error:request-timeout` (408), a new type.

### Metrics

- The `method` label is one of the registered HTTP methods, or `OTHER`.
- A plugin's metrics are their own families: a query on `barbacane_plugin_counter_total{plugin="observability",metric="slo_violation"}` reads `barbacane_plugin_observability_slo_violation_total{slo_ms="..."}`; `barbacane_plugin_histogram` becomes the plugin's own histogram, such as `barbacane_plugin_observability_latency_seconds`, observed in seconds.
- `barbacane_upstream_endpoint_requests_total`, `barbacane_upstream_endpoint_retries_total` and `barbacane_upstream_endpoint_circuit_state` describe `endpoints` pools.
- `barbacane_plugin_metrics_dropped_total` counts metrics a plugin emitted that could not be exported, with the reason.

### Logs

- The control plane logs every request that changes something at `info`, with method, path and status.
- A plugin's log line cut at 4 KiB carries `truncated=true`.

## Removed features

| Removed | What to do |
|---|---|
| The AI gateway: `ai-proxy`, `ai-prompt-guard`, `ai-token-limit`, `ai-cost-tracker`, `ai-response-guard`, `schemas/ai-gateway.yaml` | A spec dispatching to `ai-proxy` or using these middlewares no longer compiles against the official plugins. Route LLM traffic through `http-upstream`, or keep a 0.12 gateway for it. Drop `barbacane-validate-ai-regex.js` from ruleset downloads. |
| The MCP server: `/__barbacane/mcp`, `x-barbacane-mcp`, `E1060`, `E1061` | A spec that still sets `x-barbacane-mcp` compiles with an `E1015` unknown-extension warning and serves no MCP endpoint. Remove the extension; drop `barbacane-mcp-requires-fields.js` from ruleset downloads. |
| The control plane's web interface and the nginx server in the `barbacane-control` image | Use `barbacane control` and the interactive documentation at `/docs`. |

## Checklist

| Area | Done when |
|---|---|
| signing | every compiler and the control plane have `BARBACANE_SIGNING_KEY`; every data plane has `BARBACANE_TRUSTED_PUBKEY` and starts without `--allow-unsigned` |
| artifacts | every artifact recompiled with the 1.0 compiler and loaded by a 1.0 data plane |
| `barbacane.yaml` and specs | compile without errors; middleware entries checked for `@version`; YAML constructs from the table checked |
| custom plugins | rebuilt on SDK 1.0; `E1043` clean; `body_access` declared where a body is read; `HttpRequest` literals use `Default` |
| control plane | port 9090; token of 32 bytes or more; clients regenerated from `/openapi`; probes on `/health` and `/ready`; data planes use `BARBACANE_CONTROL_API_KEY` and `wss://` |
| observability | dashboards and alerts updated for the problem types, the `method` label and the plugin metric families; probes parse `/__barbacane/health` for `status` only |
