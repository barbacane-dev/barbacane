# Spec configuration

Barbacane extends OpenAPI and AsyncAPI specs with custom `x-barbacane-*` extensions. These tell the gateway how to route requests, apply middleware, and connect to backends.

## Extension Overview

| Extension | Location | Purpose |
|-----------|----------|---------|
| `x-barbacane-dispatch` | Operation | Route request to a dispatcher (required) |
| `x-barbacane-middlewares` | Root or Operation | Apply middleware chain |

## Server Base Path

An OpenAPI operation is served at the path of its server's URL followed by its own path, as OpenAPI defines it:

```yaml
servers:
  - url: https://api.example.com/v1
paths:
  /users:        # served at /v1/users
    get: ...
```

- The servers an operation uses are its own `servers`, or else its path item's, or else the document's. An empty list, or none, inherits; a document without servers is served at the root.
- The base path is the URL's path: `https://api.example.com/v1`, `//api.example.com/v1` and `/v1` all give `/v1`, and `https://api.example.com` gives none. A trailing `/` is ignored. Server variables take their `default`.
- With several servers whose paths differ, the operation is served under each one.
- A server URL whose path holds a variable without a default, or `..`, fails compilation with `E1004`.
- Routing conflicts (`E1010`) are checked on the served paths, so `/users` under a `/v1` server conflicts with `/v1/users` declared elsewhere.
- `http-upstream` without `path` forwards the path the client sent, base path included. Metrics and logs name the operation by its path as the spec writes it, without the base path.
- AsyncAPI servers are brokers; their paths do not affect the HTTP routes of AsyncAPI operations.

## Path Parameters

### Regular Parameters

Use `{paramName}` for single-segment parameters — the parameter captures exactly one path segment:

```yaml
paths:
  /users/{id}/orders/{orderId}:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
        - name: orderId
          in: path
          required: true
          schema:
            type: string
```

Every `in: path` parameter must be named in the path template; one the template does not name would never be captured, and fails compilation with `E1054`.

### A Parameter Within a Segment

A parameter may share its segment with literal text, before or after it:

```yaml
paths:
  /reports/{id}.pdf:          # GET /reports/42.pdf captures id=42
  /files/{id}/thumbnail.{ext}:  # GET /files/7/thumbnail.png captures id=7, ext=png
```

A segment holds at most one parameter (`{name}.{ext}` fails with `E1054`). The literal text is matched as the client sends it, and the value between must not be empty. At each segment, a static segment matches first, then the segments with literal text around a parameter (the longest literal text first), then a plain parameter, then a wildcard.

### Wildcard Parameters

Use `{paramName+}` to capture all remaining path segments as a single value, including any `/` characters:

```yaml
paths:
  /files/{bucket}/{key+}:
    get:
      parameters:
        - name: bucket
          in: path
          required: true
          schema:
            type: string
        - name: key
          in: path
          required: true
          allowReserved: true   # tells client tooling not to percent-encode '/'
          schema:
            type: string
```

A `GET /files/my-bucket/docs/2024/report.pdf` request captures `bucket=my-bucket` and `key=docs/2024/report.pdf`.

**Rules:**
- The wildcard parameter must be the **last segment** of the path
- At most one wildcard parameter per path
- Parameter names use the same characters as regular params (alphanumeric and `_`)

**`allowReserved` note:** This is advisory metadata for client generators and documentation tools — it signals that the value may contain unencoded `/` characters. Barbacane does not parse or enforce it, but including it produces correct client SDKs.

---

## Project Manifest (`barbacane.yaml`)

The manifest declares the specs folder and plugins available to the gateway:

```yaml
specs: ./specs/

plugins:
  mock:
    path: ./plugins/mock.wasm
  http-upstream:
    path: ./plugins/http-upstream.wasm
  jwt-auth:
    url: https://github.com/barbacane-dev/barbacane/releases/download/v0.7.0/jwt-auth.wasm
    sha256: abc123...
  ip-restriction:
    path: ./plugins/ip-restriction.wasm
    limits:
      memory_mib: 32
```

A key the format does not have, at the top level or in a plugin entry, fails with an error naming it.

### `specs` (optional)

Path to a folder containing spec files (relative to the manifest). All `*.yaml`, `*.yml`, and `*.json` files in this folder are discovered as specs.

Used by:
- `barbacane dev` — auto-discovers specs for compilation and file watching
- `barbacane compile` — falls back to this folder when `--spec` is not provided

### `plugins` (required)

Declares all WASM plugins used by your specs. Each plugin maps a name to a source:

- **Local path**: `path: ./plugins/name.wasm` — relative to the manifest directory
- **Remote URL**: `url: https://...` — downloaded at compile time, cached in `~/.barbacane/cache/plugins/`
- **Checksum** (optional): `sha256: ...` — integrity verification for remote plugins

