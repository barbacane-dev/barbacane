# ADR-0008: Dispatch Plugin Interface

**Status:** Accepted
**Date:** 2026-01-28

## Context

ADR-0006 established that Barbacane uses WASM plugins for both **middlewares** (request/response processing) and **dispatchers** (final request delivery). The middleware interface was defined; this ADR addresses the dispatch interface.

A dispatcher is responsible for delivering the processed request to its final destination. Unlike middlewares which form a chain, **each route has exactly one dispatcher** — keeping the mental model simple and predictable.

The term "dispatch" is preferred over "backend" because the plugin's responsibility is the *act of delivering* the request, not the backend service itself.

### Dispatch targets in scope

The dispatch interface must be generic enough to support:

| Target | Example |
|--------|---------|
| HTTP upstream | Reverse proxy to a microservice |
| Mock/static | Return a fixed response (dev, testing, fallback) |
| Serverless | Invoke AWS Lambda, Cloud Functions, etc. |
| Message broker | Publish to Kafka/NATS (sync-to-async bridge) |
| Future targets | Anything a WASM plugin can reach via host functions |

## Decision

### One Route, One Dispatcher

A route declares exactly one dispatcher via `x-barbacane-dispatch`:

```yaml
paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: http://user-service:3000
          timeout: 5s
```

No fan-out, no chaining. If a request needs to reach multiple targets, that is the responsibility of the upstream service, not the gateway.

### Dispatcher Interface

Every dispatch plugin implements:

```
fn dispatch(request: Request, config: Config) -> Response
```

The dispatcher receives the fully processed request (after all middlewares) and returns a response. The response then travels back through the middleware chain.

```
Request → [Middleware 1] → [Middleware 2] → [Dispatcher] → upstream
                                                ↓
Response ← [Middleware 2] ← [Middleware 1] ← [Response]
```

### Host Functions for Dispatchers

WASM plugins have no network access by default. Dispatchers are granted specific host functions depending on their declared capabilities:

| Host function | Purpose | Granted to |
|---------------|---------|------------|
| `http_call` | Make HTTP request to allowed hosts | `http-upstream`, serverless dispatchers |
| `kafka_publish` | Publish message to Kafka topic | `kafka` dispatcher |
| `nats_publish` | Publish message to NATS subject | `nats` dispatcher |
| `static_response` | Return a pre-configured response | `mock` dispatcher |

Dispatchers declare which host functions they need at registration time. The control plane validates these at compile time.

### Built-in Dispatchers

Barbacane ships with core dispatchers as WASM plugins:

| Name | Target | Config |
|------|--------|--------|
| `http-upstream` | HTTP/HTTPS reverse proxy | `url`, `timeout`, `retries`, `circuit-breaker` |
| `mock` | Static/template response | `status`, `headers`, `body` |
| `kafka` | Publish to Kafka topic | `brokers`, `topic`, `key` |
| `nats` | Publish to NATS subject | `servers`, `subject` |
| `lambda` | Invoke AWS Lambda | `function_arn`, `region` |

### Spec Examples

#### HTTP upstream (standard reverse proxy)
```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: http://order-service:8080
    timeout: 10s
    retries: 2
    circuit-breaker:
      threshold: 5
      window: 30s
```

#### Mock response (development/testing)
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    headers:
      content-type: application/json
    body: |
      {"status": "ok", "version": "1.0.0"}
```

#### Sync-to-async bridge (publish to Kafka)
```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    brokers: [kafka-1:9092, kafka-2:9092]
    topic: user.events
    key: path:id
    ack-response:
      status: 202
      body: |
        {"accepted": true}
```

## Consequences

- **Easier:** Clear mental model (one route = one destination), dispatch plugins are simple to write (one function), built-in dispatchers cover most use cases
- **Harder:** No built-in fan-out or orchestration (intentionally — this is gateway, not workflow engine)
- **Extensible:** Anyone can write a custom dispatcher for new targets (databases, gRPC, custom protocols) as a WASM plugin
