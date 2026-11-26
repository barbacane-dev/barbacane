# Released artifacts

One directory per artifact format, `v<format>/`, holding `.bca` files compiled
by a released compiler and the spec each was compiled from. They are never
rebuilt: `crates/barbacane-test/tests/artifact_corpus.rs` loads every one with
`barbacane check`, which is how a 1.x gateway keeps loading what any earlier
1.x compiler produced.

| Format | Compiled by | Artifact | Spec |
|--------|-------------|----------|------|
| 9 | the 1.0.0 development build (plugin ABI 2); the 1.0.0 release recompiles it | `v9/items.bca` | `v9/api.yaml`, with the `mock` dispatcher |

Artifacts compiled by the 1.0.0 release candidates are not in it: they bundle
plugins built for plugin ABI 1, which this gateway refuses.

A release that changes the format adds a directory with an artifact from its
compiler.
