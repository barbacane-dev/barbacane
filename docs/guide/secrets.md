# Secrets Management

Barbacane supports secure secret management to keep sensitive values like API keys, tokens, and passwords out of your specs and artifacts.

## Overview

Instead of hardcoding secrets in your OpenAPI specs, you reference them using special URI schemes. The gateway resolves these references at startup, before handling any requests.

**Key principles:**
- No secrets in specs
- No secrets in artifacts
- No secrets in Git
- Secrets resolved at gateway startup

## Secret Reference Formats

### Environment Variables

Use `env://` to reference environment variables:

```yaml
x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      client_id: my-client
      client_secret: "env://OAUTH2_CLIENT_SECRET"
```

At startup, the gateway reads the `OAUTH2_CLIENT_SECRET` environment variable and substitutes its value.

### File-based Secrets

Use `file://` to read secrets from files:

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      secret: "file:///etc/secrets/jwt-signing-key"
```

The gateway reads the file content, trims whitespace, and uses the result. This works well with:
- Kubernetes Secrets (mounted as files)
- Docker secrets
- HashiCorp Vault Agent (file injection)

## Examples

### OAuth2 Authentication Middleware

```yaml
x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      introspection_endpoint: https://auth.example.com/introspect
      client_id: my-api-client
      client_secret: "env://OAUTH2_SECRET"
      timeout: 5.0
```

Run with:
```bash
export OAUTH2_SECRET="super-secret-value"
barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

### HTTP Upstream with API Key

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: https://api.provider.com/v1
    headers:
      Authorization: "Bearer env://UPSTREAM_API_KEY"
```

### JWT Auth with File-based Key

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      public_key: "file:///var/run/secrets/jwt-public-key.pem"
      issuer: https://auth.example.com
      audience: my-api
```

### Multiple Secrets

You can use multiple secret references in the same config:

```yaml
x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      introspection_endpoint: "env://AUTH_SERVER_URL"
      client_id: "env://CLIENT_ID"
      client_secret: "env://CLIENT_SECRET"
```

## Startup Behavior

### Resolution Timing

Secrets are resolved **once** at gateway startup:

1. Gateway loads the artifact
2. Gateway scans all dispatcher and middleware configs for secret references
3. Each reference is resolved (env var read, file read)
4. Resolved values replace the references in memory
5. HTTP server starts listening

If any secret cannot be resolved, the gateway refuses to start.

### Exit Codes

| Exit Code | Meaning |
|-----------|---------|
| 0 | Normal shutdown |
| 1 | General error |
| 11 | Plugin hash mismatch |
| **13** | **Secret resolution failure** |

When exit code 13 occurs, the error message indicates which secret failed:

```
error: failed to resolve secrets: environment variable not found: OAUTH2_SECRET
```

### Missing Secrets

The gateway fails fast on missing secrets:

```bash
# Missing env var
$ barbacane serve --artifact api.bca --listen 0.0.0.0:8080
error: failed to resolve secrets: environment variable not found: API_KEY
$ echo $?
13

# Missing file
$ barbacane serve --artifact api.bca --listen 0.0.0.0:8080
error: failed to resolve secrets: file not found: /etc/secrets/api-key
$ echo $?
13
```

This fail-fast behavior ensures the gateway never starts in an insecure state.

## Supported Schemes

| Scheme | Example | Status |
|--------|---------|--------|
| `env://` | `env://API_KEY` | Supported |
| `file://` | `file:///etc/secrets/key` | Supported |
| `vault://` | `vault://secret/data/api-keys` | Planned |
| `aws-sm://` | `aws-sm://prod/api-key` | Planned |
| `k8s://` | `k8s://namespace/secret/key` | Planned |

## Best Practices

### Development

Use environment variables with `.env` files (not committed to Git):

```bash
# .env (add to .gitignore)
OAUTH2_SECRET=dev-secret-value
API_KEY=dev-api-key
```

```bash
# Load and run
source .env
barbacane serve --artifact api.bca --listen 127.0.0.1:8080 --dev
```

### Production

Use your platform's secret management:

**Docker:**
```bash
docker run -e OAUTH2_SECRET="$OAUTH2_SECRET" barbacane serve ...
```

**Kubernetes:**
```yaml
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: gateway
      env:
        - name: OAUTH2_SECRET
          valueFrom:
            secretKeyRef:
              name: api-secrets
              key: oauth2-secret
```

**Kubernetes with file-based secrets:**
```yaml
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: gateway
      volumeMounts:
        - name: secrets
          mountPath: /etc/secrets
          readOnly: true
  volumes:
    - name: secrets
      secret:
        secretName: api-secrets
```

Then use `file:///etc/secrets/key-name` in your spec.

### Secret Rotation

For secrets that need rotation:

1. Update the secret value in your secret store
2. Restart the gateway (rolling restart in Kubernetes)

The gateway does not hot-reload secrets. This simplifies the security model and avoids race conditions.

## Troubleshooting

### "environment variable not found"

```
error: failed to resolve secrets: environment variable not found: MY_SECRET
```

**Solutions:**
- Verify the env var is set: `echo $MY_SECRET`
- Ensure the env var is exported: `export MY_SECRET=value`
- Check for typos in the reference: `env://MY_SECRET`

### "file not found"

```
error: failed to resolve secrets: file not found: /path/to/secret
```

**Solutions:**
- Verify the file exists: `ls -la /path/to/secret`
- Check file permissions: the gateway process must be able to read it
- Use absolute paths starting with `/`

### "unsupported secret scheme"

```
error: failed to resolve secrets: unsupported secret scheme: vault
```

This means you're using a scheme that isn't implemented yet. Currently only `env://` and `file://` are supported.

## Security Considerations

1. **Never commit secrets to Git** - Use `.gitignore` for `.env` files
2. **Rotate secrets regularly** - Plan for secret rotation via gateway restarts
3. **Use least privilege** - Only grant the gateway access to secrets it needs
4. **Audit secret access** - Use your secret store's audit logging
5. **Encrypt at rest** - Ensure your secret storage encrypts secrets
