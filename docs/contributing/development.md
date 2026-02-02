# Development Guide

This guide helps you set up a development environment for contributing to Barbacane.

## Prerequisites

- **Rust 1.75+** - Install via [rustup](https://rustup.rs/)
- **Git** - For version control
- **Node.js 20+** - For the UI (if working on the web interface)
- **PostgreSQL 14+** - For the control plane (or use Docker)
- **Docker** - For running PostgreSQL locally (optional)

Optional:
- **cargo-watch** - For auto-rebuild on file changes
- **wasm32-unknown-unknown target** - For building WASM plugins (`rustup target add wasm32-unknown-unknown`)
- **tmux** - For running multiple services in one terminal

## Quick Start with Makefile

The easiest way to get started is using the Makefile:

```bash
# Start PostgreSQL in Docker
make db-up

# Build all WASM plugins and seed them into the database
make seed-plugins

# Start the control plane (port 9090)
make control-plane

# In another terminal, start the UI (port 5173)
make ui
```

Then open http://localhost:5173 in your browser.

### Makefile Targets

| Target | Description |
|--------|-------------|
| **Build & Test** | |
| `make` | Run check + test (default) |
| `make test` | Run all workspace tests |
| `make test-verbose` | Run tests with output |
| `make test-one TEST=name` | Run specific test |
| `make clippy` | Run clippy lints |
| `make fmt` | Format all code |
| `make check` | Run fmt-check + clippy |
| `make build` | Build debug |
| `make release` | Build release |
| `make plugins` | Build all WASM plugins |
| `make seed-plugins` | Build plugins and seed registry |
| `make clean` | Clean all build artifacts |
| **Development** | |
| `make control-plane` | Start control plane server (port 9090) |
| `make ui` | Start UI dev server (port 5173) |
| `make dev` | Show instructions to start both |
| `make dev-tmux` | Start both in tmux session |
| **Database** | |
| `make db-up` | Start PostgreSQL container |
| `make db-down` | Stop PostgreSQL container |
| `make db-reset` | Reset database (removes all data) |

Override the database URL:
```bash
make control-plane DATABASE_URL=postgres://user:pass@host/db
```

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
├── Makefile                # Development shortcuts
├── docker-compose.yml      # PostgreSQL for local dev
├── LICENSE
├── CONTRIBUTING.md
├── README.md
│
├── crates/
│   ├── barbacane/          # Data plane CLI (compile, validate, serve)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── barbacane-control/  # Control plane server
│   │   ├── Cargo.toml
│   │   ├── openapi.yaml    # API specification
│   │   └── src/
│   │       ├── main.rs
│   │       ├── server.rs
│   │       └── db/
│   │
│   ├── barbacane-compiler/ # Compilation logic
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── artifact.rs
│   │       └── error.rs
│   │
│   ├── barbacane-spec-parser/  # OpenAPI/AsyncAPI parsing
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── openapi.rs
│   │       ├── asyncapi.rs
│   │       └── error.rs
│   │
│   ├── barbacane-router/   # Request routing
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── trie.rs
│   │
│   ├── barbacane-plugin-sdk/  # Plugin development SDK
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
├── plugins/                # Built-in WASM plugins
│   ├── http-upstream/      # HTTP reverse proxy dispatcher
│   ├── mock/               # Mock response dispatcher
│   ├── lambda/             # AWS Lambda dispatcher
│   ├── kafka/              # Kafka dispatcher (AsyncAPI)
│   ├── nats/               # NATS dispatcher (AsyncAPI)
│   ├── rate-limit/         # Rate limiting middleware
│   ├── cors/               # CORS middleware
│   ├── cache/              # Caching middleware
│   ├── jwt-auth/           # JWT authentication
│   ├── apikey-auth/        # API key authentication
│   └── oauth2-auth/        # OAuth2 token introspection
│
├── ui/                     # React web interface
│   ├── package.json
│   ├── vite.config.ts
│   └── src/
│       ├── pages/          # Page components
│       ├── components/     # UI components
│       ├── hooks/          # Custom React hooks
│       └── lib/            # API client, utilities
│
├── tests/
│   └── fixtures/           # Test spec files
│       ├── minimal.yaml
│       ├── train-travel-3.0.yaml
│       └── ...
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

## UI Development

The web interface is a React application in the `ui/` directory.

### Setup

```bash
cd ui
npm install
```

### Development Server

```bash
# Using Makefile (from project root)
make ui

# Or manually
cd ui && npm run dev
```

The UI runs at http://localhost:5173 and proxies API requests to the control plane at http://localhost:9090.

### Testing

```bash
cd ui
npm run test        # Run tests once
npm run test:watch  # Watch mode
```

### Key Directories

```
ui/src/
├── pages/         # Page components (ProjectPluginsPage, etc.)
├── components/    # Reusable UI components
│   └── ui/        # Base components (Button, Card, Badge)
├── hooks/         # Custom hooks (useJsonSchema, usePlugins)
├── lib/
│   ├── api/       # API client (types, requests)
│   └── utils.ts   # Utilities (cn, formatters)
└── App.tsx        # Main app with routing
```

### Adding a New Page

1. Create page component in `src/pages/`:
   ```tsx
   // src/pages/my-feature.tsx
   export function MyFeaturePage() {
     return <div>...</div>
   }
   ```

2. Add route in `src/App.tsx`:
   ```tsx
   <Route path="/my-feature" element={<MyFeaturePage />} />
   ```

### API Client

Use the typed API client from `@/lib/api`:

```tsx
import { useQuery } from '@tanstack/react-query'
import { listPlugins } from '@/lib/api'

function MyComponent() {
  const { data: plugins, isLoading } = useQuery({
    queryKey: ['plugins'],
    queryFn: () => listPlugins(),
  })
  // ...
}
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

### Benchmarks

Criterion benchmarks are available for performance-critical components:

```bash
# Run all benchmarks
cargo bench --workspace

# Run router benchmarks (trie lookup and insertion)
cargo bench -p barbacane-router

# Run validator benchmarks (schema validation)
cargo bench -p barbacane-validator
```

**Router benchmarks** (`crates/barbacane-router/benches/routing.rs`):
- `router_lookup` - Measures lookup performance for static paths, parameterized paths, and not-found cases
- `router_insert` - Measures route insertion performance at various route counts (10-1000 routes)

**Validator benchmarks** (`crates/barbacane-validator/benches/validation.rs`):
- `validator_creation` - Measures schema compilation time
- `path_param_validation` - Validates path parameters against schemas
- `query_param_validation` - Validates query parameters
- `body_validation` - Validates JSON request bodies
- `full_request_validation` - End-to-end request validation

Benchmark results are saved to `target/criterion/` with HTML reports.

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
