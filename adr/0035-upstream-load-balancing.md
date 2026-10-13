# ADR-0035: Upstream Load Balancing in `http-upstream`

**Status:** Proposed (amends the division of responsibilities in [ADR-0018](0018-kubernetes-gateway-api-compatibility.md))
**Date:** 2026-10-14

## Context

`http-upstream` sends an operation's requests to one `url`. Spreading them across the instances of a backend is left to whatever answers at that URL. [ADR-0018](0018-kubernetes-gateway-api-compatibility.md) gives load balancing to the Gateway API controller deployed with Barbacane, and [ADR-0005](0005-proxy-framework.md) records load balancing and upstream health checks as work Barbacane would do itself if it needed them.

What the data plane does with one URL:

- **Behind a Kubernetes Service:** kube-proxy balances connections, not requests. The host HTTP client keeps up to 10 idle connections per host (`pool_max_idle_per_host`), so requests spread over the pods those connections reached.
- **Behind a name with several addresses:** the client connects to the first permitted address that accepts and reuses that connection. The other addresses take traffic only when it fails.
- **Behind a platform load balancer:** balanced as intended.

Without a platform balancer between Barbacane and its backends (virtual machines, Docker Compose, backends spread over networks or clouds), an operator must add one. Kong (upstreams and targets), APISIX (upstreams), Envoy (clusters) and Tyk balance upstreams themselves.

The host HTTP client carries a circuit breaker that nothing enables (`crates/barbacane-wasm/src/circuit_breaker.rs`, `HttpClient::circuit_breakers`):

- `configure_circuit_breaker` is the only way to register one, and only tests call it, so every lookup returns `Closed`.
- It is keyed by host name, without scheme or port.
- It counts transport failures only (connect errors, timeouts). Any HTTP response, a 503 included, records a success.
- In half-open state every request goes through; the first success closes it and any failure reopens it. The module comment says one request is let through.
- Every request takes the breaker map's write lock to record its result, although no breaker is registered.
- The error surface exists: the host maps `CircuitOpen` to a 503 of type `urn:barbacane:error:circuit-open`, and `http-upstream` uses the same type for a 503.

The roadmap states that no general circuit breaker is planned.

