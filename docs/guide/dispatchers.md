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

Returns static or interpolated responses. Useful for health checks, stubs, and testing.

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
| `body` | string | `""` | Response body (supports `{{placeholder}}` interpolation) |
| `headers` | object | `{}` | Additional response headers |
| `content_type` | string | `"application/json"` | Content-Type header value |

#### Body Interpolation

The `body` field supports `{{placeholder}}` interpolation with values from the incoming request:

| Placeholder | Description |
|-------------|-------------|
| `{{request.method}}` | HTTP method (GET, POST, etc.) |
| `{{request.path}}` | Request path |
| `{{request.query}}` | Query string (unresolved if absent) |
| `{{request.client_ip}}` | Client IP address |
| `{{headers.<name>}}` | Request header value (case-insensitive fallback) |
| `{{path_params.<name>}}` | Path parameter value |

Unresolved placeholders are left as-is in the response body.

#### Examples

**Simple health check:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"status":"healthy","version":"1.0.0"}'
```

**Interpolated response with auth context:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 418
    body: '{"error":"I am a teapot","consumer":"{{headers.x-auth-key-name}}"}'
```

**Path parameter echo:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"userId":"{{path_params.userId}}","method":"{{request.method}}"}'
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

Proxy any path to upstream using a greedy wildcard parameter (`{param+}`):

```yaml
/proxy/{path+}:
  get:
    parameters:
      - name: path
        in: path
        required: true
        allowReserved: true
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

Publishes messages to Apache Kafka topics. Designed for AsyncAPI specs using the sync-to-async bridge pattern: HTTP POST requests publish messages and return 202 Accepted. Uses a pure-Rust Kafka client with connection caching and a dedicated runtime.

```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    brokers: "kafka.internal:9092"
    topic: "user-events"
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `brokers` | string | Yes | - | Comma-separated Kafka broker addresses (e.g. `"kafka:9092"` or `"broker1:9092, broker2:9092"`) |
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
    brokers: "kafka.internal:9092"
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
        brokers: "kafka.internal:9092"
        topic: "order-events"
        key: "$request.header.X-Order-Id"
        include_metadata: true
```

**With request header forwarding:**
```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    brokers: "kafka.internal:9092"
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

Publishes messages to NATS subjects. Designed for AsyncAPI specs using the sync-to-async bridge pattern. Uses a pure-Rust NATS client with connection caching and a dedicated runtime.

```yaml
x-barbacane-dispatch:
  name: nats
  config:
    url: "nats://nats.internal:4222"
    subject: "notifications.user"
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `url` | string | Yes | - | NATS server URL (e.g. `"nats://localhost:4222"`) |
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
        url: "nats://nats.internal:4222"
        subject: "notifications"
        headers_from_request:
          - "x-request-id"
```

**Custom acknowledgment:**
```yaml
x-barbacane-dispatch:
  name: nats
  config:
    url: "nats://nats.internal:4222"
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

### s3

Proxies requests to AWS S3 or any S3-compatible object storage (MinIO, RustFS, Ceph, etc.) with AWS Signature Version 4 signing. Supports multi-bucket routing via path parameters and single-bucket CDN-style routes.

```yaml
x-barbacane-dispatch:
  name: s3
  config:
    region: us-east-1
    access_key_id: env://AWS_ACCESS_KEY_ID
    secret_access_key: env://AWS_SECRET_ACCESS_KEY
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `access_key_id` | string | Yes | - | AWS access key ID. Supports `env://` references (e.g. `env://AWS_ACCESS_KEY_ID`) |
| `secret_access_key` | string | Yes | - | AWS secret access key. Supports `env://` references |
| `region` | string | Yes | - | AWS region (e.g. `us-east-1`, `eu-west-1`) |
| `session_token` | string | No | - | Session token for temporary credentials (STS / AssumeRole / IRSA). Supports `env://` references |
| `endpoint` | string | No | - | Custom S3-compatible endpoint URL (e.g. `https://minio.internal:9000`). When set, path-style URLs are always used |
| `force_path_style` | boolean | No | `false` | Use path-style URLs (`s3.{region}.amazonaws.com/{bucket}/{key}`) instead of virtual-hosted style. Automatically `true` when `endpoint` is set |
| `bucket` | string | No | - | Hard-coded bucket name. When set, `bucket_param` is ignored. Use for single-bucket routes like `/assets/{key+}` |
| `bucket_param` | string | No | `"bucket"` | Name of the path parameter that holds the bucket |
| `key_param` | string | No | `"key"` | Name of the path parameter that holds the object key |
| `timeout` | number | No | `30` | Request timeout in seconds |

