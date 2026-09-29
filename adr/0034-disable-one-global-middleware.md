# ADR-0034: Disabling One Global Middleware per Operation

**Status:** Proposed (supersedes the "Per-route override" section of [ADR-0006](0006-wasm-plugin-architecture.md))
**Date:** 2026-09-29

## Context

The compiler resolves each operation's middleware chain from the global `x-barbacane-middlewares` and the operation's own ([SPEC-001 §3.1](../specs/SPEC-001-compilation.md), `resolve_middlewares` in `crates/barbacane-compiler/src/artifact.rs`):

- No operation key: the operation runs the global chain.
- `x-barbacane-middlewares: []`: the operation runs no middleware at all.
- A non-empty list: the global entries whose `name` matches no operation entry, in order, followed by the operation entries. An operation entry with the same name as a global one replaces it.

The chain is resolved at compile time and the artifact carries it per route ([ADR-0011](0011-spec-compilation-model.md)), so the data plane only sees the result.

These rules offer no way to remove one global middleware and keep the others. The only removal is `[]`, which removes every one. The common case is a public endpoint under global authentication: a health check, an OAuth callback, an incoming webhook authenticated by a token in its path. Such an operation must drop the global `oidc-auth` or `jwt-auth`, and with `[]` it also drops rate limiting, CORS, logging and every other global middleware.

The alternative today is `[]` followed by the full chain minus the auth plugin, restated on the operation. The restated copy does not follow later changes to the global chain.

Two further properties of the current matching affect any change here:

- Names are compared verbatim. A global `rate-limit@0.1.0` and an operation `rate-limit` do not match, so both run.
- One operation entry replaces every global entry of that name. The traffic-control guide recommends stacking `rate-limit` instances with distinct `policy_name`s; an operation that overrides `rate-limit` replaces all of them with its one entry.

ADR-0006's "Per-route override" section states that an operation overrides the global chain entirely. That does not match the compiler, SPEC-001 or the spec configuration guide, which all describe merging.

## Decision

### `enabled: false` removes a global middleware

A middleware entry takes an optional `enabled` boolean, `true` by default. At operation level, `enabled: false` removes the global entries of that name from the operation's chain and adds nothing:

```yaml
x-barbacane-middlewares:            # global
  - name: oidc-auth
    config: { issuer_url: "env://OIDC_ISSUER_URL", audience: "my-api" }
  - name: rate-limit
    config: { quota: 300, window: 60, partition_key: client_ip }

paths:
  /webhooks/{webhookId}:
    post:
      x-barbacane-middlewares:
        - name: oidc-auth
          enabled: false            # resolved chain: rate-limit
```

`enabled: true` is the same as omitting the key.

### Resolution

For an operation with a non-empty `x-barbacane-middlewares`:

1. The match key of an entry is its plugin name without the version suffix (`rate-limit@0.1.0` and `rate-limit` share the key `rate-limit`), as `normalize_plugin_name` in `crates/barbacane-compiler/src/manifest.rs` computes it.
2. Every global entry whose key matches the key of any operation entry is removed.
3. The operation entries with `enabled` not `false` are appended, in order.

No key and `[]` keep their current meaning.

### Stacked instances

Matching by key applies to every instance of a plugin. Disabling or overriding `rate-limit` on an operation affects all global `rate-limit` instances. Addressing one stacked instance is out of scope for this record; an optional per-entry `id` used as the match key in place of the name is the expected follow-up.

### Validation

New compile errors, in the extension-validity range:

| Code | Condition |
|------|-----------|
| `E1012` | An entry with `enabled: false` also sets `config`; the config would never be used |
| `E1013` | An operation entry with `enabled: false` matches no global entry, so it removes nothing |
| `E1014` | A global entry sets `enabled: false` |

`barbacane validate` reports the same codes. The Barbacane vacuum ruleset gains:

- A rule for each of `E1012` to `E1014`, so specs fail lint the way they fail compilation.
- `barbacane-global-auth-disabled` (info): an operation disables a global authentication plugin. This lists every such operation for review.
- `barbacane-auth-plugins-stacked` (warn): an operation adds an authentication plugin whose key differs from the global one without disabling the global one. Both run, so a request needs both credentials. The message points to `enabled: false`.

### Documentation

SPEC-001 §3.1, the spec configuration guide ("Middleware Merging") and the vacuum guide describe `enabled`, the match key and the stacking scope when this is implemented. ADR-0006's status line records that its "Per-route override" section is superseded.

## Consequences

- A public endpoint keeps the global middlewares it needs (rate limiting, CORS, logging) while dropping authentication, and follows later changes to the global chain.
- Removals are explicit in the spec: `enabled: false` is searchable, and the info rule lists every operation that disables authentication.
- The artifact format and the data plane do not change. The resolved chain is computed at compile time, so artifacts from a compiler with this change run on current gateways.
- Matching by the unversioned name changes the chain of a spec that names the same plugin with and without a version at the two levels: both entries ran, and now the operation entry replaces the global one. The CHANGELOG records this.
- An operation cannot yet disable one of several stacked instances of a plugin.

## Alternatives considered

- **A separate `x-barbacane-middlewares-exclude` list.** It splits one chain across two extensions and needs its own precedence rules against the merge.
- **`[]` followed by the full chain restated.** Available today. The copy does not follow the global chain. Overriding by name has the same cost for config: changing one setting of a global plugin on an operation means restating its whole config there, and keeping that copy in sync by hand.
- **Replacing the global chain entirely when an operation lists middlewares**, as ADR-0006 describes. Every spec that relies on merging would change behaviour, and removing one middleware would still mean restating the rest.
- **A per-instance `id` now.** It also solves stacked instances, but adds a second identity for entries before a spec needs it. Deferred as above.
