# ADR-0001: Use ADRs to Record Architecture Decisions

**Status:** Accepted
**Date:** 2026-01-28

## Context

We are building a next-generation API gateway that will involve many significant technical decisions around language choice, frameworks, protocols, and architecture patterns. These decisions need to be documented so that:

- Future contributors understand *why* things are the way they are
- We can revisit decisions when context changes
- Specifications can be derived from a clear decision trail

## Decision

We will use Architecture Decision Records (ADRs) to document all significant architecture decisions. Each ADR will:

- Be numbered sequentially (0001, 0002, ...)
- Be stored in `docs/adr/`
- Follow the lightweight template in `TEMPLATE.md`
- Be immutable once accepted (superseded by new ADRs if changed)

## Consequences

- **Easier:** Onboarding, auditing decisions, writing specs from decisions
- **Harder:** Nothing significant; ADRs are lightweight by design