#### URL Styles

**Virtual-hosted (default for AWS S3):**
```
Host: {bucket}.s3.{region}.amazonaws.com
Path: /{key}
```

**Path-style (set `force_path_style: true` or when `endpoint` is set):**
```
Host: s3.{region}.amazonaws.com   # or custom endpoint host
Path: /{bucket}/{key}
```

Custom endpoints always use path-style regardless of `force_path_style`.

#### Wildcard Keys

Use `{key+}` (greedy wildcard) to capture multi-segment S3 keys containing slashes:

```yaml
/files/{bucket}/{key+}:
  get:
    parameters:
      - { name: bucket, in: path, required: true, schema: { type: string } }
      - { name: key, in: path, required: true, allowReserved: true, schema: { type: string } }
    x-barbacane-dispatch:
      name: s3
      config:
        region: us-east-1
        access_key_id: env://AWS_ACCESS_KEY_ID
        secret_access_key: env://AWS_SECRET_ACCESS_KEY
```

`GET /files/uploads/2024/01/report.pdf` → S3 key `2024/01/report.pdf` in bucket `uploads`.

#### Examples

**Multi-bucket proxy with OIDC authentication:**
```yaml
paths:
  /storage/{bucket}/{key+}:
    get:
      parameters:
        - { name: bucket, in: path, required: true, schema: { type: string } }
        - { name: key, in: path, required: true, allowReserved: true, schema: { type: string } }
      x-barbacane-middlewares:
        - name: oidc-auth
          config:
            issuer: https://auth.example.com
            audience: my-api
            required: true
      x-barbacane-dispatch:
        name: s3
        config:
          region: eu-west-1
          access_key_id: env://AWS_ACCESS_KEY_ID
          secret_access_key: env://AWS_SECRET_ACCESS_KEY
```

**Single-bucket CDN (hard-coded bucket, rate limited):**
```yaml
paths:
  /assets/{key+}:
    get:
      parameters:
        - { name: key, in: path, required: true, allowReserved: true, schema: { type: string } }
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            quota: 120
            window: 60
      x-barbacane-dispatch:
        name: s3
        config:
          region: us-east-1
          bucket: public-assets
          access_key_id: env://AWS_ACCESS_KEY_ID
          secret_access_key: env://AWS_SECRET_ACCESS_KEY
```

**S3-compatible storage (MinIO / RustFS):**
```yaml
x-barbacane-dispatch:
  name: s3
  config:
    region: us-east-1
    endpoint: https://minio.internal:9000
    access_key_id: env://MINIO_ACCESS_KEY
    secret_access_key: env://MINIO_SECRET_KEY
    bucket: uploads
```

**Temporary credentials (STS / AssumeRole / IRSA):**
```yaml
x-barbacane-dispatch:
  name: s3
  config:
    region: us-east-1
    access_key_id: env://AWS_ACCESS_KEY_ID
    secret_access_key: env://AWS_SECRET_ACCESS_KEY
    session_token: env://AWS_SESSION_TOKEN
    bucket: my-bucket
```

#### Error Handling

| Status | Condition |
|--------|-----------|
| 400 Bad Request | Missing bucket or key path parameter |
| 502 Bad Gateway | `host_http_call` failed (network error, endpoint unreachable) |
| 403 / 404 / 5xx | Passed through transparently from S3 |

#### Security

- **Credentials**: Use `env://` references so secrets are never baked into spec files or compiled artifacts
- **Session tokens**: Support for STS, AssumeRole, and IRSA (IAM Roles for Service Accounts) via `session_token`
- **Signing**: All requests are signed with AWS Signature Version 4. The signed headers are `host`, `x-amz-content-sha256`, `x-amz-date`, and `x-amz-security-token` (when a session token is present)
- **Binary objects**: The current implementation returns the response body as a UTF-8 string. Binary objects (images, archives, etc.) are not suitable for this dispatcher — use pre-signed URLs for binary downloads

