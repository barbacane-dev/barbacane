# ADR-0032: Native LDAP Host Functions and the `ldap-auth` Plugin

**Status:** Accepted (supersedes the decision in [ADR-0028](0028-ldap-auth-http-proxy.md))
**Date:** 2026-09-16

## Context

[ADR-0028](0028-ldap-auth-http-proxy.md) parked the `ldap-auth` plugin. It rejected an HTTP-bridge design because a bridge reduces to what `basic-auth`, `oauth2-auth` and `oidc-auth` already do, and it declined the native design (Option A: LDAP host functions backed by the `ldap3` crate) on the grounds that `ldap3` pulls in `cross-krb5`, a C FFI binding to libkrb5, and both `native-tls` and `rustls` unconditionally.

Those two claims do not hold for `ldap3` 0.12:

- `cross-krb5` is optional and enabled only by the `gssapi` feature. Simple binds do not need it.
- TLS is feature-selected. With `default-features = false` and `tls-rustls-aws-lc-rs`, the crate uses rustls on aws-lc-rs, which is the workspace's existing TLS stack. Resolving the dependency in this workspace unifies on the locked `rustls 0.23`, `tokio-rustls 0.26` and `aws-lc-rs 1.16`; no second TLS stack or crypto provider is introduced.
- Every non-optional dependency (`lber`, `nom`, `tokio`, `bytes`, `futures`, `url`) is pure Rust. The only crates new to the workspace are `ldap3`, `lber` and `x509-parser` with its ASN.1 dependencies. All pass the `cargo deny` licence, ban and source rules.

What ADR-0028 identified as real cost remains real: a stateful connection pool in `barbacane-wasm`, and a host-function surface that must be versioned and kept. Kafka and NATS already carry the same kind of cost through `kafka_client.rs` and `nats_client.rs`; LDAP follows their pattern.

LDAP authentication is a differentiator. The mainstream open-source alternatives gate directory integration behind paid tiers; Barbacane ships it as an ordinary plugin.

## Decision

### Capability and host functions

A new `ldap` capability grants three host functions in the `barbacane` import module:

```text
host_ldap_bind(req_ptr: i32, req_len: i32) -> i32
host_ldap_search(req_ptr: i32, req_len: i32) -> i32
host_ldap_read_result(buf_ptr: i32, buf_len: i32) -> i32
```

Requests are JSON in guest memory; the return value is the length of the JSON result, read back with `host_ldap_read_result`, or -1 on an ABI error. Both requests carry the connection parameters (`url`, `bind_dn`, `password`, `starttls`, `timeout_ms`), so the host holds no plugin configuration. `host_ldap_search` adds `base_dn`, `scope` (`base`, `one`, `sub`), `filter`, `attributes` and `size_limit`. The result is `{ success, error?, code?, entries? }`, where `code` is one of `connection_failed`, `invalid_credentials`, `bind_failed`, `search_failed`, `invalid_request`, `timeout`, `blocked`, so a plugin can distinguish a rejected credential (401) from a directory failure (502).

### Host client

`crates/barbacane-wasm/src/ldap_client.rs` owns a dedicated single-worker tokio runtime, like the broker clients, and is entered from the synchronous host function through `std::thread::scope`. After the blocking call the host refreshes the store's epoch deadline, so directory latency is not charged to the plugin's CPU budget.

- **Credential binds use a fresh connection every time.** `host_ldap_bind` connects, binds as the supplied DN, and unbinds. A connection that has carried a user's identity is never cached or reused.
- **Search connections are pooled** and bound as the service account named in the request. The cache key is the calling plugin, the URL, the bind DN and a fingerprint of the password, so rotated credentials do not reuse a stale bind and one plugin cannot reuse another's connection. The pool holds at most 32 connections per plugin and 256 in total, evicting closed entries first and then the least recently used; a failed search evicts its connection.
- **Transport policy.** A password is sent over a plaintext `ldap://` connection without StartTLS only when the request sets `allow_plaintext`; otherwise the call fails with `plaintext_refused` before any connection is opened. Anonymous searches are not affected.
- **SSRF guard.** The directory host is resolved through the same `resolve_permitted_addrs` check as plugin HTTP calls, brokers and WebSocket upstreams, honouring `BARBACANE_ALLOW_INTERNAL_EGRESS`. Plaintext `ldap://` connections are pinned to the vetted address. `ldaps://` and StartTLS connections keep the hostname so SNI and certificate validation work, matching the NATS `tls://` rule; the residual rebinding window between resolution and connect is the same as for NATS and is tracked as a follow-up for both clients.
- **TLS.** `ldaps://` and StartTLS use rustls on aws-lc-rs with the system trust store. A requested StartTLS upgrade that the server refuses fails the connection; there is no plaintext fallback and no option to skip certificate verification.
- **Bounds.** Connect timeout 5 s; per-operation timeout from the request, clamped to 30 s, default 10 s, applied to the pooled bind as well as the search; at most 1000 entries and 1 MiB of entry data per search, enforced while entries stream in so a hostile server cannot grow host memory past the caps. Binary attributes are omitted from results.

### SDK

`barbacane_plugin_sdk::ldap` wraps the host functions with typed requests and results, provides RFC 4515 filter-value and RFC 4514 DN-value escaping, and compiles to `Unsupported` errors on non-WASM targets so plugin unit tests run natively.

### `ldap-auth` plugin

A middleware plugin declaring `host_functions = ["ldap", "log", "clock_now"]`:

1. Extracts `Authorization: Basic` credentials.
2. Searches for the user under `user_base_dn` with `user_filter`, substituting the escaped username, on the service-account connection.
3. Binds as the resolved DN with the submitted password on a fresh connection.
4. Resolves groups from a configured attribute on the user entry (`memberOf` by default) or from a group search, and reduces group DNs to their RDN value unless configured otherwise.
5. On success sets `x-auth-consumer`, `x-auth-consumer-groups`, `x-auth-user` and `x-auth-dn`, and strips `Authorization` unless configured otherwise. On failure returns an RFC 9457 problem with status 401 and a `WWW-Authenticate: Basic realm=...` challenge; an unknown user and a wrong password produce the same response.
6. Caches successful and failed results for a configurable TTL, so a credential-stuffing run does not become a load generator against the directory.

Configuration follows the conventions of the existing auth plugins: `realm` and `strip_credentials` from `basic-auth`; `timeout` in seconds and `*_seconds` TTLs from `oidc-auth`; the service-account password is the single `writeOnly` field and is expected as an `env://` or `file://` reference.

### Out of scope

SASL mechanisms (GSSAPI/Kerberos, NTLM), referrals, paged results, client-certificate authentication to the directory, and directory writes. Each can be added as a further host function or request field without changing the decision here.

## Consequences

- `ldap-auth` ships as an official plugin, and directory-backed authentication no longer requires an OIDC bridge in front of the directory.
- `barbacane-wasm` gains one more native client (`ldap_client.rs`) and three host functions to maintain and version, in the shape of the existing broker clients.
- Three crates join the dependency graph (`ldap3`, `lber`, `x509-parser` with ASN.1 helpers). The TLS stack is unchanged.
- The `ldap` capability appears in `KNOWN_CAPABILITIES`, `capability_to_imports`, the plugin contract documentation and the vacuum ruleset.
- Integration tests run against a real directory: `ghcr.io/glauth/glauth`, which ships a default directory and needs no configuration, is added as a CI service for the integration-test job.
- ADR-0028's rejection of the HTTP-bridge design stands. Its "do not implement" decision and its dependency claims are superseded by this record.
