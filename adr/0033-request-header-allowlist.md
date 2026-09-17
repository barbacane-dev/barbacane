# ADR-0033: Request Header Allowlist Derived from the Spec

**Status:** Accepted
**Date:** 2026-09-16

## Context

The data plane forwards every client request header into the middleware chain and to the dispatcher (`crates/barbacane/src/main.rs`, one value per header name). Header parameters declared in the spec (`in: header`) are validated for presence and schema but are not used to filter what is forwarded. `http-upstream` forwards everything except hop-by-hop headers and adds `x-forwarded-*`; the MCP tool-call path forwards all headers by design. Parameters declared `in: cookie` are parsed and ignored by validation.

Consequences of passthrough by default:

- The identity contract between auth plugins and `acl` was forgeable: a client could supply `x-auth-consumer-groups` for a user the auth plugin gives no groups. #184 closes that one namespace at ingress. The general class remains: any header a plugin or an upstream reads without an auth plugin having written it is attacker-controlled.
- Upstreams receive headers the spec never mentions, so the spec does not describe what the upstream actually gets, and header-driven behaviour in upstream applications (feature flags, debug switches, tenant selection, cache variation) is reachable from the outside.
- Plugins that read headers named in their configuration (`apikey-auth.header_name`, `correlation-id.header_name`, `rate-limit` `header:` partition keys, `cache` vary headers, transformers) depend on headers the spec does not mention.

Gateways in the same space forward an allowlist by default (KrakenD forwards six standard headers unless `input_headers` names more). Barbacane's principle is that the OpenAPI or AsyncAPI document drives the gateway, so the allowlist must come from the spec's own vocabulary, not from a Barbacane extension.

## Decision

### Allowlist at ingress

Before the middleware chain runs, the data plane keeps only request headers that fall in one of four sets, matched case-insensitively. Everything else is dropped.

1. **Baseline**, fixed and documented:
   - Message framing and negotiation: `host`, `content-type`, `content-length`, `content-encoding`, `transfer-encoding`, `accept`, `accept-encoding`, `accept-language`, `accept-charset`, `user-agent`, `range`, `if-match`, `if-none-match`, `if-modified-since`, `if-unmodified-since`, `if-range`, `cache-control`, `pragma`, `expect`.
   - CORS: `origin`, `access-control-request-method`, `access-control-request-headers`.
   - Tracing and correlation: `traceparent`, `tracestate`, `x-request-id`.
   - WebSocket upgrade: `upgrade`, `connection`, `sec-websocket-key`, `sec-websocket-version`, `sec-websocket-protocol`, `sec-websocket-extensions`.
   - Proxy chain information as received today: `x-forwarded-for`, `x-forwarded-proto`, `x-forwarded-host`, `x-real-ip`, `forwarded`. Their trustworthiness is a separate concern (trusted-proxy configuration) and is unchanged by this decision. `ip-restriction` and the `client_ip` partition of `rate-limit` and `ai-token-limit` read `x-forwarded-for` and `x-real-ip`, so both are in the baseline rather than left to plugin configuration.
2. **Declared parameters**: every `in: header` parameter of the compiled operation (the compiler already merges path-item parameters into operations; `components/parameters` with `$ref` covers reuse). `cookie` is forwarded when the operation declares any `in: cookie` parameter.
3. **Security schemes**: the headers named by the security schemes the operation's `security` requirement applies. An `apiKey` scheme with `in: header` contributes its `name`; an `apiKey` scheme with `in: cookie` contributes `cookie`; `http` schemes (`basic`, `bearer`, and any other scheme) and `oauth2` / `openIdConnect` contribute `authorization`. `Authorization` is therefore never in the baseline: it is forwarded because the spec says the operation is authenticated.
4. **Plugin headers**: the headers the operation's middleware and dispatchers read. A plugin states these in its own `config-schema.json`, the same file `writeOnly` already uses for secrets, so the user's spec gains no new keys. Three annotations mark a field whose value names a header, and the compiler walks each configuration alongside its schema so a marked field is read where it actually sits:

   - `"format": "header-name"` on a string or a list of them, as `apikey-auth.header_name` and `cache.vary` are.
   - `"format": "header-ref"` on a selector that names a header among other things, as a `rate-limit` partition key does with `header:<name>` and a message key with `$request.header.<name>`. Any other value selects something that is not a header and names nothing.
   - `"format": "header-name-map"` on an object whose *keys* are header names, as a rename table is.

   A field left out of a configuration still applies through its schema `default`, which is the header the plugin will actually read, so the default is collected in its place.

### Authentication requires a security requirement

An operation may run an auth middleware through `x-barbacane-middlewares` and declare no `security` block. Set 3 then admits no credential, the header is dropped, and the middleware rejects every request for want of one. The spec also describes itself as anonymous while requiring a credential, so generated documentation and clients are wrong about it.

Such an operation is a compile error (`E1057`) rather than a silently broken one. A plugin's `plugin.toml` states its family in `category`, and `authentication` is the one the compiler acts on: an operation running such a plugin must name a requirement, on itself or at the root, resolving to a scheme defined under `components.securitySchemes`. The credential header then comes from set 3 like any other, and a third-party auth plugin works the same way without the compiler knowing its name.

