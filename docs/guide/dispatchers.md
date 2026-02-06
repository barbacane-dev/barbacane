# Dispatchers

Dispatchers handle how requests are processed and responses are generated. Every operation in your OpenAPI or AsyncAPI spec needs an `x-barbacane-dispatch` extension.

## Overview

```yaml
paths:
  /example:
    get:
      x-barbacane-dispatch:
        name: <dispatcher-name>
        config:
          # dispatcher-specific config
```

## Available Dispatchers

All dispatchers are implemented as WASM plugins and must be declared in your `barbacane.yaml` manifest.

### mock

Returns static responses. Useful for health checks, stubs, and testing.

```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"status":"ok"}'
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `status` | integer | 200 | HTTP status code |
| `body` | string | `""` | Response body |
| `headers` | object | `{}` | Additional response headers |
| `content_type` | string | `"application/json"` | Content-Type header value |

#### Examples

**Simple health check:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"status":"healthy","version":"1.0.0"}'
```

**Not found response:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 404
    body: '{"error":"resource not found"}'
```

**Empty success:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 204
```

**Custom headers:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '<html><body>Hello</body></html>'
    content_type: 'text/html'
    headers:
      X-Custom-Header: 'custom-value'
      Cache-Control: 'no-cache'
```

### http-upstream

Reverse proxy to an HTTP/HTTPS upstream backend. Supports path parameter substitution, header forwarding, and configurable timeouts.

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"
    path: "/api/v2/resource"
    timeout: 30.0
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `url` | string | Yes | - | Base URL of the upstream (must be HTTPS in production) |
| `path` | string | No | Same as operation path | Upstream path template with `{param}` substitution |
| `timeout` | number | No | 30.0 | Request timeout in seconds |
| `tls` | object | No | - | TLS configuration for mTLS (see below) |

##### TLS Configuration (mTLS)

For upstreams that require mutual TLS (client certificate authentication):

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `tls.client_cert` | string | If mTLS | Path to PEM-encoded client certificate |
| `tls.client_key` | string | If mTLS | Path to PEM-encoded client private key |
| `tls.ca` | string | No | Path to PEM-encoded CA certificate for server verification |

**Example with mTLS:**

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://secure-backend.internal"
    tls:
      client_cert: "/etc/barbacane/certs/client.crt"
      client_key: "/etc/barbacane/certs/client.key"
      ca: "/etc/barbacane/certs/ca.crt"
```

**Notes:**
- Both `client_cert` and `client_key` must be specified together
- Certificate files must be in PEM format
- The `ca` option adds a custom CA for server verification (in addition to system roots)
- TLS-configured clients are cached and reused across requests

#### Path Parameters

Path parameters from the OpenAPI spec are automatically substituted in the `path` template:

```yaml
# OpenAPI path: /users/{userId}/orders/{orderId}
/users/{userId}/orders/{orderId}:
  get:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://backend.internal"
        path: "/api/users/{userId}/orders/{orderId}"

# Request: GET /users/123/orders/456
# Backend: GET https://backend.internal/api/users/123/orders/456
```

#### Path Rewriting

Map frontend paths to different backend paths:

```yaml
# Frontend: /v2/products
# Backend: https://catalog.internal/api/v1/catalog/products
/v2/products:
  get:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://catalog.internal"
        path: "/api/v1/catalog/products"
```

#### Wildcard Proxy

Proxy any path to upstream:

```yaml
/proxy/{path}:
  get:
    parameters:
      - name: path
        in: path
        required: true
        schema:
          type: string
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://httpbin.org"
        path: "/{path}"
        timeout: 10.0
```

#### Timeout Override

Per-operation timeout for long-running operations:

```yaml
/reports/generate:
  post:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://reports.internal"
        path: "/generate"
        timeout: 120.0  # 2 minutes for report generation
```

#### Error Handling

The dispatcher returns RFC 9457 error responses:

| Status | Condition |
|--------|-----------|
| 502 Bad Gateway | Connection failed or upstream returned invalid response |
| 503 Service Unavailable | Circuit breaker is open |
| 504 Gateway Timeout | Request exceeded configured timeout |

#### Security

- **HTTPS required in production**: `http://` URLs are rejected by the compiler (E1031)
- **Development mode**: Use `--allow-plaintext` at compile time and `--allow-plaintext-upstream` at runtime
- **TLS**: Uses rustls with system CA roots by default
- **mTLS**: Configure `tls.client_cert` and `tls.client_key` for mutual TLS authentication
- **Custom CA**: Use `tls.ca` to add a custom CA certificate for private PKI