An entry has `path` or `url`, not both, and `sha256` only with `url`.

### `limits` (optional)

Resource limits for the plugin's instances, each replacing the value the plugin declares in its `plugin.toml` (or the default): `memory_mib`, `fuel` and `timeout_ms`, as the [plugin guide](../contributing/plugins.md#resource-limits) describes them. A key left out keeps the plugin's value. Raise a limit when your configuration needs more than the plugin's author planned for, such as a long `ip-restriction` list; lower one to confine a plugin further. A value below 1 or above the gateway's ceiling fails compilation with `E1044`. The control plane takes the same `limits` on a project's plugin.

---

## Dispatchers

Every operation needs an `x-barbacane-dispatch` to tell Barbacane how to handle it:

```yaml
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/v2/users
```

### Dispatch Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Dispatcher plugin name |
| `config` | object | No | Plugin-specific configuration |

An entry without `config`, dispatcher or middleware, runs the plugin as `config: {}`, with its defaults. A plugin whose schema requires a field refuses it at compile time (`E1023`).

Built-in dispatchers: `mock`, `http-upstream`, `lambda`, `kafka`, `nats`, `s3`. See [Dispatchers](dispatchers.md) for configuration details.

Config values support secret references: `env://VAR_NAME` (environment variable) or `file:///path/to/secret` (file). See [Secrets](secrets.md).

---

## Middlewares

Middlewares process requests before dispatching and responses after.

### Global Middlewares

Apply to all operations:

```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 100
      window: 60
  - name: cors
    config:
      allowed_origins: ["https://app.example.com"]

paths:
  # ... all operations get these middlewares
```

### Operation Middlewares

Apply to a specific operation (runs after global middlewares):

```yaml
paths:
  /admin/users:
    get:
      security:
        - bearerAuth: []
      x-barbacane-middlewares:
        - name: oidc-auth
          config:
            issuer_url: "https://auth.example.com"
            audience: "my-api"
            required_scopes: "admin:read"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

An operation that runs an authentication middleware needs a `security` requirement naming a scheme from `components.securitySchemes`; without one the compiler stops with `E1057`. The [complete example](#complete-example) below declares `bearerAuth`.

### Middleware Merging

Operation middlewares are **merged** with global ones. Entries match by plugin name without the `@version` suffix: an operation entry replaces every global entry of its plugin, and the other globals are preserved.

```yaml
# Global: rate limit 100/min + cors
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 100
      window: 60
  - name: cors
    config:
      allow_origin: "*"

paths:
  /public/stats:
    get:
      # Override rate-limit; cors still applies from globals
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            quota: 1000
            window: 60
      # Resolved chain: cors (global) → rate-limit (operation override)
```

To drop one global middleware and keep the rest, give its name with `enabled: false`. A public endpoint under global authentication keeps rate limiting and CORS this way:

```yaml
x-barbacane-middlewares:            # global
  - name: oidc-auth
    config: { issuer_url: "env://OIDC_ISSUER_URL", audience: "my-api" }
  - name: rate-limit
    config: { quota: 300, window: 60, partition_key: client_ip }

paths:
  /webhooks/{webhookId}:
    post:
      x-barbacane-middlewares:
        - name: oidc-auth
          enabled: false            # resolved chain: rate-limit
```

`enabled: false` removes every global entry of that plugin, including stacked instances. It takes no `config` (`E1012`), it must match a global entry (`E1013`), and it is refused at the global level (`E1014`).

Use an empty array to explicitly disable all middlewares for an operation:

```yaml
x-barbacane-middlewares: []  # No middlewares at all
```

### Middleware Chain Order

1. Global middlewares (in order defined)
2. Operation middlewares (in order defined)
3. Dispatch
4. Response middlewares (reverse order)

## Complete Example

```yaml
openapi: "3.1.0"
info:
  title: E-Commerce API
  version: "2.0.0"

# Global middlewares
x-barbacane-middlewares:
  - name: correlation-id
    config:
      header_name: X-Request-ID
  - name: cors
    config:
      allowed_origins: ["https://shop.example.com"]
  - name: rate-limit
    config:
      quota: 200
      window: 60

paths:
  /health:
    get:
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"healthy"}'
      responses:
        "200":
          description: OK

  /products:
    get:
      operationId: listProducts
      x-barbacane-middlewares:
        - name: cache
          config:
            ttl: 300
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.shop.example.com"
          path: /api/products
      responses:
        "200":
          description: Product list

  /orders:
    post:
      operationId: createOrder
      security:
        - bearerAuth: []
      x-barbacane-middlewares:
        - name: oidc-auth
          config:
            issuer_url: "https://auth.shop.example.com"
            audience: "shop-api"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.shop.example.com"
          path: /api/orders
      responses:
        "201":
          description: Order created

  /orders/{orderId}/pay:
    post:
      operationId: payOrder
      security:
        - bearerAuth: []
      x-barbacane-middlewares:
        - name: oidc-auth
          config:
            issuer_url: "https://auth.shop.example.com"
            audience: "shop-api"
            required_scopes: "orders:pay"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://payments.example.com"  # Different backend!
          path: /process/{orderId}
          timeout: 45.0
      responses:
        "200":
          description: Payment processed

