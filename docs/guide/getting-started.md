# Getting Started

This guide walks you through creating your first Barbacane-powered API gateway.

## Prerequisites

- Rust 1.75+ (for building from source)
- An OpenAPI 3.x specification

## Installation

### From Source

```bash
git clone https://github.com/barbacane/barbacane.git
cd barbacane
cargo build --release

# Binary is in target/release/barbacane
```

### Using Cargo (coming soon)

```bash
cargo install barbacane
```

## Your First Gateway

### 1. Create an OpenAPI Spec

Create a file called `api.yaml`:

```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

servers:
  - url: https://api.example.com
    x-barbacane-upstream:
      name: backend
      timeout: 30s

paths:
  /health:
    get:
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"ok"}'
      responses:
        "200":
          description: Health check response

  /users:
    get:
      operationId: listUsers
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/users
      responses:
        "200":
          description: List of users

  /users/{id}:
    get:
      operationId: getUser
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/users/{id}
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
            format: uuid
      responses:
        "200":
          description: User details
```

The key additions are:
- `x-barbacane-dispatch` on each operation: tells Barbacane how to handle the request

### 2. Validate the Spec

```bash
barbacane validate --spec api.yaml
```

Output:
```
✓ api.yaml is valid

validated 1 spec(s): 1 valid, 0 invalid
```

### 3. Compile to Artifact

```bash
barbacane compile --spec api.yaml --output api.bca
```

Output:
```
compiled 1 spec(s) to api.bca (3 routes)
```

The `.bca` (Barbacane Compiled Artifact) file contains:
- Compiled routing table
- Embedded source specs (for `/__barbacane/openapi`)
- Manifest with checksums

### 4. Run the Gateway

```bash
barbacane serve --artifact api.bca --listen 127.0.0.1:8080 --dev
```

Output:
```
barbacane: loaded 3 route(s) from artifact
barbacane: listening on 127.0.0.1:8080
```

### 5. Test It

```bash
# Health check (mock dispatcher)
curl http://127.0.0.1:8080/health
# {"status":"ok"}

# Gateway health
curl http://127.0.0.1:8080/__barbacane/health
# {"status":"healthy","artifact_version":1,"compiler_version":"0.1.0","routes_count":3}

# View the OpenAPI spec
curl http://127.0.0.1:8080/__barbacane/openapi
# Returns your original spec

# Try a non-existent route
curl http://127.0.0.1:8080/nonexistent
# {"error":"not found"}

# Try wrong method
curl -X POST http://127.0.0.1:8080/health
# {"error":"method not allowed"}
```

## What's Next?

- [Spec Configuration](spec-configuration.md) - Learn about all `x-barbacane-*` extensions
- [Dispatchers](dispatchers.md) - Route to HTTP backends, mock responses, and more
- [Middlewares](middlewares.md) - Add authentication, rate limiting, CORS

## Development Mode

The `--dev` flag enables:
- Verbose error messages with dispatcher details
- Detailed logging
- No production-only restrictions

For production, omit the flag:
```bash
barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```