### ai-proxy

AI gateway dispatcher — exposes a unified OpenAI-compatible API to clients and routes to LLM providers (OpenAI, Anthropic, Ollama). Supports named targets for policy-driven routing, provider fallback on failure, and token count propagation for downstream middlewares.

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    provider: openai
    model: gpt-4o
    api_key: "${OPENAI_API_KEY}"
```

#### How It Works

Clients always send requests in [OpenAI chat completion format](https://platform.openai.com/docs/api-reference/chat). The dispatcher handles provider differences transparently:

- **OpenAI / Ollama** — requests are passed through as-is (both are OpenAI-compatible).
- **Anthropic** — requests and responses are automatically translated between OpenAI format and the [Anthropic Messages API](https://docs.anthropic.com/en/api/messages) (system messages extracted, stop reasons mapped, usage fields normalized).

After each successful call, the dispatcher writes context keys (`ai.provider`, `ai.model`, `ai.prompt_tokens`, `ai.completion_tokens`) for downstream middlewares to consume (e.g. for cost tracking or token budgets).

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `provider` | string | If no `targets` | - | Provider type: `openai`, `anthropic`, or `ollama` |
| `model` | string | If no `targets` | - | Model identifier (e.g. `gpt-4o`, `claude-opus-4-6`, `mistral`) |
| `api_key` | string | No | - | API key. Supports `${ENV_VAR}` substitution. Omit for Ollama |
| `base_url` | string | No | Provider default | Custom endpoint URL (Azure OpenAI, self-hosted vLLM, remote Ollama, etc.) |
| `timeout` | integer | No | 120 | Request timeout in seconds |
| `max_tokens` | integer | No | - | Default `max_tokens` injected when the client omits it. Required for Anthropic (enforced by their API). Acts as a cost guardrail for OpenAI/Ollama |
| `fallback` | array | No | `[]` | Ordered list of fallback providers (see below) |
| `targets` | object | No | - | Named provider targets for policy-driven routing (see below) |
| `default_target` | string | No | - | Target to use when no `ai.target` context key is set. Must match a key in `targets` |

**Provider defaults:**

| Provider | Default Base URL |
|----------|-----------------|
| `openai` | `https://api.openai.com` |
| `anthropic` | `https://api.anthropic.com` |
| `ollama` | `http://localhost:11434` |

#### Configuration Modes

**Flat (single provider):**

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    provider: openai
    model: gpt-4o
    api_key: "${OPENAI_API_KEY}"
    max_tokens: 4096
```

**Named targets (policy-driven routing):**

Define named provider profiles. The `cel` middleware selects the active target by writing `ai.target` into context before the dispatcher runs.

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    default_target: standard
    targets:
      standard:
        provider: ollama
        model: mistral
      premium:
        provider: anthropic
        model: claude-opus-4-6
        api_key: "${ANTHROPIC_API_KEY}"
    max_tokens: 4096
```

With a `cel` middleware routing by API key tier:

```yaml
x-barbacane-middlewares:
  - name: apikey-auth
    config:
      keys: [{ key: "${FREE_KEY}", name: free }, { key: "${PAID_KEY}", name: paid }]
  - name: cel
    config:
      rules:
        - expr: 'context["apikey.name"] == "paid"'
          set_context:
            ai.target: premium
```

#### Target Resolution

The dispatcher resolves the active target using this priority chain:

1. **`ai.target` context key** — set by an upstream middleware (e.g. `cel`)
2. **`default_target`** — the named target to use when no context key is present
3. **Flat config** — the top-level `provider`/`model` fields

If none resolve, the dispatcher returns 500.

#### Provider Fallback

The `fallback` list is tried in order when the primary target returns a 5xx or a connection error. 4xx responses (client errors) are returned directly without fallback.

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    provider: openai
    model: gpt-4o
    api_key: "${OPENAI_API_KEY}"
    fallback:
      - provider: anthropic
        model: claude-sonnet-4-20250514
        api_key: "${ANTHROPIC_API_KEY}"
      - provider: ollama
        model: mistral
        base_url: "http://ollama.internal:11434"
