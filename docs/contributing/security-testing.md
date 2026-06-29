# Security testing

Barbacane ships a dedicated security testing framework with three layers:

1. an **adversarial integration suite** that both regression-locks known
   findings and helps discover new ones,
2. a set of **cargo-fuzz targets** for the highest-value parser / loader /
   memory surfaces, and
3. this documentation plus a `make security-test` entry point.

The suite is designed to be **red today, green as fixes land**: every test
asserts the *secure* (hardened) behaviour, so a failing test is a real, tracked
security gap. Each red test carries a comment of the form
`// EXPECTED TO FAIL until <FINDING-ID> is fixed`.

## Threat model categories

| Finding      | Category              | What it locks down |
|--------------|-----------------------|--------------------|
| BARB-SEC-001 | Authz / IDOR          | Every mutating control-plane route requires an admin Bearer token (`BARBACANE_CONTROL_ADMIN_TOKEN`); `/health` and `/ws/data-plane` are exempt. Project A cannot read/mutate project B's resources. |
| BARB-SEC-002 | SSRF                  | The WASM host HTTP client refuses to connect to loopback / link-local / private / metadata IPs, including after redirects (resolve-then-check). |
| BARB-SEC-003 | DoS / resource limits | Oversized chunked bodies → 413 without full buffering; slowloris read-timeout; 404 floods do not create one Prometheus series per raw path (sentinel `<not_found>`). |
| BARB-SEC-004 | Sandbox / capability  | A plugin granted only `log` cannot call other host functions — the linker is per-plugin / default-deny. |
| BARB-SEC-005 | Crypto / auth         | JWT `alg:none`, expired `exp`, tampered signature, and wrong `aud` are rejected; a forged `X-Forwarded-For` does not bypass `ip-restriction` or reset rate-limit buckets. |
| BARB-SEC-006 | Artifact integrity    | The `.bca` load path verifies per-entry SHA-256 against the manifest and an Ed25519 signature against `BARBACANE_TRUSTED_PUBKEY`; a single flipped byte fails the load. |

## Layer 1 — Adversarial integration suite

The suite lives under `crates/barbacane-test/tests/`:

```
crates/barbacane-test/tests/
  security.rs                       # test-binary root + shared helpers
  security/
    authz.rs                        # BARB-SEC-001
    ssrf.rs                         # BARB-SEC-002
    dos.rs                          # BARB-SEC-003
    sandbox.rs                      # BARB-SEC-004
    crypto_auth.rs                  # BARB-SEC-005
    artifact_integrity.rs           # BARB-SEC-006
```

Fixtures specific to security tests live in `tests/fixtures/security/` (with
their own `barbacane.yaml` manifest).

### Running it

```bash
# Build the binaries the harness drives as subprocesses.
cargo build -p barbacane        # data plane (most categories)
cargo build -p barbacane-control # control plane (BARB-SEC-001)

# Build WASM plugins (the data-plane fixtures bundle them).
make plugins

# Run just the security suite.
cargo test -p barbacane-test --test security
```

**Docker / service requirements** (the same as the rest of the integration
suite — see [Development setup](development.md)):

- Most categories boot the **data plane** (`barbacane serve`) as a subprocess.
  They need the `barbacane` binary built and the fixture WASM plugins present.
- **BARB-SEC-001** boots the **control plane** (`barbacane-control serve`),
  which needs PostgreSQL. Start it with `make db-up` and export `DATABASE_URL`.
  When PostgreSQL or the binary is unavailable the authz tests **skip** (they
  print a `skip:` line and return) rather than fail spuriously.

It is expected that security tests **fail at runtime today** — that is the
point. They turn green as each finding is fixed. A handful are `#[ignore]`d
because they need an src-side change first (see below); each carries a
`// BLOCKED: …` comment explaining exactly what.

### What needs to be exposed for the parked tests

A few tests are `#[ignore]`d with a precise blocker:

- **Sandbox (BARB-SEC-004)** — needs (a) an adversarial fixture plugin that
  declares only `log` but imports `host_http_call`, and (b) a per-plugin
  default-deny linker in `barbacane-wasm` (today `add_host_functions` registers
  *all* host functions). The load-time positive control additionally needs
  `barbacane-wasm` added as a dev-dependency of `barbacane-test` so
  `validate::validate_imports` can be called directly.
