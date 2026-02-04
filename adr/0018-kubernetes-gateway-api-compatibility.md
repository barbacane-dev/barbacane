# ADR-0018: Kubernetes Gateway API Compatibility

**Status:** Accepted
**Date:** 2026-02-04

## Context

The Kubernetes networking landscape is undergoing a significant shift:

- **Ingress NGINX EOL:** The community-maintained `ingress-nginx` controller [reaches end-of-life in March 2026](https://www.kubernetes.dev/blog/2025/11/12/ingress-nginx-retirement/). No further releases, bugfixes, or security patches will be provided after this date.

- **Ingress API frozen:** While the Ingress API itself is not deprecated, it is feature-frozen. All innovation is happening in the Gateway API.

- **Gateway API maturity:** The [Kubernetes Gateway API](https://gateway-api.sigs.k8s.io/) has reached GA (v1.0+) and is now the recommended standard for traffic management in Kubernetes. Major implementations (Envoy Gateway, Istio, Cilium, Traefik, Kong) have achieved [v1.4.x conformance](https://gateway-api.sigs.k8s.io/implementations/).

Organizations running Kubernetes must now choose their path forward. This ADR addresses how Barbacane fits into this landscape.

### The Question

Should Barbacane implement Gateway API compliance, and if so, how?

Barbacane's core philosophy (ADR-0004) is **spec-driven configuration**: OpenAPI/AsyncAPI specifications are the single source of truth. The Gateway API uses Kubernetes CRDs (Gateway, HTTPRoute, etc.) for routing — a fundamentally different paradigm.

| Aspect | Barbacane | Gateway API |
|--------|-----------|-------------|
| Configuration source | OpenAPI/AsyncAPI specs | Kubernetes CRDs |
| Primary concern | API contract enforcement | Traffic routing |
| Validation | JSON Schema from spec | Basic path/header matching |
| Target audience | API-first teams | Platform/infrastructure teams |

### Key Insight

Gateway API and Barbacane solve **different problems**:

- **Gateway API:** Where does traffic go? (routing, load balancing, TLS termination)
- **Barbacane:** Is this request valid? What policies apply? (contract enforcement, authentication, rate limiting, transformation)

These concerns are complementary, not competing.

## Decision

We will pursue **Option C: Complementary Positioning** — Barbacane operates as a backend behind Gateway API controllers, handling API-specific concerns that Gateway API implementations don't address.

### Options Considered

#### Option A: Native Gateway API Implementation

Implement Barbacane as a GatewayClass controller that directly consumes Gateway API CRDs.

```
Gateway + HTTPRoute CRDs → Barbacane Controller → Upstream Services
```

**Pros:**
- First-class Kubernetes citizen
- Standard conformance testing
- Familiar to platform teams

**Cons:**
- Abandons spec-driven philosophy (ADR-0004)
- Reinvents what Envoy Gateway, Istio, Cilium already do well
- Loses API contract enforcement — the thing that makes Barbacane unique
- Massive implementation effort for table-stakes features

**Verdict:** Wrong direction. We'd be building a worse version of Envoy Gateway.

#### Option B: Gateway API Adapter

Create a Kubernetes operator that translates Gateway API CRDs into OpenAPI specs, then compiles through the Barbacane pipeline.

```
Gateway + HTTPRoute CRDs → Adapter Operator → OpenAPI Spec → Compiler → Data Plane
```

**Pros:**
- Maintains spec-driven philosophy internally
- Gateway API users get Barbacane features

**Cons:**
- Significant complexity: Kubernetes operator, CRD watching, translation layer
- Mapping is lossy (Gateway API and OpenAPI aren't 1:1)
- Adding complexity to arrive at what we already have (OpenAPI specs)
- Must track Gateway API spec changes forever

**Verdict:** Complexity for complexity's sake. We'd build a translator to get back to our starting point.

#### Option C: Complementary Positioning (Selected)

Position Barbacane as a service behind Gateway API controllers. Gateway API handles Kubernetes-native routing; Barbacane handles API semantics.

```
Client → Gateway API Controller → Barbacane → Upstream Services
        (Envoy/Istio/Cilium)     (validation, auth, policies)
```

**Pros:**
- Zero Gateway API implementation required
- Works with any Gateway API controller (Envoy Gateway, Istio, Cilium, etc.)
- Clear value proposition: "Add API contract enforcement to your Gateway API setup"
- Barbacane stays focused on what it does uniquely well
- Simple architecture, no Kubernetes-specific code in core

**Cons:**
- Extra hop in request path (mitigated by same-node deployment)
- Users operate two systems

**Verdict:** Plays to our strengths. Let Gateway API handle routing; we handle API intelligence.

#### Option D: No Gateway API Consideration

Ignore Gateway API entirely. Focus exclusively on standalone spec-driven deployments.

**Pros:**
- Maximum simplicity
- No scope creep

**Cons:**
- Misses the Kubernetes market where Gateway API is becoming standard
- No guidance for users deploying Barbacane in K8s

**Verdict:** Too isolationist. We should document how Barbacane fits in the Gateway API world, even if we don't implement it.

### Why Option C

1. **Separation of concerns:** Gateway API controllers excel at L4/L7 routing, TLS termination, and load balancing. Barbacane excels at API contract enforcement, request validation, and policy execution. Neither needs to do the other's job.

2. **No new code required:** Barbacane already works as an HTTP backend. No operators, no CRD handling, no conformance burden.

3. **Ecosystem leverage:** Users choose their preferred Gateway API implementation (Envoy Gateway, Istio, Cilium) and add Barbacane for API-specific features.

4. **Clear positioning:** "Your Gateway API controller routes traffic. Barbacane enforces your API contract."

5. **Future flexibility:** If strong demand emerges for tighter integration, we can revisit. But we won't build speculatively.

## Architecture

### Deployment Model

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Kubernetes Cluster                               │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    Gateway API Resources                         │   │
│  │  ┌──────────┐  ┌────────────┐                                   │   │
│  │  │ Gateway  │  │ HTTPRoute  │  (Standard Gateway API CRDs)      │   │
│  │  └──────────┘  └────────────┘                                   │   │
│  └─────────────────────────────┬───────────────────────────────────┘   │
│                                │                                        │
│                                ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │              Gateway API Controller                              │   │
│  │         (Envoy Gateway / Istio / Cilium / Traefik)              │   │
│  │                                                                  │   │
│  │  • TLS termination                                              │   │
│  │  • Host/path-based routing                                      │   │
│  │  • Load balancing                                               │   │
│  │  • Traffic splitting                                            │   │
│  └─────────────────────────────┬───────────────────────────────────┘   │
│                                │                                        │
│                    Routes to Barbacane as backend                       │
│                                │                                        │
│                                ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    Barbacane Data Plane                          │   │
│  │                                                                  │   │
│  │  • OpenAPI contract validation                                  │   │
│  │  • JWT / API key authentication                                 │   │
│  │  • Rate limiting                                                │   │
│  │  • Request/response transformation                              │   │
│  │  • Caching                                                      │   │
│  │  • Mock responses                                               │   │
│  └─────────────────────────────┬───────────────────────────────────┘   │
│                                │                                        │
│                                ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    Upstream Services                             │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Example: Envoy Gateway + Barbacane

```yaml
# Gateway API routes /api/* to Barbacane
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: api-route
spec:
  parentRefs:
    - name: main-gateway
  rules:
    - matches:
        - path:
            type: PathPrefix
            value: /api
      backendRefs:
        - name: barbacane
          port: 8080
---
# Barbacane runs as a standard Kubernetes deployment
apiVersion: apps/v1
kind: Deployment
metadata:
  name: barbacane
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: barbacane
          image: barbacane/barbacane:latest
          args:
            - serve
            - --artifact=/config/api.bca
          volumeMounts:
            - name: config
              mountPath: /config
      volumes:
        - name: config
          configMap:
            name: barbacane-artifact
```

### Value Proposition by Layer

| Layer | Responsibility | Tool |
|-------|---------------|------|
| Edge routing | TLS, host routing, path prefix | Gateway API controller |
| API enforcement | Validation, auth, rate limiting | Barbacane |
| Business logic | Application code | Upstream services |

### What Barbacane Adds

Features that Gateway API implementations typically lack or handle poorly:

| Feature | Gateway API | Barbacane |
|---------|-------------|-----------|
| Request body validation | No | JSON Schema from OpenAPI |
| Response validation | No | Contract enforcement |
| Parameter validation | Basic | Full OpenAPI schema |
| API key authentication | Via extensions | Native plugin |
| JWT validation | Via extensions | Native plugin with claims |
| Rate limiting | Via extensions | Flexible key extraction |
| Response caching | No | Vary-aware caching |
| Mock responses | No | From OpenAPI examples |
| Request transformation | Limited | WASM plugins |

## Consequences

### Easier

- **No Kubernetes-specific code:** Core Barbacane remains platform-agnostic
- **No conformance burden:** We don't track Gateway API spec changes
- **Clear value proposition:** Easy to explain what Barbacane adds
- **Deployment flexibility:** Works with any Gateway API implementation
- **Simple architecture:** No operators, no CRD translation

### Harder

- **Extra hop:** Requests traverse Gateway API controller then Barbacane (mitigated by same-node deployment)
- **Two systems:** Users must understand both Gateway API and Barbacane
- **No single pane of glass:** Configuration split between CRDs and OpenAPI specs

### What We're NOT Doing

- Building a Gateway API controller
- Building a CRD-to-OpenAPI translator
- Implementing Gateway API conformance tests
- Adding Kubernetes operator code to the core project

### Future Considerations

If strong demand emerges for tighter Kubernetes integration:

- Helm charts with common Gateway API + Barbacane patterns
- Documentation for specific Gateway API implementations (Envoy Gateway, Istio, etc.)
- Example configurations for popular setups

But we won't build these speculatively.

## Related ADRs

- [ADR-0004: OpenAPI and AsyncAPI as Single Source of Truth](0004-spec-driven-configuration.md)
- [ADR-0007: Control Plane / Data Plane Separation](0007-control-data-plane-separation.md)

## References

- [Kubernetes Gateway API Documentation](https://gateway-api.sigs.k8s.io/)
- [Gateway API Implementations](https://gateway-api.sigs.k8s.io/implementations/)
- [Ingress NGINX Retirement Announcement](https://www.kubernetes.dev/blog/2025/11/12/ingress-nginx-retirement/)
- [CNCF: Understanding Kubernetes Gateway API](https://www.cncf.io/blog/2025/05/02/understanding-kubernetes-gateway-api-a-modern-approach-to-traffic-management/)