components:
  securitySchemes:
    bearerAuth:
      type: http
      scheme: bearer
```

## AsyncAPI Support

Barbacane supports AsyncAPI 3.x for event-driven APIs. AsyncAPI specs work similarly to OpenAPI, with channels and operations instead of paths and methods.

### Sync-to-Async Bridge Pattern

AsyncAPI `send` operations are accessible via HTTP POST requests. This enables clients to publish messages to Kafka or NATS through the gateway:

1. Client sends HTTP POST to the channel address
2. Gateway validates the message against the schema
3. Dispatcher publishes to Kafka/NATS
4. Gateway returns 202 Accepted

### Basic AsyncAPI Example

```yaml
asyncapi: "3.0.0"
info:
  title: User Events API
  version: "1.0.0"

channels:
  userEvents:
    address: /events/users
    messages:
      UserCreated:
        contentType: application/json
        payload:
          type: object
          required:
            - userId
            - email
          properties:
            userId:
              type: string
              format: uuid
            email:
              type: string
              format: email

operations:
  publishUserCreated:
    action: send
    channel:
      $ref: '#/channels/userEvents'
    x-barbacane-dispatch:
      name: kafka
      config:
        brokers: "kafka.internal:9092"
        topic: "user-events"
```

### Channel Parameters

AsyncAPI channels can have parameters (like path params in OpenAPI). AsyncAPI 3 restricts a parameter's values with `enum`, which is enforced:

```yaml
channels:
  orderEvents:
    address: /events/{region}/orders
    parameters:
      region:
        enum: [eu, us]
    messages:
      OrderPlaced:
        payload:
          type: object
```

A parameter with a `schema`, as AsyncAPI 2 writes it, is validated against that schema.

### Message Validation

Message payloads are validated against the schema before dispatch. Invalid messages receive a 400 response with validation details.

A `send` operation's body is valid when it matches one of the operation's messages (the channel's messages when the operation names none) of its content type. A payload written as a Multi Format Schema Object is applied when its `schemaFormat` is JSON Schema, AsyncAPI or OpenAPI; a payload in another format (Avro, RAML, Protobuf) is not applied, and compilation warns with `E1073`.

### HTTP Method Mapping

| AsyncAPI Action | HTTP Method |
|-----------------|-------------|
| `send` | POST |
| `receive` | GET |

### Middlewares

AsyncAPI operations support the same middleware extensions as OpenAPI:

```yaml
operations:
  publishEvent:
    action: send
    channel:
      $ref: '#/channels/events'
    # Authenticated through the security scheme the server declares.
    x-barbacane-middlewares:
      - name: oidc-auth
        config:
          issuer_url: "https://auth.example.com"
          audience: "events-api"
    x-barbacane-dispatch:
      name: kafka
      config:
        brokers: "kafka.internal:9092"
        topic: "events"
```

---

## API Lifecycle

Barbacane supports API lifecycle management through standard OpenAPI deprecation and the `x-sunset` extension.

### Marking Operations as Deprecated

Use the standard OpenAPI `deprecated` field:

```yaml
paths:
  /v1/users:
    get:
      deprecated: true
      summary: List users (deprecated, use /v2/users)
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

