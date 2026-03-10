# ADR-0028: LDAP Auth via HTTP Proxy Instead of Native Host Functions

**Status:** Rejected
**Date:** 2026-03-10

## Context

`ldap-auth` is a planned middleware plugin that authenticates requests against an LDAP/Active Directory directory. Two implementation strategies were considered:

**Option A — Native LDAP host functions**: add `host_ldap_bind`, `host_ldap_search`, etc. to `barbacane-wasm`, backed by the `ldap3` Rust crate.

**Option B — HTTP proxy**: the plugin calls `host_http_call` against an external LDAP-over-HTTP bridge (sidecar or standalone service). No new host functions required.

### Why Option A is problematic for Barbacane

**`cross-krb5` FFI dependency.** `ldap3` pulls in `cross-krb5`, a C FFI binding to `libkrb5` (Kerberos). This:
- Requires `libkrb5-dev` system libraries at link time
- Makes the Barbacane binary non-self-contained on edge targets where those libraries are not present
- Adds C code to the trust boundary

**Duplicate TLS stacks.** `ldap3` unconditionally pulls in both `native-tls` and `rustls`. Barbacane currently uses a single TLS stack via `reqwest`; adding a second one increases binary size for no user-visible benefit.

**Stateful connection management in the host.** LDAP is a stateful protocol (bind → search → unbind). A correct implementation requires a connection pool in `barbacane-wasm` (similar to `kafka_client.rs` / `nats_client.rs`) with reconnect logic, TLS upgrade handling, and per-store pool sizing. This is non-trivial maintenance surface.

**New host function surface.** At minimum: `host_ldap_bind`, `host_ldap_search`, `host_ldap_unbind`, and a result-reading function — all of which must be versioned and supported indefinitely.

### Why Option B dissolves the problem it was trying to solve

LDAP is a custom binary protocol — the same is true of Kafka and NATS, which Barbacane implements natively. The difference is that `rskafka` and `async-nats` are pure-Rust, FFI-free, and self-contained. The HTTP proxy was proposed as a pragmatic workaround for the C FFI constraint, not as a statement about LDAP's nature.

On closer inspection, the minimal HTTP contract required for Option B reduces to:

```
POST {validate_url}
Content-Type: application/json
{"username": "alice", "password": "secret"}

→ 200 OK              # credentials valid
→ 401 Unauthorized    # credentials invalid
```

This is not LDAP. It is "Basic Auth delegated to a remote HTTP endpoint" — which is already covered by the existing plugin set:

- **`basic-auth`** handles Basic Auth credential extraction
- **`oauth2-auth`** covers credential delegation via the OAuth2 ROPC flow (RFC 6749 §4.3), which most LDAP-backed identity providers expose
- **`oidc-auth`** covers LDAP-backed IdPs that surface OIDC endpoints

What genuinely distinguishes LDAP auth — direct LDAP bind, DN-based user resolution, `memberOf` group checks — requires native LDAP operations that the HTTP abstraction cannot express without replicating a proprietary bridge API.

## Decision

**Do not implement `ldap-auth` at this time.**

Option A (native host functions) is blocked by the `cross-krb5` C FFI dependency in the only production-grade Rust LDAP client. Option B (HTTP bridge) reduces to functionality already covered by `basic-auth`, `oauth2-auth`, and `oidc-auth`, and strips out the LDAP-specific semantics that would justify a dedicated plugin.

The item is parked until a pure-Rust, FFI-free LDAP client exists. At that point it should be revisited as native host functions following the Kafka/NATS pattern.

## Consequences

- No new plugin or host functions are introduced
- Users needing LDAP/AD authentication today should front their directory with an OIDC-capable IdP and use `oidc-auth`
- The roadmap item is marked as blocked pending Rust ecosystem maturity