```

#### Streaming

For OpenAI-compatible providers (OpenAI, Ollama), streaming is supported natively — set `"stream": true` in the client request body and the dispatcher uses `host_http_stream` to relay SSE chunks.

Anthropic streaming is not yet supported; when `"stream": true` is sent to an Anthropic target, the dispatcher buffers the full response and returns it non-streamed (a warning is logged).

#### Context Propagation

After a successful dispatch, the following context keys are set:

| Context Key | Description |
|-------------|-------------|
| `ai.provider` | Provider name (`openai`, `anthropic`, `ollama`) |
| `ai.model` | Model identifier used |
| `ai.prompt_tokens` | Input token count (non-streamed responses only) |
| `ai.completion_tokens` | Output token count (non-streamed responses only) |

Token counts are unavailable for streamed responses.

#### Metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `requests_total` | `provider`, `status` | Total requests per provider and HTTP status |
| `request_duration_seconds` | `provider` | Latency histogram per provider |
| `tokens_total` | `provider`, `type` (`prompt`/`completion`) | Token usage counters |
| `fallback_total` | `from_provider`, `to_provider` | Fallback events between providers |

#### Examples

**Simple OpenAI proxy:**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        provider: openai
        model: gpt-4o
        api_key: "${OPENAI_API_KEY}"
        max_tokens: 4096
```

**Anthropic with cost guardrail:**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        provider: anthropic
        model: claude-sonnet-4-20250514
        api_key: "${ANTHROPIC_API_KEY}"
        max_tokens: 2048
        timeout: 180
```

**Local Ollama for development:**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        provider: ollama
        model: mistral
```

**Multi-provider with fallback and tier-based routing:**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-middlewares:
      - name: apikey-auth
        config:
          keys:
            - { key: "${FREE_KEY}", name: free }
            - { key: "${PAID_KEY}", name: paid }
      - name: cel
        config:
          rules:
            - expr: 'context["apikey.name"] == "paid"'
              set_context:
                ai.target: premium
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        default_target: standard
        targets:
          standard:
            provider: ollama
            model: mistral
          premium:
            provider: anthropic
            model: claude-opus-4-6
            api_key: "${ANTHROPIC_API_KEY}"
        fallback:
          - provider: openai
            model: gpt-4o
            api_key: "${OPENAI_API_KEY}"
        max_tokens: 4096
```

**Azure OpenAI (custom endpoint):**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        provider: openai
        model: gpt-4o
        api_key: "${AZURE_OPENAI_KEY}"
        base_url: "https://my-resource.openai.azure.com"
```

#### Error Handling

| Status | Condition |
|--------|-----------|
| 500 Internal Server Error | No provider configured (missing `provider` and no `targets`) |
| 502 Bad Gateway | All providers (primary + fallback chain) failed |

Error responses use RFC 9457 `application/problem+json` format.

### ws-upstream

Transparent WebSocket proxy. Upgrades the client connection to WebSocket and relays frames bidirectionally to an upstream WebSocket server. The gateway handles the full lifecycle: handshake, frame relay, and connection teardown.

```yaml
x-barbacane-dispatch:
  name: ws-upstream
  config:
    url: "ws://echo.internal:8080"
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `url` | string | Yes | - | Upstream WebSocket URL (`ws://` or `wss://`) |
| `connect_timeout` | number | No | 5 | Connection timeout in seconds (0.1–300) |
| `path` | string | No | Same as operation path | Upstream path template with `{param}` substitution |

#### How It Works

1. Client sends an HTTP request with `Upgrade: websocket`
2. The plugin validates the upgrade header and connects to the upstream WebSocket server
3. On success, the gateway returns `101 Switching Protocols` to the client
4. Frames are relayed bidirectionally between client and upstream until either side closes

The middleware chain (`x-barbacane-middlewares`) runs on the initial HTTP request and can inspect/modify headers, enforce authentication, apply rate limiting, etc. Once the connection is upgraded, middleware is bypassed for individual frames.

#### Path Parameters

Path parameters from the OpenAPI spec are substituted in the `path` template:

```yaml
/ws/{room}:
  get:
    parameters:
      - name: room
        in: path
        required: true
        schema:
          type: string
    x-barbacane-dispatch:
      name: ws-upstream
      config:
        url: "ws://chat.internal:8080"
        path: "/rooms/{room}"

# Request: GET /ws/general → Upstream: ws://chat.internal:8080/rooms/general
```

