# Security testing

The adversarial security suite for Barbacane lives in this crate under
`tests/security.rs` + `tests/security/` (one module per threat category), with
fixtures in `../../tests/fixtures/security/`.

Full documentation — threat model, finding IDs, how to run (incl. the Docker /
PostgreSQL requirements), how to run the cargo-fuzz targets in `../../fuzz/`,
and how to add a new security test — is in:

> **[docs/contributing/security-testing.md](../../docs/contributing/security-testing.md)**

Quick start:

```bash
# Compile-only (no services needed):
cargo test -p barbacane-test --test security --no-run

# Run (RED until findings are fixed — that is intended):
make security-test
```
