# Development Guide

This guide helps you set up a development environment for contributing to Barbacane.

## Prerequisites

- **Rust 1.75+** - Install via [rustup](https://rustup.rs/)
- **Git** - For version control

Optional:
- **cargo-watch** - For auto-rebuild on file changes
- **just** - Command runner (alternative to make)

## Getting Started

### Clone the Repository

```bash
git clone https://github.com/barbacane/barbacane.git
cd barbacane
```

### Build

```bash
# Build all crates
cargo build --workspace

# Build in release mode
cargo build --workspace --release
```

### Test

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p barbacane-router

# Run tests with output
cargo test --workspace -- --nocapture

# Run a specific test
cargo test -p barbacane-router trie::tests::static_takes_precedence
```

### Run

```bash
# Validate a spec
cargo run --bin barbacane -- validate --spec tests/fixtures/minimal.yaml

# Compile a spec
cargo run --bin barbacane -- compile --spec tests/fixtures/minimal.yaml --output test.bca

# Run the gateway
cargo run --bin barbacane -- serve --artifact test.bca --listen 127.0.0.1:8080 --dev
```

## Project Structure

```
barbacane/
├── Cargo.toml              # Workspace definition
├── LICENSE
├── CONTRIBUTING.md
├── README.md
│
├── crates/
│   ├── barbacane/          # Main CLI (compile, validate, serve)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── barbacane-compiler/ # Compilation logic
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── artifact.rs
│   │       └── error.rs
│   │
│   ├── barbacane-spec-parser/  # OpenAPI parsing
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── parser.rs
│   │       ├── model.rs
│   │       └── error.rs
│   │
│   ├── barbacane-router/   # Request routing
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── trie.rs
│   │
│   ├── barbacane-plugin-sdk/  # Plugin development
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   │
│   └── barbacane-test/     # Test harness
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           └── gateway.rs
│
├── tests/
│   └── fixtures/           # Test spec files
│       ├── minimal.yaml
│       ├── train-travel-3.0.yaml
│       ├── train-travel-3.1.yaml
│       └── train-travel-3.2.yaml
│
├── docs/                   # Documentation
│   ├── index.md
│   ├── guide/
│   ├── reference/
│   └── contributing/
│
└── adr/                    # Architecture Decision Records
    ├── 0001-*.md
    └── ...
```

## Development Workflow

### Making Changes

1. **Create a branch**
   ```bash
   git checkout -b feature/my-feature
   ```

2. **Make changes and test**
   ```bash
   cargo test --workspace
   ```

3. **Format code**
   ```bash
   cargo fmt --all
   ```

4. **Check lints**
   ```bash
   cargo clippy --workspace -- -D warnings
   ```

5. **Commit**
   ```bash
   git commit -m "feat: add my feature"
   ```

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation
- `refactor:` - Code refactoring
- `test:` - Adding tests
- `chore:` - Maintenance

Examples:
```
feat: add cache middleware
fix: handle empty path in router
docs: add middleware configuration guide
refactor: extract trie traversal logic
test: add integration tests for 405 responses
```

## Adding a New Crate

1. Create the crate directory:
   ```bash
   mkdir -p crates/barbacane-mycrate/src
   ```

2. Create `Cargo.toml`:
   ```toml
   [package]
   name = "barbacane-mycrate"
   description = "Description here"
   version.workspace = true
   edition.workspace = true
   license.workspace = true

   [dependencies]
   # Use workspace dependencies
   serde = { workspace = true }
   ```

3. Add to workspace in root `Cargo.toml`:
   ```toml
   [workspace]
   members = [
       # ...existing crates...
       "crates/barbacane-mycrate",
   ]

   [workspace.dependencies]
   barbacane-mycrate = { path = "crates/barbacane-mycrate" }
   ```

4. Create `src/lib.rs`:
   ```rust
   //! Brief description of the crate.
   //!
   //! More detailed explanation.
   ```

## Testing

### Unit Tests

Place unit tests in the same file:

```rust
// src/parser.rs

pub fn parse(input: &str) -> Result<Spec, Error> {
    // implementation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let result = parse("openapi: 3.1.0...");
        assert!(result.is_ok());
    }
}
```

### Integration Tests

Use `barbacane-test` crate for full-stack tests:

```rust
use barbacane_test::TestGateway;

#[tokio::test]
async fn test_my_feature() {
    let gateway = TestGateway::from_spec("tests/fixtures/my-fixture.yaml")
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/my-endpoint").await.unwrap();
    assert_eq!(resp.status(), 200);
}
```

### Test Fixtures

Add test spec files to `tests/fixtures/`:

```yaml
# tests/fixtures/my-feature.yaml
openapi: "3.1.0"
info:
  title: Test
  version: "1.0.0"
paths:
  /test:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
```

## Debugging

### Logging

Use `eprintln!` for development logging:

```rust
if cfg!(debug_assertions) {
    eprintln!("debug: processing request to {}", path);
}
```

### Running with Verbose Output

```bash
# Gateway with dev mode
cargo run --bin barbacane -- serve --artifact test.bca --dev

# Compile with output
cargo run --bin barbacane -- compile --spec api.yaml --output api.bca
```

### Integration Test Debugging

```bash
# Run single test with output
cargo test -p barbacane-test test_gateway_health -- --nocapture
```

## Performance Profiling

### Benchmarks (coming soon)

```bash
cargo bench -p barbacane-router
```

### Flamegraph

```bash
cargo install flamegraph
cargo flamegraph -p barbacane -- --artifact test.bca
```

## Documentation

### Doc Comments

All public APIs should have doc comments:

```rust
/// Parse an OpenAPI specification from a string.
///
/// # Arguments
///
/// * `input` - YAML or JSON string containing the spec
///
/// # Returns
///
/// Parsed `ApiSpec` or error if parsing fails.
///
/// # Example
///
/// ```
/// let spec = parse_spec("openapi: 3.1.0...")?;
/// println!("Found {} operations", spec.operations.len());
/// ```
pub fn parse_spec(input: &str) -> Result<ApiSpec, ParseError> {
    // ...
}
```

### Generate Docs

```bash
cargo doc --workspace --open
```

## Release Process

1. Update version in workspace `Cargo.toml`
2. Update CHANGELOG.md
3. Create git tag: `git tag v0.1.0`
4. Push: `git push origin main --tags`
5. CI builds and publishes

## Getting Help

- Open an issue on GitHub
- Check existing ADRs for design decisions
- Read the architecture docs
