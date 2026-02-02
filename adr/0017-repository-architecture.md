# ADR-0017: Repository Architecture

**Status:** Proposed
**Date:** 2026-01-30

## Context

The Barbacane repository has evolved significantly from its original scope. Initially conceived as a documentation repository, it now contains:

- **Core runtime crates** (`crates/barbacane`, `barbacane-wasm`, `barbacane-control`, `barbacane-test`)
- **Plugin SDK** (`crates/barbacane-plugin-sdk`, `barbacane-plugin-macros`)
- **Official plugins** (`plugins/mock`, `lambda`, `jwt-auth`, `apikey-auth`, `oauth2-auth`, `rate-limit`, `cache`) — and growing
- **Specifications** (`specs/`)
- **ADRs** (`adr/`)
- **Reference documentation** (`docs/`)
- **Test fixtures** (`tests/fixtures/`)

As the plugin ecosystem expands, we need to decide on the repository structure that best supports:

1. Core development velocity
2. Plugin development (official and community)
3. Documentation maintenance
4. Clear boundaries between components

## Options Considered

### Option A: Status Quo (Monorepo)

Keep everything in a single repository.

```
barbacane/
├── crates/           # Core runtime
├── plugins/          # Official plugins
├── docs/             # Documentation
├── specs/            # Specifications
├── adr/              # Architecture decisions
└── tests/            # Integration tests
```

**Pros:**
- Atomic commits across core and plugins
- Single CI pipeline, shared tooling
- Easy local development — `cargo build` builds everything
- Plugin SDK changes immediately testable against all plugins
- Single source of truth for documentation

**Cons:**
- Repository grows with each plugin
- Contributors must clone entire repo
- Plugin releases tied to repo tags (unless we publish crates independently)
- Docs changes create noise for core developers

### Option B: Separate Plugin Repositories

Each official plugin lives in its own repository (`barbacane-plugin-jwt-auth`, `barbacane-plugin-rate-limit`, etc.).

**Pros:**
- Independent versioning per plugin
- Smaller, focused repositories
- Community plugins follow the same structure as official ones

**Cons:**
- Coordination overhead for cross-cutting changes (SDK updates)
- Harder to test plugin compatibility with core changes
- CI duplication across repos
- Discovery problem — users must find plugins across many repos

### Option C: Core + Plugins Monorepo Split

Two repositories: `barbacane` (core runtime) and `barbacane-plugins` (all official plugins).

**Pros:**
- Core stays focused
- Plugins can be released on a different cadence
- Clearer contributor paths (core vs. plugin development)

**Cons:**
- Still need coordination for SDK changes
- Two repos to maintain and release
- Testing cross-repo changes is awkward

### Option D: Monorepo with Plugin Template

Keep all official plugins in the main repository. Provide a `barbacane-plugin-template` repository for community plugin authors.

```
barbacane/                        # Main repo — core + official plugins
├── crates/
├── plugins/
└── ...

barbacane-plugin-template/        # Template repo
├── src/lib.rs
├── plugin.toml
├── config-schema.json
├── Cargo.toml
└── README.md
```

**Pros:**
- Official plugins tested atomically with core
- Community plugins have a clear starting point
- Single main repo for the project
- Template repo is minimal and stable

**Cons:**
- Main repo still grows with plugins
- Template may drift if not actively maintained

## Decision

**Option D: Monorepo with Plugin Template**

We retain the monorepo structure for the core runtime and all official plugins. A separate `barbacane-plugin-template` repository provides a starting point for community plugin development.

### Repository Structure

```
barbacane/                              # Main repository
├── crates/
│   ├── barbacane/                      # Data plane binary
│   ├── barbacane-control/              # Control plane
│   ├── barbacane-wasm/                 # WASM runtime
│   ├── barbacane-plugin-sdk/           # Plugin SDK
│   ├── barbacane-plugin-macros/        # Proc macros
│   └── barbacane-test/                 # Test utilities
├── plugins/
│   ├── mock/                           # Official: mock responses
│   ├── lambda/                         # Official: AWS Lambda dispatch
│   ├── jwt-auth/                       # Official: JWT authentication
│   ├── apikey-auth/                    # Official: API key authentication
│   ├── oauth2-auth/                    # Official: OAuth2 introspection
│   ├── rate-limit/                     # Official: Rate limiting
│   └── cache/                          # Official: Response caching
├── docs/
│   ├── getting-started/
│   ├── reference/
│   └── guides/
├── specs/                              # Formal specifications
├── adr/                                # Architecture decisions
└── tests/
    └── fixtures/                       # Test API specs
```

### Plugin Categories

1. **Core plugins** — shipped with the data plane binary (currently none — all are WASM)
2. **Official plugins** — maintained in `plugins/`, tested in CI, released alongside core
3. **Community plugins** — separate repositories, follow the template, listed in a plugin registry (future M9)

### Versioning Strategy

- Core crates: semver, published to crates.io
- Official plugins: versioned in `plugin.toml`, bundled in artifacts at compile time
- Plugin SDK: semver, published to crates.io — community plugins depend on this

When the SDK has a breaking change:

1. Bump SDK version (major)
2. Update all official plugins in the same PR
3. Document migration path for community plugins
4. Release notes highlight SDK changes

### Documentation Location

Documentation remains in the main repository (`docs/`). Rationale:

- Code and docs evolve together
- Docs reference specific plugin configs — easier to keep in sync
- Single PR can update code + docs atomically
- Generated API docs (rustdoc) come from the same repo

If documentation grows significantly (e.g., a dedicated docs site with search, versioning), we can extract to a `barbacane-docs` repo later. For now, simplicity wins.

### When to Reconsider

Revisit this decision if:

- The plugin count exceeds ~20 and CI times become painful
- A community emerges that wants to contribute plugins without core access
- Documentation requires a static site generator with its own build pipeline

## Consequences

**Easier:**
- Single `git clone` gets everything needed to develop core or plugins
- Plugin SDK changes are immediately tested against all official plugins
- Documentation stays in sync with code
- Clear boundary: official plugins are in the repo, community plugins are external

**Harder:**
- Contributors must understand the full repo structure (mitigated by CONTRIBUTING.md)
- Large PRs that touch core + multiple plugins (but these are rare and usually intentional)
- Plugin authors who only want to work on one plugin still clone the whole repo

**Next steps:**
- Create `barbacane-plugin-template` repository with minimal scaffolding
- Add `CONTRIBUTING.md` with paths for core vs. plugin development
- Document plugin publishing workflow (when M9 Control Plane is complete)
