# Getting started

From an empty directory to a running gateway, then the same project run as in production.

## Install

Install `barbacane` from the [installation guide](installation.md). For macOS on Apple Silicon:

```bash
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-aarch64-apple-darwin
chmod +x barbacane-aarch64-apple-darwin
sudo mv barbacane-aarch64-apple-darwin /usr/local/bin/barbacane
barbacane --version
```

## Create a project

```bash
barbacane init my-api --fetch-plugins
cd my-api
```

This creates:

- `barbacane.yaml`: the project manifest, naming the `specs/` folder and the plugins;
- `specs/api.yaml`: an OpenAPI spec with a few example operations;
- `plugins/mock.wasm` and `plugins/http-upstream.wasm`: the official mock and HTTP proxy dispatchers, downloaded from the release of your `barbacane` version;
- `.gitignore`: ignores compiled artifacts (`*.bca`) and signing keys (`*.pk8`).

`barbacane init my-api --template minimal --fetch-plugins` starts from a spec with a single operation instead.

## Run it

```bash
barbacane dev
```

```
barbacane dev: loaded barbacane.yaml (1 spec(s), 2 plugin(s))
barbacane dev: compiled 4 route(s) in 63ms
barbacane dev: listening on http://0.0.0.0:8080
barbacane dev: admin API on http://127.0.0.1:8081
barbacane dev: watching /path/to/my-api/barbacane.yaml, ./specs, ./plugins/mock.wasm, ./plugins/http-upstream.wasm
```

`barbacane dev` compiles every spec in `specs/`, serves them, and recompiles and reloads when a spec, the manifest or a plugin changes. It loads its artifacts unsigned, and logs a warning saying so.

In another terminal:

```bash
curl http://localhost:8080/health
# {"status": "ok"}

curl http://localhost:8080/users/0b7c6f2e-8d5a-4c1e-9f3b-2a6d8e4c1b9f
# {"id": "0b7c6f2e-8d5a-4c1e-9f3b-2a6d8e4c1b9f", "name": "John Doe"}
```

The spec is enforced before any plugin runs. The `id` path parameter is declared as a UUID, and `POST /users` requires a `name` of at least one character and an `email`:

```bash
curl http://localhost:8080/users/not-a-uuid
# {"type":"urn:barbacane:error:validation-failed","title":"Request validation failed","status":400,
#  "detail":"invalid parameter 'id' in path: \"not-a-uuid\" is not a \"uuid\"", ...}

curl -X POST http://localhost:8080/users -H 'content-type: application/json' -d '{"name": ""}'
# {"type":"urn:barbacane:error:validation-failed","title":"Request validation failed","status":400,
#  "detail":"invalid request body: \"\" is shorter than 1 character; \"email\" is a required property", ...}

curl http://localhost:8080/nope
# {"status":404,"title":"Not Found","type":"urn:barbacane:error:not-found"}
```

Errors are RFC 9457 problem details (`application/problem+json`).

## Change it

Each operation names the plugin that answers it in `x-barbacane-dispatch`. In `specs/api.yaml`:

```yaml
  /health:
    get:
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status": "ok"}'
          headers:
            Content-Type: application/json
```

Change the body to `'{"status": "ready"}'` and save: `barbacane dev` recompiles, and `curl http://localhost:8080/health` answers `{"status": "ready"}`.

To send an operation to a real backend, dispatch it to `http-upstream`:

```yaml
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: https://api.example.com
```

Middlewares (authentication, rate limiting, CORS, caching) are listed in `x-barbacane-middlewares` on an operation or at the root of the spec, and declared in `barbacane.yaml` like the dispatchers. See [spec configuration](spec-configuration.md), [dispatchers](dispatchers.md) and [middlewares](middlewares/index.md).

Check a spec without serving it:

```bash
barbacane validate --spec specs/api.yaml
# ✓ specs/api.yaml is valid
#
# validated 1 spec(s): 1 valid, 0 invalid
```

## Run it in production

In production the gateway serves a compiled artifact, and only one signed by a key it pins. Generate the key once, and keep it out of the repository (`.gitignore` already ignores `*.pk8`):

```bash
barbacane key generate --out signing.pk8
# wrote signing.pk8: keep it secret, and compile with BARBACANE_SIGNING_KEY=signing.pk8
# pin this public key on data planes as BARBACANE_TRUSTED_PUBKEY:
# 87f3b949...
```

Compile the project into a signed artifact:

```bash
BARBACANE_SIGNING_KEY=signing.pk8 barbacane compile -m barbacane.yaml -o api.bca
# compiled 1 spec(s) to api.bca (4 routes, 1 plugin(s) bundled)
```

The artifact holds the routes, the specs and the plugins the specs use. Serve it, pinning the public key:

```bash
BARBACANE_TRUSTED_PUBKEY=$(barbacane key public --key signing.pk8) \
  barbacane serve --artifact api.bca --listen 0.0.0.0:8080
# barbacane: loaded 4 route(s) from artifact
# barbacane: listening on http://0.0.0.0:8080
# barbacane: admin API on http://127.0.0.1:8081
```

Without `BARBACANE_TRUSTED_PUBKEY`, `barbacane serve` refuses to start; `--allow-unsigned` serves unsigned artifacts instead. The [configuration reference](../reference/configuration.md#artifact-signing-quickstart) covers key rotation. To run the gateway as a container, see the [installation guide](installation.md#container-images).

## Observe it

The admin API serves Prometheus metrics, on `127.0.0.1:8081` by default:

```bash
curl http://127.0.0.1:8081/metrics
```

Logs are JSON by default; `--log-format pretty` reads better in a terminal, and `--otlp-endpoint` exports traces to an OpenTelemetry collector. See [observability](observability.md).

## Manage gateways centrally

To compile and deploy from one place, run the control plane and drive it with `barbacane control`:

```bash
barbacane control push --project <ID> specs/api.yaml
barbacane control deploy --project <ID> --wait
```

See the [control plane guide](control-plane.md).

## Next

- [Spec configuration](spec-configuration.md): every `x-barbacane-*` extension
- [Dispatchers](dispatchers.md): proxying, mocks, messaging, cloud services
- [Middlewares](middlewares/index.md): authentication, rate limiting, CORS, caching
- [Secrets](secrets.md): API keys and passwords outside the spec
- [Observability](observability.md): metrics, logs and traces