- **Crypto (BARB-SEC-005)** — the "validly-signed JWT is accepted" case needs
  real RS256 signature verification (`public_key_pem`) in the `jwt-auth` plugin
  plus a matching private key in the fixture to sign with.
- **DoS slowloris (BARB-SEC-003)** — needs a configurable + observable
  read/header timeout to assert against without flaky wall-clock sleeps.
- **Authz IDOR phase 2 (BARB-SEC-001)** — needs per-project credentials and
  ownership checks on the global `/specs/{id}` and `/artifacts/{id}` routes.

## Layer 2 — Fuzz targets (cargo-fuzz)

The fuzz crate is a **standalone** crate at the repo root (`fuzz/`). It uses the
empty-`[workspace]`-table trick so it is *not* part of the parent Cargo
workspace — no edit to the root `Cargo.toml` `exclude` list is needed, and
`cargo build` at the repo root never pulls it in.

```
fuzz/
  Cargo.toml
  fuzz_targets/
    spec_parser.rs       # barbacane_compiler::parse_spec  — hostile OpenAPI/AsyncAPI
    artifact_loader.rs   # .bca gzip/tar loaders — decompression bombs / malformed archives
    jsonrpc.rs           # MCP JsonRpcRequest deserialization
    validator.rs         # OperationValidator::validate_request (+ percent-decoder)
    wasm_host_memory.rs  # guest-slice bounds — BLOCKED, see below
```

### Prerequisites and running

```bash
rustup toolchain install nightly
cargo install cargo-fuzz

# From the fuzz/ directory:
cd fuzz
cargo +nightly fuzz run spec_parser
cargo +nightly fuzz run artifact_loader
cargo +nightly fuzz run jsonrpc
cargo +nightly fuzz run validator
# cargo +nightly fuzz run wasm_host_memory   # currently inert — see below
```

> Note: `cargo-fuzz` and a nightly toolchain are required to actually *fuzz*.
> The targets also build on stable (`cd fuzz && cargo build --bins`), which is
> what CI / pre-push uses to ensure they don't bit-rot.

### Maintainer actions to sharpen the targets

- **`validator`** reaches the percent-decoder *indirectly* through
  `validate_request` because `urlencoding_decode` is private in
  `crates/barbacane/src/validator.rs`. For a tighter, faster target, expose a
  thin wrapper:
  ```rust
  pub fn fuzz_urldecode(s: &str) -> String { urlencoding_decode(s) }
  ```
- **`artifact_loader`** writes fuzz bytes to a temp file because the loaders take
  `&Path`. A `pub fn load_*_from_reader<R: Read>(r: R)` (or a
  `decompress_artifact<R: Read>(r) -> …`) in
  `crates/barbacane-compiler/src/artifact.rs` would let the target fuzz
  in-memory.
- **`wasm_host_memory`** is currently an inert stub. The guest-slice bounds
  check is duplicated inline in every host-function closure in
  `crates/barbacane-wasm/src/instance.rs` and is not reachable as a pure
  function. Extract it, e.g.:
  ```rust
  pub fn guest_slice_bounds(ptr: i32, len: i32, mem_len: usize)
      -> Result<(usize, usize), GuestMemoryError>;
  ```
  then add `barbacane-wasm` to `fuzz/Cargo.toml` and call it from the target
  (the intended body is sketched in the target's module docs).

## Adding a new security test

1. Pick (or open) a finding ID, e.g. `BARB-SEC-007`.
2. Add the test to the matching category file under
   `crates/barbacane-test/tests/security/`, or create a new category module and
   register it in the `mod security { … }` block in `tests/security.rs`.
3. Assert the **secure** behaviour. Add the marker comment:
   `// EXPECTED TO FAIL until BARB-SEC-007 is fixed`.
4. If the test cannot compile/run against current APIs, mark it `#[ignore]` with
   a `// BLOCKED: needs <X> exposed` comment rather than leaving it broken.
5. Put any new fixtures under `tests/fixtures/security/` and declare the plugins
   they use in `tests/fixtures/security/barbacane.yaml`.
6. Keep tests deterministic — avoid wall-clock sleeps; note any that are
   unavoidable.
7. Update the threat-model table above.