A plugin instance lives for one request ([#138](https://github.com/barbacane-dev/barbacane/issues/138)), so state that must outlive a request, such as a rotation position or an endpoint's health, cannot live in the plugin.

## Decision

### Scope

Balancing the requests of one dispatch across several endpoints of the same service. Out of scope:

- Balancing in front of the gateway's replicas, which stays with the platform.
- Service discovery. Endpoints come from the spec, and a name keeps resolving through DNS as it does today.
- Splitting traffic between different services or versions. The roadmap's `canary` middleware covers that, and can reuse the weights defined here.
- `ws-upstream`. Its connections are long-lived ([ADR-0026](0026-websocket-proxy.md)), so per-request balancing does not apply as is.

### Configuration

`http-upstream` takes either `url`, as today, or `endpoints`:

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    endpoints:
      - url: https://orders-1.internal:8443
      - url: https://orders-2.internal:8443
      - url: https://orders-3.internal:8443
        weight: 2
    path: /orders/{orderId}
    timeout: 10
    retries: 1
```

- `endpoints` holds one or more entries, each with a `url` (required, accepting `env://` and `file://` references as `url` does) and a `weight` (integer, at least 1, default 1).
- `url` and `endpoints` are exclusive. The config schema states it, so a config with both, or neither, fails `E1023`.
- `E1031` applies to every endpoint URL. `extract_upstream_url` reads `url` only today.
- The name is `endpoints`, not `targets`: in `ai-proxy`, `targets` are named tiers chosen by a policy ([ADR-0030](0030-ai-gateway-responses-api.md)), not a pool.
- Endpoints are listed per operation, as `url` is.

### Selection

Smooth weighted round-robin, the algorithm nginx uses: deterministic, plain round-robin when weights are equal, and an endpoint's extra turns interleaved rather than consecutive. The rotation belongs to a pool, identified by its list of endpoints and weights, so operations listing the same endpoints share one rotation.

Least-request selection is a follow-up: it needs the in-flight count of each endpoint, which the host can keep.

### Where balancing runs

In the host, next to the connection pools. The SDK's outbound request gains an optional list of endpoints. When it is set, the request's `url` holds the path and query, and the host prefixes the endpoint it selects. This goes through a new host function and capability, as `hash` did for `s3` in 0.12.2, so a gateway that predates this change refuses the new `http-upstream.wasm` when it loads rather than misrouting its requests.

### Health: the circuit breaker becomes per-endpoint passive health checking

The dormant breaker is kept and given this role:

- It is keyed by endpoint: scheme, host and port.
- A breaker is registered for each endpoint of an `endpoints` list the first time a request uses it.
- Failures are transport errors (connect, TLS handshake, timeout) and responses `502`, `503` and `504`, where the endpoint or something in front of it reports itself unavailable. Other responses, `500` included, are answers from the application and record a success.
- Selection skips an endpoint whose breaker is open. After the reset timeout it takes requests again; the first success closes the breaker and a failure reopens it. The module comment is corrected to say so.
- When every endpoint of a pool is open, the host sends to them anyway, as Envoy's panic threshold does, so that a burst of false positives does not turn into a full outage. The responses then decide.
- Thresholds are gateway settings, not per operation. One endpoint's breaker serves every operation that lists it, so two operations could not hold different thresholds for it. The breaker's current defaults are kept: 5 failures within 60 seconds open it, it is tried again after 30 seconds, and 1 success closes it.
- An upstream given as a single `url` gets no breaker. Failing fast on the only endpoint replaces a slow failure with a 503 without sending the request anywhere else. That is a load-shedding policy and a decision of its own, so `url` keeps its current behaviour.
- Results are recorded only for registered endpoints, under the map's read lock; each breaker holds its own lock. Requests to a single `url` no longer take a write lock.
- `configure_circuit_breaker`, which has no caller, is replaced by the registration above.

### Retries

- A request whose connection failed (connect error, connect timeout, TLS handshake) never reached the endpoint. It is sent to the next endpoint whatever its method.
- A request that was sent, and answered `502`, `503` or `504` or timed out while waiting for the response, is sent to another endpoint only if its method is idempotent (RFC 9110 §9.2.2: `GET`, `HEAD`, `OPTIONS`, `TRACE`, `PUT`, `DELETE`).
- `retries` (default 1, at most the number of endpoints minus one) bounds the extra attempts. It is request policy, so it is set per operation.
- All attempts share the operation's `timeout`, so retries cannot extend a request beyond it.
- A dispatcher receives the whole request body, so the body can be sent again.

### Observability

- Metrics per endpoint: requests, failures, breaker state, and retries.
- The dispatch span records the endpoint used.
- Breaker transitions are logged with the endpoint.

### Compatibility

- The artifact format does not change: endpoints are dispatcher configuration, which the artifact carries without interpreting.
- The new `http-upstream.wasm` declares the new capability, so it needs a gateway with this change. An older `http-upstream.wasm` keeps working on such a gateway.
- ADR-0018's positioning stands: the platform balances in front of the gateway and a Gateway API controller routes to it. This record adds that Barbacane balances its own calls to a backend when the spec lists several endpoints.

## Consequences

- A deployment without a platform balancer lists a backend's instances in the spec, and an instance that fails is routed around.
- Endpoint lists are static: the spec, with `env://` and `file://` references. Changing a backend's instances means recompiling, or naming a host or address that resolves to them.
- An operation's endpoint list is repeated wherever the same backend is used. A shared definition is a possible follow-up, below.
- The circuit breaker becomes code that runs, with a documented and tested role. Upstreams given as a single `url` keep their current behaviour.
- When this is implemented, the roadmap's circuit breaker line describes per-endpoint breakers for `endpoints` pools, and "Connection pooling tuning" keeps active health checks only.
- Least-request selection, active health checks and the `canary` middleware's weighted splits remain follow-ups.

## Alternatives considered

- **Leave balancing to the platform.** Keeps ADR-0018's split as it is, but a deployment without a platform balancer needs one added, and per-request spreading in Kubernetes is not available.
- **Named upstreams** at the spec or manifest root, such as `x-barbacane-upstreams`, referenced by name from a dispatch. Defined once, and the host knows every endpoint at startup, which active health checks need. It adds a section to the artifact (a format change), a namespace shared by every spec, and a new class of reference errors. It can be built later on the same host machinery, with `endpoints` as the inline form.
- **Balancing in the plugin.** A plugin instance lives for one request, so it cannot keep a rotation or health state. Host functions to store that state would rebuild the host design with an extra round trip per decision.
- **DNS round-robin**, spreading requests over the addresses one name resolves to. There are no weights or health, pooled connections stay with the address that answered first, and the SSRF guard already filters those addresses. Deployments that want it have DNS-aware balancers.
- **Deleting the circuit breaker.** Removes untested code, but per-endpoint health needs this state machine.
- **Enabling the breaker for every upstream host.** Changes the behaviour of every single-`url` upstream (a 503 with no other endpoint to try), and as written it counts transport errors only and keys by host without port.
