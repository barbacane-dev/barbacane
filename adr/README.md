# Architecture Decision Records

One file per decision, numbered in order. [ADR-0001](0001-use-adr-for-architecture-decisions.md) sets the format, and [TEMPLATE.md](TEMPLATE.md) is the template.

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-use-adr-for-architecture-decisions.md) | Use ADRs to Record Architecture Decisions | Accepted |
| [0002](0002-rust-as-core-language.md) | Rust as Core Implementation Language | Accepted |
| [0003](0003-project-naming.md) | Project Naming: Barbacane | Accepted |
| [0004](0004-spec-driven-configuration.md) | OpenAPI and AsyncAPI as Single Source of Truth | Accepted |
| [0005](0005-proxy-framework.md) | Proxy Framework: Hyper/Tower Stack | Accepted |
| [0006](0006-wasm-plugin-architecture.md) | WASM Plugin Architecture | Accepted |
| [0007](0007-control-data-plane-separation.md) | Control Plane / Data Plane Separation | Implemented |
| [0008](0008-dispatch-plugin-interface.md) | Dispatch Plugin Interface | Accepted |
| [0009](0009-security-model.md) | Security Model | Accepted |
| [0010](0010-observability.md) | Observability Strategy | Accepted |
| [0011](0011-spec-compilation-model.md) | Spec Compilation Model | Accepted |
| [0012](0012-error-model.md) | Error Model | Accepted |
| [0013](0013-multi-tenancy.md) | Multi-Tenancy: Single Tenant Per Instance | Accepted |
| [0014](0014-testing-strategy.md) | Testing Strategy | Accepted |
| [0015](0015-api-versioning-lifecycle.md) | API Versioning & Lifecycle | Accepted |
| [0016](0016-plugin-development-contract.md) | Plugin Development Contract & Spec Configuration | Accepted |
| [0017](0017-repository-architecture.md) | Repository Architecture | Accepted |
| [0018](0018-kubernetes-gateway-api-compatibility.md) | Kubernetes Gateway API Compatibility | Accepted |
| [0019](0019-packaging-and-release-strategy.md) | Packaging and Release Strategy | Accepted |
| [0020](0020-request-response-transformers.md) | Request/Response Transformers | Accepted |
| [0021](0021-config-provenance.md) | Spec-to-Run configuration provenance and drift detection | Accepted |
| [0022](0022-admin-api.md) | Dedicated listening server for the admin API | Accepted |
| [0023](0023-wasm-plugin-streaming.md) | WASM Plugin Streaming Support | Implemented |
| [0024](0024-ai-gateway-plugin.md) | AI Gateway Plugin | Deprecated |
| [0025](0025-mcp-server.md) | MCP Server Support | Deprecated |
| [0026](0026-websocket-proxy.md) | WebSocket Transparent Proxy | Accepted |
| [0027](0027-oci-artifact-distribution.md) | OCI Artifact Distribution for .bca Artifacts | Proposed |
| [0028](0028-ldap-auth-http-proxy.md) | LDAP Auth via HTTP Proxy Instead of Native Host Functions | Superseded by ADR-0032 |
| [0029](0029-remote-plugin-distribution.md) | Remote Plugin Distribution via HTTPS | Accepted |
| [0030](0030-ai-gateway-responses-api.md) | AI Gateway: OpenAI Responses API Support & Dynamic Model Routing | Deprecated |
| [0031](0031-waf-integration.md) | WAF Integration (ModSecurity / OWASP CRS Compatible) | Accepted |
| [0032](0032-native-ldap-host-functions.md) | Native LDAP Host Functions and the `ldap-auth` Plugin | Accepted |
| [0033](0033-request-header-allowlist.md) | Request Header Allowlist Derived from the Spec | Accepted |
| [0034](0034-disable-one-global-middleware.md) | Disabling One Global Middleware per Operation | Accepted |
| [0035](0035-upstream-load-balancing.md) | Upstream Load Balancing in `http-upstream` | Accepted |