#### Query String Forwarding

Query parameters from the client request are automatically forwarded to the upstream URL:

```
Client: GET /ws/echo?token=abc → Upstream: ws://echo.internal:8080/?token=abc
```

#### Examples

**Basic WebSocket proxy:**
```yaml
/ws:
  get:
    x-barbacane-dispatch:
      name: ws-upstream
      config:
        url: "ws://backend.internal:8080"
```

**With authentication and path routing:**
```yaml
/ws/{room}:
  get:
    parameters:
      - name: room
        in: path
        required: true
        schema:
          type: string
    x-barbacane-middlewares:
      - name: jwt-auth
        config:
          required: true
    x-barbacane-dispatch:
      name: ws-upstream
      config:
        url: "wss://realtime.internal"
        path: "/rooms/{room}"
        connect_timeout: 10
```

**Secure upstream (WSS):**
```yaml
/live:
  get:
    x-barbacane-dispatch:
      name: ws-upstream
      config:
        url: "wss://stream.example.com"
        connect_timeout: 15
```

#### Error Handling

| Status | Condition |
|--------|-----------|
| 400 Bad Request | Missing `Upgrade: websocket` header |
| 502 Bad Gateway | Upstream connection failed or timed out |

#### Security

- **WSS in production**: Use `wss://` for encrypted upstream connections
- **Development mode**: `ws://` URLs are allowed with `--allow-plaintext-upstream`
- **Authentication**: Apply auth middleware (jwt-auth, oidc-auth, etc.) to protect WebSocket endpoints — middleware runs on the initial upgrade request

### fire-and-forget

Forwards the incoming request to a configured upstream URL without waiting for the result, and returns an immediate static response. Useful for webhook ingestion, async job submission, and audit trails.

The outbound HTTP call is best-effort: if the upstream is unreachable or returns an error, the client still receives the configured response.

```yaml
x-barbacane-dispatch:
  name: fire-and-forget
  config:
    url: "http://backend:3000/ingest"
    response:
      status: 202
      body: '{"accepted": true}'
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `url` | string | Yes | - | Upstream URL to forward the request to |
| `timeout_ms` | integer | No | 5000 | Timeout in milliseconds for the upstream HTTP call |
| `response` | object | No | See below | Static response returned to the client |

##### Response Object

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `status` | integer | No | 202 | HTTP status code |
| `content_type` | string | No | `application/json` | Content-Type header value |
| `body` | string | No | `""` | Response body |
| `headers` | object | No | `{}` | Additional response headers |

#### How It Works

1. The request arrives (already processed by the middleware chain)
2. The dispatcher forwards method, headers, and body to the configured `url`
3. The upstream response is discarded — errors are logged as warnings
4. The configured static response is returned to the client

Because the upstream call happens synchronously in the WASM runtime, the client does wait for the HTTP call to complete (or time out). For truly decoupled async processing, consider using the `kafka` or `nats` dispatchers with a consumer behind them.

#### Examples

**Webhook ingestion:**
```yaml
/webhooks/stripe:
  post:
    x-barbacane-dispatch:
      name: fire-and-forget
      config:
        url: "https://processor.internal/stripe-events"
        timeout_ms: 3000
        response:
          status: 202
          body: '{"received": true}'
```

**Audit logging:**
```yaml
/audit/events:
  post:
    x-barbacane-dispatch:
      name: fire-and-forget
      config:
        url: "http://audit-service:9090/log"
        response:
          status: 200
          body: '{"logged": true}'
          headers:
            X-Audit-Status: accepted
```

**Minimal config (defaults to 202, empty body):**
```yaml
/notify:
  post:
    x-barbacane-dispatch:
      name: fire-and-forget
      config:
        url: "https://notification-service.internal/send"
```

#### Error Handling

The dispatcher never returns an error to the client. Upstream failures are logged but the configured static response is always returned.

| Upstream Outcome | Client Receives | Log Level |
|------------------|----------------|-----------|
| Success | Configured response | DEBUG |
| Connection error | Configured response | WARN |
| Timeout | Configured response | WARN |

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