The scheme says where the credential travels and the plugin follows, so the compiler does not second-guess it. A key in the query string satisfies the requirement and admits no header, because it needs none. Only `mutualTLS` alone fails, since a certificate is presented during the handshake and no middleware reads it off the request. Where the scheme names a header and the plugin's configuration names a different one, the document wins and the disagreement is reported as `E1072`.

Pairing a plugin with a scheme it does not implement, such as `jwt-auth` with an `apiKey`, is a different question and belongs to `E1032`.

`category` also carries the grouping the middleware guide documents, so the pages and the compiler read one source rather than two.

### Reserved namespace

`x-auth-*` is the auth plugins' output contract. It is always dropped from client requests, whatever an allowlist holds. Naming one is a compile error (`E1056`) on every path a name enters an allowlist by: a declared header parameter, an `apiKey` scheme name, and any of the plugin annotations above.

### Migration

There is no passthrough switch. A header the upstream or a plugin needs is declared in the spec; that is the only way to let it through. Upgrading is supported by observability instead:

- Dropped headers increment `barbacane_request_headers_dropped_total` (no name label, to bound cardinality).
- In `--dev` mode the data plane logs each dropped header name at warn level on the request that carried it, so a developer sees what to declare on the first call. In production the same information is logged at debug level.
- The release notes carry an upgrade note listing the baseline and the three declaration mechanisms.

### Scope

Request headers entering the middleware chain and reaching dispatchers: HTTP, WebSocket upgrade requests, and MCP tool-call dispatch. Response headers from upstreams to clients are unchanged and are a follow-up decision.

### Security scheme model

Set 3 rests on spec vocabulary the compiler does not read today: neither `components.securitySchemes` nor the `security` blocks are parsed, and no Rust type models the scheme kinds. The compiler gains that model: `security` at the root and on the operation, `components.securitySchemes`, and a `SecurityScheme` enum covering `apiKey` (with its `in` and `name`), `http`, `oauth2` and `openIdConnect`. Resolution follows the OpenAPI rule that an operation's `security` overrides the root's, and an empty requirement (`security: []`) makes the operation anonymous and contributes nothing.

The same model is what the long-specified `E1032` (a referenced scheme has no matching auth middleware) and `E1040` (a scheme is defined but never referenced) need. Those checks are not part of this decision, but the model is shaped to carry them. `E1040` is already in use for `UndeclaredPlugin`, so a scheme check takes a free code.

### Artifact and validation

The compiler computes each operation's allowlist (sets 2 to 4) and stores it on the compiled operation. Adding the field changes `routes.json`, whose checksum is bound into `artifact_hash`, so an artifact built before this change fails the existing integrity check. `ARTIFACT_VERSION` increments as the explicit record of the format change, and the data plane gains a check of it at load: today nothing compares the field, so a format mismatch surfaces only as an integrity failure, which names the wrong cause. The baseline is a data-plane constant.

`in: cookie` parameters gain the same validation as header parameters so the cookie rule rests on declared data. `apikey-auth` reads its header name from the applied security scheme when one is declared, with `header_name` kept for specs that declare none; the compiler warns when the two disagree.

The allowlist covers the request headers reaching the middleware chain and the dispatcher. The WAF receives its own header list, which keeps duplicates and non-UTF-8 values and is not filtered: a rule set exists to inspect what the client actually sent.

## Consequences

- The spec becomes the contract for what an upstream receives, not only for what a client may send, using only OpenAPI vocabulary: parameters, security schemes, and the middleware configuration the spec already carries.
- Deployments that rely on undeclared headers reaching the upstream must declare them as parameters (operation, path item, or `components/parameters`). This is a behaviour change with no opt-out, in line with the secure-by-default changes of 0.8.0; it ships with an upgrade note and the observability described above.
- A plugin's `config-schema.json` states which request headers it reads, so that is documented in one place per plugin rather than in its source.
- The `x-auth-*` bypass class is closed structurally, and the identity headers are guaranteed to originate from auth plugins.
- Compiler: an OpenAPI security model where none exists, allowlist computation, security-scheme resolution, cookie parameter validation, `ARTIFACT_VERSION` bump, new compile error for reserved names. Data plane: one filter at dispatch, which HTTP, WebSocket and MCP all reach, and one at CORS preflight, which precedes any operation and so takes the baseline alone; an artifact version check at load; the counter; the dev-mode log. Vacuum ruleset: a rule flagging reserved `x-auth-*` declarations. Docs: spec-configuration guide, middleware guide, `http-upstream` and MCP pages, CLI reference, artifact reference.
- A plugin's `config-schema.json` is embedded in its `.wasm` by the SDK macros, as `plugin.toml` already was, so set 4 works wherever the binary travels: a container image holding only the binary, a release download, or a URL. A plugin built before that convention, or one shipping no schema, falls back to a sibling file and otherwise contributes nothing to set 4; the control plane, whose registry stores neither the schema nor the category, is the remaining case. Each such plugin is reported as `E1071`.
- A plugin that reads headers by evaluating an expression is only partly covered. A literal `headers['<name>']` subscript is read, which is how a `cel` condition is ordinarily written, but a computed name is invisible and `opa-authz` forwards the whole header map to a policy the compiler never sees. Headers reaching those plugins must be declared as parameters, which the middleware guide says at the example that reads one.