### lambda

Invokes AWS Lambda functions via Lambda Function URLs. Implemented as a WASM plugin.

```yaml
x-barbacane-dispatch:
  name: lambda
  config:
    url: "https://abc123.lambda-url.us-east-1.on.aws/"
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `url` | string | Yes | - | Lambda Function URL |
| `timeout` | number | No | 30.0 | Request timeout in seconds |
| `pass_through_headers` | boolean | No | true | Pass incoming headers to Lambda |

#### Setup

1. Enable Lambda Function URLs in AWS Console or via CLI
2. Use the generated URL in your OpenAPI spec

```bash
# Enable Function URL for your Lambda
aws lambda create-function-url-config \
  --function-name my-function \
  --auth-type NONE

# Get the URL
aws lambda get-function-url-config --function-name my-function
```

#### Examples

**Basic Lambda invocation:**
```yaml
/api/process:
  post:
    x-barbacane-dispatch:
      name: lambda
      config:
        url: "https://abc123.lambda-url.us-east-1.on.aws/"
```

**With custom timeout:**
```yaml
/api/long-running:
  post:
    x-barbacane-dispatch:
      name: lambda
      config:
        url: "https://xyz789.lambda-url.eu-west-1.on.aws/"
        timeout: 120.0
```

#### Request/Response Format

The dispatcher passes the incoming HTTP request to Lambda:
- Method, headers, and body are forwarded
- Lambda should return a standard HTTP response

Lambda response format:
```json
{
  "statusCode": 200,
  "headers": {"content-type": "application/json"},
  "body": "{\"result\": \"success\"}"
}
```

#### Error Handling

| Status | Condition |
|--------|-----------|
| 502 Bad Gateway | Lambda invocation failed or returned invalid response |
| 504 Gateway Timeout | Request exceeded configured timeout |

### kafka

Publishes messages to Apache Kafka topics. Designed for AsyncAPI specs using the sync-to-async bridge pattern: HTTP POST requests publish messages and return 202 Accepted.

```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    topic: "user-events"
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `topic` | string | Yes | - | Kafka topic to publish to |
| `key` | string | No | - | Message key expression (see below) |
| `ack_response` | object | No | - | Custom acknowledgment response |
| `include_metadata` | boolean | No | false | Include partition/offset in response |
| `headers_from_request` | array | No | `[]` | Request headers to forward as message headers |

##### Key Expression

The `key` property supports dynamic expressions:

| Expression | Description |
|------------|-------------|
| `$request.header.X-Key` | Extract key from request header |
| `$request.path.userId` | Extract key from path parameter |
| `literal-value` | Use a literal string value |

##### Custom Acknowledgment Response

Override the default 202 Accepted response:

```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    topic: "orders"
    ack_response:
      body: {"queued": true, "estimatedDelivery": "5s"}
      headers:
        X-Queue-Name: "orders"
```

#### Examples

**Basic Kafka publish:**
```yaml
# AsyncAPI spec
asyncapi: "3.0.0"
info:
  title: Order Events
  version: "1.0.0"
channels:
  orderEvents:
    address: /events/orders
    messages:
      OrderCreated:
        payload:
          type: object
          properties:
            orderId:
              type: string
operations:
  publishOrder:
    action: send
    channel:
      $ref: '#/channels/orderEvents'
    x-barbacane-dispatch:
      name: kafka
      config:
        topic: "order-events"
        key: "$request.header.X-Order-Id"
        include_metadata: true
```

**With request header forwarding:**
```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    topic: "audit-events"
    headers_from_request:
      - "x-correlation-id"
      - "x-user-id"
```

#### Response

On successful publish, returns 202 Accepted:

```json
{
  "status": "accepted",
  "topic": "order-events",
  "partition": 3,
  "offset": 12345
}
```

(partition/offset only included if `include_metadata: true`)

#### Error Handling

| Status | Condition |
|--------|-----------|
| 502 Bad Gateway | Kafka publish failed |

### nats

Publishes messages to NATS subjects. Designed for AsyncAPI specs using the sync-to-async bridge pattern.