When a client calls a deprecated endpoint, the response includes a `Deprecation: true` header per [draft-ietf-httpapi-deprecation-header](https://datatracker.ietf.org/doc/draft-ietf-httpapi-deprecation-header/).

### Setting a Sunset Date

Use `x-sunset` to specify when an endpoint will be removed (per [RFC 8594](https://datatracker.ietf.org/doc/html/rfc8594)):

```yaml
paths:
  /v1/users:
    get:
      deprecated: true
      x-sunset: "Thu, 31 Dec 2026 23:59:59 GMT"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

The response includes a `Sunset` header per [RFC 8594](https://datatracker.ietf.org/doc/html/rfc8594), whether or not the operation is deprecated. `x-sunset` is an HTTP-date (RFC 9110), an RFC 3339 date-time, or a date (`2026-12-31`, midnight UTC); the header carries it as an HTTP-date, with the day name of its date. Any other value fails compilation with `E1018`.

### Example Response Headers

```
HTTP/1.1 200 OK
Deprecation: true
Sunset: Thu, 31 Dec 2026 23:59:59 GMT
Content-Type: application/json
```

### Best Practices

1. **Mark deprecated first**: Set `deprecated: true` before setting a sunset date
2. **Give advance notice**: Set the sunset date at least 6 months in advance
3. **Update API docs**: Include migration instructions in the operation summary or description
4. **Monitor usage**: Track calls to deprecated endpoints via metrics

---

## Validation

The compiler validates your spec:

```bash
barbacane validate --spec api.yaml
```

Errors you might see:

| Error Code | Meaning |
|------------|---------|
| E1010 | Routing conflict: two operations the router files as one route, in one spec or several. Paths compare by their non-empty segments, under their servers' base paths (`/users/` is `/users`), and an AsyncAPI `send` is a `POST`, a `receive` a `GET` |
| E1020 | Missing `x-barbacane-dispatch` on operation |
| E1031 | Plaintext `http://` or `ws://` upstream URL (use HTTPS or WSS, or `--allow-plaintext` at compile time) |
| E1054 | Invalid path template (unbalanced braces, empty param name, duplicate param, `{param+}` not last segment, multiple wildcards, more than one parameter in a segment), or a path parameter the template does not name |

### Request values

- Path, query, header and cookie parameters are checked against their `schema`, each value read as the type the schema names: under `type: integer`, `?limit=10` is the integer `10` and `?limit=ten` fails validation; under `type: boolean`, `true` and `false` are booleans.
- Arrays and objects are read the way the parameter's `style` and `explode` write them. Query and cookie parameters default to `form` with `explode: true`, path and header parameters to `simple`:

  | Style | Array | Object |
  |-------|-------|--------|
  | `form` (query, cookie) | `?id=1&id=2`; with `explode: false`, `?id=1,2` | `?role=admin&age=3` (one parameter per property); with `explode: false`, `?p=role,admin,age,3` |
  | `spaceDelimited`, `pipeDelimited` (query) | `?id=1%202`, `?id=1\|2` | `?p=role\|admin` |
  | `deepObject` (query) | | `?p[role]=admin&p[age]=3` |
  | `simple` (path, header) | `1,2` | `role,admin,age,3`; with `explode: true`, `role=admin,age=3` |
  | `label` (path) | `.1,2`; with `explode: true`, `.1.2` | `.role,admin`; with `explode: true`, `.role=admin` |
  | `matrix` (path) | `;id=1,2`; with `explode: true`, `;id=1;id=2` | `;p=role,admin`; with `explode: true`, `;role=admin` |

- A declared query parameter or cookie sent twice is refused, unless it is an array in exploded style, which sends one pair per item.
- A parameter described by `content` instead of `schema` is read as its media type: under a JSON media type the value is parsed as a JSON document. An OpenAPI 3.2 `querystring` parameter with `application/x-www-form-urlencoded` and an object schema is checked as the object the query string's fields make.
- Path parameter values are percent-decoded and keep `+` as `+`; in a query, `+` is a space.
- A parameter the gateway cannot read fails compilation (`E1004`): one without `name` or `in`, an `in` other than `path`, `query`, `header`, `cookie` and `querystring`, a `style` its location does not take, both `schema` and `content`, a `content` that names more or fewer than one media type, or a `querystring` parameter with `schema`.
- A request body is checked when its content type is JSON (`application/json`, or a `+json` type), `application/x-www-form-urlencoded` or `multipart/form-data`. A form body is checked as the object its fields make, each field read as the type its property names; a field sent twice is an array, which only an `array` property accepts. A schema for another content type (XML, plain text) is not applied, and compilation warns with `E1073`.
- In an OpenAPI 3.0 spec, `nullable: true` admits `null` in a schema that names a `type`, as OpenAPI 3.0.3 defines it: a schema without `type` is unchanged, and an `enum` that does not list `null` still refuses it. OpenAPI 3.1 and later spell it `type: [string, "null"]`.
- A property marked `readOnly: true` is not required of a request, even when its schema lists it in `required`: OpenAPI applies that requirement to responses. A request that sends the property is checked against its schema.
- A request schema may nest 32 levels deep (`E1051`) and hold 1024 properties (`E1052`), counting the definitions it references.

### References

A `$ref` stands for the object it names wherever the spec allows one: schemas, parameters, request bodies, responses and path items, and AsyncAPI channels, messages and parameters. Only references within the document (`#/...`) are followed, and one that does not resolve is an error (`E1003`).

In OpenAPI 3.1 and later, keywords written beside a schema's `$ref` apply too, as JSON Schema 2020-12 defines. OpenAPI 3.0 and AsyncAPI ignore them.

### YAML aliases

A YAML anchor can be repeated with aliases. A spec whose aliases make it stand for a value more than 16 times its own size (plus 1 MiB) is refused as a parse error (`E1002`).

## Next Steps

- [Dispatchers](dispatchers.md) - All dispatcher types and options
- [Middlewares](middlewares/index.md) - Available middleware plugins
- [CLI Reference](../reference/cli.md) - Full command options
