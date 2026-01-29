# Contributing to Barbacane

Thank you for your interest in contributing to Barbacane. This document explains how to contribute code, report issues, and participate in the project.

## Code of Conduct

Be respectful. We're all here to build something useful.

## How to Contribute

### Reporting Issues

- Search existing issues before opening a new one
- Use the issue templates when available
- Include reproduction steps for bugs
- For security vulnerabilities, email security@barbacane.dev instead of opening a public issue

### Submitting Changes

1. **Fork and clone** the repository
2. **Create a branch** from `main` for your changes
3. **Make your changes** following the code style guidelines below
4. **Write tests** for new functionality
5. **Run the test suite** to ensure nothing is broken
6. **Submit a pull request** with a clear description

### Pull Request Process

1. PRs require at least one approving review
2. All CI checks must pass (fmt, clippy, tests, benchmarks)
3. Commits should be signed off (see below)
4. Keep PRs focused — one logical change per PR

## Development Setup

### Prerequisites

- Rust stable (install via [rustup](https://rustup.rs/))
- PostgreSQL (for control plane tests)

### Building

```bash
cargo build
```

### Testing

```bash
cargo test
```

### Code Style

We use standard Rust formatting and lints:

```bash
# Format code
cargo fmt

# Run lints
cargo clippy -- -D warnings
```

**Guidelines:**

- Follow Rust API guidelines
- Keep functions focused and small
- Prefer explicit error handling over panics
- Write doc comments for public APIs
- Use meaningful variable and function names

### Commit Messages

Write clear commit messages:

```
Short summary (50 chars or less)

Longer description if needed. Explain what and why,
not how (the code shows how).

Fixes #123
```

## Sign-off (DCO)

All commits must be signed off to certify you wrote or have the right to submit the code:

```bash
git commit --signoff -m "Your commit message"
```

This adds a `Signed-off-by` line to your commit, indicating you agree to the [Developer Certificate of Origin](https://developercertificate.org/):

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.

Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Project Structure

```
Barbacane/
├── adr/                    # Architecture Decision Records
├── specs/                  # Technical specifications
├── crates/
│   ├── barbacane/          # Data plane binary
│   ├── barbacane-control/  # Control plane CLI
│   ├── barbacane-spec-parser/
│   ├── barbacane-compiler/
│   ├── barbacane-router/
│   ├── barbacane-plugin-sdk/
│   └── barbacane-test/
└── tests/fixtures/         # Test spec files
```

## Getting Help

- Open an issue for bugs or feature requests
- Start a discussion for questions or ideas

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
