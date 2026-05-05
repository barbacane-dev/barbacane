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
| `fallback_key` | string | No | - | Fallback object key for SPA routing. When set, a 404 on a GET request triggers a second S3 fetch with this key (e.g. `index.html`). Query parameters are stripped from the S3 request to avoid SigV4 signature errors |
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

#### SPA Fallback

Set `fallback_key` to serve a default object (typically `index.html`) when S3 returns a 404 on a GET request. This enables client-side routing for single-page applications:

```yaml
x-barbacane-dispatch:
  name: s3
  config:
    region: us-east-1
    bucket: my-spa
    access_key_id: env://AWS_ACCESS_KEY_ID
    secret_access_key: env://AWS_SECRET_ACCESS_KEY
    fallback_key: index.html
```

`GET /dashboard/settings` → S3 returns 404 → re-fetches `index.html` from the same bucket.

When `fallback_key` is set, query parameters are automatically stripped from the S3 request. Frontend query strings (e.g. `?code=...&state=...` from OIDC callbacks) belong to the client-side router — forwarding them to S3 would invalidate the SigV4 signature and prevent the 404→fallback path from triggering.

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

**SPA with OIDC authentication (fallback to `index.html`):**
```yaml
paths:
  /{path+}:
    get:
      parameters:
        - { name: path, in: path, required: true, allowReserved: true, schema: { type: string } }
      x-barbacane-dispatch:
        name: s3
        config:
          region: us-east-1
          bucket: my-spa
          access_key_id: env://AWS_ACCESS_KEY_ID
          secret_access_key: env://AWS_SECRET_ACCESS_KEY
          fallback_key: index.html
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

AI gateway dispatcher (ADR-0030). One plugin serves three OpenAI-compatible surfaces — Chat Completions, Responses, and Models — across OpenAI, Anthropic, and Ollama upstreams. Supports glob-based routing on the client's `model`, named targets for `cel`-driven policy, catalog `allow`/`deny`, provider fallback on 5xx, and token-count propagation into request context for downstream middlewares.

> **Easy button:** Barbacane ships [`schemas/ai-gateway.yaml`](https://github.com/barbacane-dev/barbacane/blob/main/schemas/ai-gateway.yaml) with all three operations pre-bound. Drop it into your `specs/` folder, set `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `OLLAMA_BASE_URL`, and you have a working AI gateway. See the [AI Gateway quickstart](ai-gateway.md).

#### Caller-owned model

The `model` identifier is part of the **client's** contract, not the gateway's (ADR-0030 §0). Gateway config declares **providers** (where to go, with what credentials), never an authoritative model list. The client's `model` field is passed to the upstream verbatim.

- A request that omits `model` (or sends an empty string) is rejected with `400 problem+json` and `code: "model_required"`.
- The legacy `model:` field on dispatcher config is **rejected** at lint time by `vacuum:barbacane` (`Unknown config field "model" for dispatcher "ai-proxy"`) and at runtime by the WASM plugin's deserializer. If you're upgrading from a pre-ADR-0030 spec, delete `model:` from every `ai-proxy` config block.

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    provider: openai
    api_key: "env://OPENAI_API_KEY"
```

#### Protocol surfaces

The dispatcher selects behavior from `req.path`:

| Path | Surface | Notes |
|---|---|---|
| `POST /v1/chat/completions` | Chat Completions | Translated for Anthropic; passthrough for OpenAI/Ollama. |
| `POST /v1/responses` | Responses API | Stateless (ADR-0030 §2). `previous_response_id` returns 400; `store: true` is permissive but emits `Warning: 299`. Synthetic id is `resp_<uuid-v7>`. Ollama not supported. |
| `GET /v1/models` | Model catalog aggregator | Walks every unique provider, queries each upstream's `/v1/models` (or `/api/tags` for Ollama). Partial-failure response on per-provider error. |
| _other_ | Routes through Chat Completions for backward compatibility with operator-defined custom paths. | |

#### Resolution chain

For each request the dispatcher resolves a target via this 4-step ladder:

1. **`ai.target` context key** — set by an upstream middleware (typically `cel`). Resolves to `targets[<name>]`.
2. **Routes** — first `routes[].pattern` glob that matches the client's `model` wins.
3. **`default_target`** — when no context key is present and no route matched.
4. **Flat config** — the top-level `provider` field.

If `routes` is configured but no entry matched and there's no fallthrough, the dispatcher returns `400 problem+json` with `code: "no_route"`. If nothing is configured at all, it returns `500`.

The dispatcher emits `resolution_total{resolution=context|routes|default|flat}` so operators can debug *"why did my request go to provider X?"*.

#### Routes table — glob-based dynamic model routing

The headline ADR-0030 §3 feature. Each entry binds a glob `pattern` (`*`, `?`, `[...]`) to a provider; first match wins.

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    routes:
      - pattern: "claude-*"
        provider: anthropic
        api_key: "env://ANTHROPIC_API_KEY"
      - pattern: "gpt-*"
        provider: openai
        api_key: "env://OPENAI_API_KEY"
      - pattern: "o[1-4]*"           # OpenAI reasoning models
        provider: openai
        api_key: "env://OPENAI_API_KEY"
      - pattern: "*"                 # catch-all → local Ollama
        provider: ollama
        base_url: "env://OLLAMA_BASE_URL"
```

