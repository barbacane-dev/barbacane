# ADR-0003: Project Naming — Barbacane

**Status:** Accepted
**Date:** 2026-01-28

## Context

We need a project name that:

- Reflects the core identity: a secure, spec-driven API gateway built in Rust
- Is pronounceable by English and French speakers (founding team is French)
- Is unique enough to avoid conflicts with existing projects
- Can scale to an ecosystem of tools beyond just the gateway

Candidates considered and rejected:

| Name | Reason for rejection |
|------|---------------------|
| Ferric | Already used (Ferric-AI on GitHub) |
| Écluse | Already used internally |
| Seuil | Hard to pronounce for English speakers |
| Rempart | GitHub organization already taken |
| Oxide, Anvil, Pact, ... | Already widely used in open-source |

## Decision

The project is named **Barbacane**.

A barbican (barbacane in French) is a fortified outpost protecting the gate of a castle — literally a defensive gateway. This maps directly to what the product does: a hardened entry point that enforces API contracts.

- **GitHub organization:** `barbacane-dev` (allows for non-Rust repos in the ecosystem)
- **Vendor extensions:** `x-barbacane-*`
- **Ecosystem convention:** `Barbacane <Product>` (e.g., Barbacane Gateway, Barbacane CLI)

## Consequences

- **Easier:** Strong, unique brand identity with clear meaning in both French and English
- **Harder:** Less immediate "API gateway" recognition than a generic name — requires brand building
