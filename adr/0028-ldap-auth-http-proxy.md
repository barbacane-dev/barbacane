# ADR-0028: LDAP Auth via HTTP Proxy Instead of Native Host Functions

**Status:** Accepted
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

### Why Option B fits Barbacane's model

In practice, enterprise LDAP/AD is a central directory service, never co-located with the gateway. An HTTP call to an LDAP-over-HTTP bridge is network-equivalent to a direct LDAP call. Suitable bridges include:

- [lldap](https://github.com/lldap/lldap) — lightweight LDAP server with a GraphQL/REST API
- [ldap-proxy](https://github.com/nicowillis/ldap-proxy) — minimal HTTP-to-LDAP adapter
- Any existing identity provider that exposes LDAP federation over REST (Keycloak, Azure AD via MS Graph)

The plugin already has `host_http_call` available. No new capabilities, no new host functions, no new dependencies in the runtime binary.

## Decision

Implement `ldap-auth` as a WASM middleware plugin that authenticates by calling an external LDAP-over-HTTP bridge via `host_http_call`. The plugin accepts a configurable `bridge_url` and performs a credential validation request on each incoming request.

No new host functions or capabilities are added to `barbacane-wasm`.

## Consequences

**Easier:**
- `ldap-auth` ships with zero runtime changes — it is a pure plugin
- Barbacane's binary remains self-contained and edge-deployable
- The bridge can be upgraded, swapped, or replaced independently of Barbacane
- Users with Azure AD, Keycloak, or similar IdPs that already expose REST APIs can skip the bridge entirely

**Harder:**
- Users must run an LDAP-over-HTTP bridge alongside Barbacane (one extra service)
- The plugin cannot do raw LDAP operations (e.g., group membership tree walks) beyond what the bridge exposes; complex LDAP queries depend on bridge capabilities
- Latency adds one extra HTTP hop compared to a native LDAP connection (negligible in most enterprise deployments given LDAP is always remote)