Patterns are case-sensitive, anchored full-string matches via the `globset` crate. Glob syntax (`*`, `?`, `[...]`) is enforced at lint time by the plugin's JSON schema; brace alternation (`{a,b}`) and regex constructs are rejected.

#### Catalog policy — `allow` / `deny`

Optional glob lists on a target restrict which models the client may dispatch. Evaluated **after** target resolution, against the client's `model` field. A denied model returns `403 problem+json` with `code: "model_not_permitted"` and **does not fall through** to another route — silent escalation to a different provider would be a security hole.

```yaml
routes:
  - pattern: "claude-*"
    provider: anthropic
    api_key: "env://ANTHROPIC_API_KEY"
    deny: ["claude-opus-*"]          # cost guardrail: no Opus on this gateway
  - pattern: "gpt-*"
    provider: openai
    api_key: "env://OPENAI_API_KEY"
    allow: ["gpt-4o", "gpt-4o-mini"] # only these from OpenAI
```

> **Subtlety (ADR-0030 §3):** catalog policy applies on **every resolution path** that produces a target carrying `allow`/`deny`. A `cel` misconfig that sets `ai.target: anthropic-tier` cannot leak a denied model — the deny on that target still fires.

`allow` / `deny` are also valid on `targets.<name>.{allow,deny}` for the named-target / context-driven path.

##### `allow` no-fallthrough escape hatch

```yaml
routes:
  - pattern: "gpt-*"
    provider: openai
    allow: ["gpt-4o", "gpt-4o-mini"]
  - pattern: "*"
    provider: ollama
    base_url: "env://OLLAMA_BASE_URL"
```

A request for `gpt-3.5-turbo` returns `403`, **not** the catch-all ollama route — the operator's `allow` list is a promise, not a filter. To send anything-else-from-OpenAI to ollama instead, tighten the pattern so non-matching models miss the route entirely:

```yaml
routes:
  - pattern: "gpt-4o*"           # only matches what allow would have permitted
    provider: openai
  - pattern: "*"
    provider: ollama
    base_url: "env://OLLAMA_BASE_URL"
```

Now `gpt-3.5-turbo` falls through to ollama; `gpt-4o-mini` still routes to OpenAI.

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `provider` | string | If no `targets`/`routes` | - | Provider type: `openai`, `anthropic`, or `ollama` |
| `api_key` | string | No | - | API key. Supports `env://VAR` substitution. Omit for Ollama |
| `base_url` | string | No | Provider default | Custom endpoint URL (Azure OpenAI, self-hosted vLLM, remote Ollama, etc.) |
| `timeout` | integer | No | 120 | LLM request timeout (seconds) for chat completions and responses |
| `models_timeout_ms` | integer | No | 5000 | Per-provider timeout (**milliseconds**) for the `/v1/models` aggregator only — separate from `timeout` because discovery doesn't need 120s of patience |
| `max_tokens` | integer | No | - | Default `max_tokens` injected when the client omits it. Required for Anthropic (enforced by their API). Cost guardrail for OpenAI/Ollama |
| `routes` | array | No | `[]` | Glob-pattern routing entries (see above) |
| `fallback` | array | No | `[]` | Ordered list of fallback providers tried on 5xx / connection error |
| `targets` | object | No | - | Named provider targets selectable via `ai.target` context |
| `default_target` | string | No | - | Target to use when no `ai.target` context key is set and no route matched |

