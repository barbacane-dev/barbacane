# Released artifacts

One directory per artifact format, `v<format>/`, holding `.bca` files compiled
by a released compiler and the spec each was compiled from. They are never
rebuilt: `crates/barbacane-test/tests/artifact_corpus.rs` loads every one with
`barbacane check`, which is how a 1.x gateway keeps loading what any earlier
1.x compiler produced.

| Format | Compiled by | Artifact | Spec |
|--------|-------------|----------|------|
| 8 | 1.0.0-rc.1 | `v8/items.bca` | `v8/api.yaml`, with the `mock` dispatcher |
| 9 | 1.0.0-rc.3 | `v9/items.bca` | `v9/api.yaml`, with the `mock` dispatcher |

A release that changes the format adds a directory with an artifact from its
compiler.
