# ADR-0033: Request Header Allowlist Derived from the Spec

**Status:** Proposed
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
   - Proxy chain information as received today: `x-forwarded-for`, `x-forwarded-proto`, `x-forwarded-host`, `forwarded`. Their trustworthiness is a separate concern (trusted-proxy configuration) and is unchanged by this decision.
2. **Declared parameters**: every `in: header` parameter of the compiled operation (the compiler already merges path-item parameters into operations; `components/parameters` with `$ref` covers reuse). `cookie` is forwarded when the operation declares any `in: cookie` parameter.
3. **Security schemes**: the headers named by the security schemes the operation's `security` requirement applies. An `apiKey` scheme with `in: header` contributes its `name`; an `apiKey` scheme with `in: cookie` contributes `cookie`; `http` schemes (`basic`, `bearer`, and any other scheme) and `oauth2` / `openIdConnect` contribute `authorization`. `Authorization` is therefore never in the baseline: it is forwarded because the spec says the operation is authenticated.
4. **Plugin-configured headers**: header names that the operation's middleware and dispatcher configurations read. A plugin declares such fields in its own `config-schema.json` with `"format": "header-name"`, the same mechanism `writeOnly` already uses for secrets. The compiler resolves every configuration in the chain and collects those values (`apikey-auth.header_name`, `correlation-id.header_name`, `rate-limit` and `ai-token-limit` `header:` partition keys, `cache` vary headers, transformer sources). This set is derived from plugin manifests and the spec's existing middleware configuration; the user's spec gains no new keys.

### Reserved namespace

`x-auth-*` is the auth plugins' output contract. It is always dropped from client requests, and declaring an `x-auth-*` header parameter, security scheme header, or plugin header field is a compile error. #184's ingress strip is this rule's implementation and stays in place.

### Migration

There is no passthrough switch. A header the upstream or a plugin needs is declared in the spec; that is the only way to let it through. Upgrading is supported by observability instead:

- Dropped headers increment `barbacane_request_headers_dropped_total` (no name label, to bound cardinality).
- In `--dev` mode the data plane logs each dropped header name at warn level on the request that carried it, so a developer sees what to declare on the first call. In production the same information is logged at debug level.
- The release notes carry an upgrade note listing the baseline and the three declaration mechanisms.

### Scope

Request headers entering the middleware chain and reaching dispatchers: HTTP, WebSocket upgrade requests, and MCP tool-call dispatch. Response headers from upstreams to clients are unchanged and are a follow-up decision.

### Artifact and validation

The compiler computes each operation's allowlist (sets 2 to 4) and stores it on the compiled operation; `ARTIFACT_VERSION` increments and the field is bound into `artifact_hash`. The baseline is a data-plane constant. `in: cookie` parameters gain the same validation as header parameters so the cookie rule rests on declared data. `apikey-auth` reads its header name from the applied security scheme when one is declared, with `header_name` kept for specs that declare none; the compiler warns when the two disagree.

## Consequences

- The spec becomes the contract for what an upstream receives, not only for what a client may send, using only OpenAPI vocabulary: parameters, security schemes, and the middleware configuration the spec already carries.
- Deployments that rely on undeclared headers reaching the upstream must declare them as parameters (operation, path item, or `components/parameters`). This is a behaviour change with no opt-out, in line with the secure-by-default changes of 0.8.0; it ships with an upgrade note and the observability described above.
- Plugins gain a `format: "header-name"` annotation on the fields that name request headers, which also documents, in one place per plugin, which headers it reads.
- The `x-auth-*` bypass class is closed structurally, and the identity headers are guaranteed to originate from auth plugins.
- Compiler: allowlist computation, security-scheme resolution, cookie parameter validation, `ARTIFACT_VERSION` bump, new compile error for reserved names. Data plane: one filter at the request-conversion sites (already unified in `plugin_request_headers`) and the MCP path, the counter, the dev-mode log. Vacuum ruleset: a rule flagging reserved `x-auth-*` declarations. Docs: spec-configuration guide, middleware guide, `http-upstream` and MCP pages, CLI reference.