`TargetConfig` (entries in `targets` map and `fallback` array): `provider` (required), `api_key`, `base_url`, `allow`, `deny`. Same shape as `routes[]` minus the `pattern`.

**Provider defaults:**

| Provider | Default Base URL |
|----------|-----------------|
| `openai` | `https://api.openai.com` |
| `anthropic` | `https://api.anthropic.com` |
| `ollama` | `http://localhost:11434` |

#### Provider fallback

The `fallback` list is tried in order when the primary target returns a 5xx or a connection error. 4xx responses (client errors) — including `model_not_permitted` (403) and `model_required` (400) — are returned directly without fallback.

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    provider: openai
    api_key: "env://OPENAI_API_KEY"
    fallback:
      - provider: anthropic
        api_key: "env://ANTHROPIC_API_KEY"
      - provider: ollama
        base_url: "http://ollama.internal:11434"
```

#### Streaming

OpenAI-compatible providers (OpenAI, Ollama) stream natively via `host_http_stream` — set `"stream": true` in the client request body.

Anthropic streaming is buffered: the dispatcher waits for the full response and returns it non-streamed (a warning is logged). True SSE translation is deferred per ADR-0024 / ADR-0030 §2.

#### Responses API specifics

`POST /v1/responses` is **stateless-only** (ADR-0030 §2):

| Body field | Behavior |
|---|---|
| `previous_response_id: <non-null>` | `400 previous_response_id_not_supported` — the gateway has no session storage. |
| `store: true` (or absent — OpenAI's server-side default) | Permissive: request flows statelessly. The response carries `Warning: 299 - "store ignored; gateway is stateless, see ADR-0030"` and increments `barbacane_plugin_ai_proxy_responses_store_downgrades_total`. |
| `store: false` | No warning, no counter. |
| `model` missing / empty | `400 model_required`. |

The synthetic `id` on the response is `resp_<uuid-v7>` (time-ordered, opaque). The OpenAI passthrough rewrites the upstream's id to a synthetic one too — clients only ever see gateway-issued ids, so the stateless contract is uniform.

For Anthropic, `input[]` items translate to Messages API `content` blocks: `input_text` / `input_image` → `text` / `image`; `function_call` + `function_call_output` → `tool_use` + `tool_result`. `reasoning` items are dropped (Anthropic doesn't accept client-supplied reasoning input); the response carries `Warning: 299 - "reasoning items dropped..."` and increments `barbacane_plugin_ai_proxy_responses_reasoning_dropped_total`.

For Ollama: `400 responses_not_supported_for_provider` — Ollama's OpenAI-compat surface is Chat Completions only as of 2026-04.

#### Models endpoint specifics

`GET /v1/models` walks every unique `(provider, base_url)` declared in `routes`, `targets`, and the flat config; queries each upstream's `/v1/models` (or `/api/tags` for Ollama, then translates the shape); returns OpenAI-compatible `{ object: "list", data: [...] }`.

Per-provider failures (5xx, timeout, connection error) are non-fatal: response is `200 OK` with `partial: true` and a `warnings: [{provider, status, detail}]` array. Each failure increments `barbacane_plugin_ai_proxy_models_provider_failures_total{provider}`.

The aggregator is sequential — one hung upstream blocks subsequent ones. The `models_timeout_ms` config (default 5 s) caps each provider's worst-case contribution to the aggregate response.

#### Context propagation

After a successful Chat Completions / Responses dispatch, these context keys are written for downstream middlewares:

| Context Key | Description |
|-------------|-------------|
| `ai.provider` | Provider name (`openai`, `anthropic`, `ollama`) |
| `ai.model` | Caller-supplied model identifier (ADR-0030 §0) |
| `ai.prompt_tokens` | Input token count (non-streamed responses only) |
| `ai.completion_tokens` | Output token count (non-streamed responses only) |

Token counts are unavailable for streamed responses.

#### Composing with AI middlewares

Four middlewares (see [AI Gateway](middlewares/ai-gateway.md) in the middlewares guide) consume the context keys above and add guardrails around the dispatcher:

| Middleware | Role | Context it reads |
|---|---|---|
| [`ai-prompt-guard`](middlewares/ai-gateway.md#ai-prompt-guard) | Validate prompts before dispatch | `ai.policy` (profile selection) |
| [`ai-token-limit`](middlewares/ai-gateway.md#ai-token-limit) | Token-based sliding-window rate limiting | `ai.policy`, `ai.prompt_tokens`, `ai.completion_tokens` |
| [`ai-cost-tracker`](middlewares/ai-gateway.md#ai-cost-tracker) | Per-request USD cost metric | `ai.provider`, `ai.model`, `ai.prompt_tokens`, `ai.completion_tokens` |
| [`ai-response-guard`](middlewares/ai-gateway.md#ai-response-guard) | PII redaction + blocked-pattern scanning | `ai.policy` (profile selection) |

All four adopt the same **named-profile + CEL** composition: each plugin defines named profiles; a `cel` middleware upstream writes `ai.policy` (and/or `ai.target`) into context to select the active profile. One CEL decision (e.g. consumer tier) can fan out to provider routing, prompt strictness, token budget, and redaction strictness.

For consumer-policy on the model itself (e.g. *"free-tier can't use `gpt-4o`"*), use `cel` with `request.body_json` access and `on_match.deny`:

```yaml
x-barbacane-middlewares:
  - name: cel
    config:
      expression: "request.body_json.model.startsWith('gpt-4o') && request.claims.tier != 'premium'"
      on_match:
        deny:
          status: 403
          code: model_not_permitted