```yaml
x-barbacane-dispatch:
  name: nats
  config:
    subject: "notifications.user"
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `subject` | string | Yes | - | NATS subject to publish to (supports wildcards) |
| `ack_response` | object | No | - | Custom acknowledgment response |
| `headers_from_request` | array | No | `[]` | Request headers to forward as message headers |

#### Examples

**Basic NATS publish:**
```yaml
asyncapi: "3.0.0"
info:
  title: Notification Service
  version: "1.0.0"
channels:
  notifications:
    address: /notifications/{userId}
    parameters:
      userId:
        schema:
          type: string
    messages:
      Notification:
        payload:
          type: object
          required:
            - title
          properties:
            title:
              type: string
            body:
              type: string
operations:
  sendNotification:
    action: send
    channel:
      $ref: '#/channels/notifications'
    x-barbacane-dispatch:
      name: nats
      config:
        subject: "notifications"
        headers_from_request:
          - "x-request-id"
```

**Custom acknowledgment:**
```yaml
x-barbacane-dispatch:
  name: nats
  config:
    subject: "events.user.signup"
    ack_response:
      body: {"accepted": true}
      headers:
        X-Subject: "events.user.signup"
```

#### Response

On successful publish, returns 202 Accepted:

```json
{
  "status": "accepted",
  "subject": "notifications"
}
```

#### Error Handling

| Status | Condition |
|--------|-----------|
| 502 Bad Gateway | NATS publish failed |

---

## Custom WASM Dispatchers

Custom dispatchers can be implemented as WASM plugins using the Plugin SDK. The `mock` dispatcher is an example of a WASM-based dispatcher.

### Planned Dispatchers

The following dispatchers are planned for future releases:

#### AsyncAPI Dispatchers (Kafka, NATS)

Barbacane supports parsing AsyncAPI 3.x specifications. Message broker dispatchers are planned:

```yaml
# Planned Kafka dispatcher
x-barbacane-dispatch:
  name: kafka
  config:
    broker: "kafka.internal:9092"
    topic: "orders"
```

```yaml
# Planned NATS dispatcher
x-barbacane-dispatch:
  name: nats
  config:
    url: "nats://nats.internal:4222"
    subject: "events.orders"
```

#### gRPC Dispatcher

```yaml
x-barbacane-dispatch:
  name: grpc
  config:
    upstream: grpc-backend
    service: users.UserService
    method: GetUser
```

#### GraphQL Dispatcher

```yaml
x-barbacane-dispatch:
  name: graphql
  config:
    upstream: graphql-backend
    operation: |
      query GetUser($id: ID!) {
        user(id: $id) {
          id
          name
          email
        }
      }
```

---

## Dispatcher Development

See [Plugin Development](../contributing/plugins.md) for creating custom dispatchers.

### Dispatcher Interface

```rust
trait Dispatcher {
    /// Initialize with configuration.
    fn init(config: Value) -> Result<Self, Error>;

    /// Handle a request and produce a response.
    async fn dispatch(
        &self,
        ctx: &RequestContext,
    ) -> Result<Response, Error>;
}
```

---

## Best Practices

### Set Appropriate Timeouts

- Fast endpoints (health, simple reads): 5-10s
- Normal operations: 30s (default)
- Long operations (reports, uploads): 60-120s

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://backend.internal"
    timeout: 10.0  # Quick timeout for simple operation
```

### Mock for Development

Use mock dispatchers during API design:

```yaml
/users/{id}:
  get:
    x-barbacane-dispatch:
      name: mock
      config:
        status: 200
        body: '{"id":"123","name":"Test User"}'
```

Then switch to real backend:

```yaml
/users/{id}:
  get:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://user-service.internal"
        path: "/api/users/{id}"
```

### Use HTTPS in Production

Barbacane enforces HTTPS for upstream connections at two levels:

| Flag | Command | Purpose |
|------|---------|---------|
| `--allow-plaintext` | `compile` | Allow `http://` URLs in spec (bypasses E1031 validation) |
| `--allow-plaintext-upstream` | `serve` | Allow runtime HTTP client to connect to `http://` upstreams |

**Why two flags?**

- **Compile-time validation** catches insecure URLs early in your CI pipeline
- **Runtime enforcement** provides defense-in-depth, even if specs are modified

For local development with services like WireMock or Docker containers:

```bash
# Allow http:// URLs during compilation
barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca --allow-plaintext

# Allow plaintext connections at runtime
barbacane serve --artifact api.bca --dev --allow-plaintext-upstream
```

In production, omit both flags to ensure all upstream connections use TLS.