```

See [Authorization](middlewares/authorization.md#cel) for the full `cel` reference.

#### Metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `requests_total` | `provider`, `status` | Total requests per provider and HTTP status |
| `request_duration_seconds` | `provider` | Latency histogram per provider |
| `tokens_total` | `provider`, `type` (`prompt`/`completion`) | Token usage counters |
| `fallback_total` | `from_provider`, `to_provider` | Fallback events between providers |
| `resolution_total` | `resolution` (`context`/`routes`/`default`/`flat`) | Which step of the resolution chain picked the target |
| `responses_store_downgrades_total` | `provider` | Requests where `store ≠ false` was downgraded to stateless |
| `responses_reasoning_dropped_total` | `provider` | Responses translations that dropped a `reasoning` input item |
| `models_provider_failures_total` | `provider` | `/v1/models` aggregator per-provider failures |

Metric names are prefixed by the host runtime as `barbacane_plugin_ai_proxy_<name>`.

#### Error handling

All errors are RFC 9457 `application/problem+json`.

| Status | Code | Condition |
|---|---|---|
| 400 | `model_required` | Request body missing a non-empty `model` field |
| 400 | `no_route` | `routes` configured but no entry matched and no fallthrough |
| 400 | `previous_response_id_not_supported` | Stateful Responses feature; gateway is stateless (ADR-0030 §2) |
| 400 | `responses_not_supported_for_provider` | `POST /v1/responses` against an Ollama target |
| 403 | `model_not_permitted` | The resolved target's `allow`/`deny` rejected the model |
| 405 | `method_not_allowed` | `/v1/models` accepts only `GET` |
| 500 | (no code) | Misconfiguration: no `provider`, no `routes`, no `targets`, no `default_target` |
| 502 | (no code) | All providers (primary + fallback chain) failed |

#### Examples

**Simple OpenAI proxy:**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        provider: openai
        api_key: "env://OPENAI_API_KEY"
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
        api_key: "env://ANTHROPIC_API_KEY"
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
```

**Routes-driven multi-provider (ADR-0030 §3):**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        routes:
          - pattern: "claude-*"
            provider: anthropic
            api_key: "env://ANTHROPIC_API_KEY"
            deny: ["claude-opus-*"]      # catalog policy
          - pattern: "gpt-*"
            provider: openai
            api_key: "env://OPENAI_API_KEY"
          - pattern: "*"
            provider: ollama
            base_url: "env://OLLAMA_BASE_URL"
        max_tokens: 4096
```

**Tier-based context routing (`cel` writes `ai.target`):**
```yaml
/v1/chat/completions:
  post:
    x-barbacane-middlewares:
      - name: apikey-auth
        config:
          keys:
            - { key: "env://FREE_KEY", name: free }
            - { key: "env://PAID_KEY", name: paid }
      - name: cel
        config:
          expression: "request.headers['x-auth-consumer'] == 'paid'"
          on_match:
            set_context:
              ai.target: premium
    x-barbacane-dispatch:
      name: ai-proxy
      config:
        default_target: standard
        targets:
          standard:
            provider: ollama
          premium:
            provider: anthropic
            api_key: "env://ANTHROPIC_API_KEY"
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
        api_key: "env://AZURE_OPENAI_KEY"
        base_url: "https://my-resource.openai.azure.com"
```

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
