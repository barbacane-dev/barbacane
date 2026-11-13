# Stability policy

Barbacane follows [semantic versioning](https://semver.org). From 1.0.0, what this page lists keeps working across 1.x releases: a spec, a plugin, an artifact, a script or a dashboard built against 1.x keeps working on a later 1.x, except where this page says otherwise.

Pre-releases (`1.0.0-rc.1` and the like) are outside the promise: anything may change before the release they lead to.

## What is covered

### Specs

- The `x-barbacane-*` extensions ([reference](extensions.md)): their names, where they go, and what they mean.
- Each official plugin's configuration, as its `config-schema.json` describes it: field names, types, defaults and meaning.
- What the compiler accepts: a spec and a `barbacane.yaml` that compile with one 1.x release compile with a later one.

A minor release may add extensions, plugin fields, and compiler warnings. It does not turn an accepted spec into an error, except for a security fix (see [Exceptions](#exceptions)).

### Artifacts

A 1.x gateway loads every artifact compiled by a 1.x compiler up to its own version. An artifact compiled by a newer compiler than the gateway may use a format the gateway does not read, and is refused with a message saying to upgrade the gateway. Upgrading the gateway never requires recompiling; only 2.0 may.

The [artifact format](artifact.md) itself is not an interface: read artifacts with `barbacane`, not by unpacking them.

### Plugins

A plugin built against `barbacane-plugin-sdk` 1.x runs on any later 1.x gateway. Covered:

- the host functions, their signatures and results, and the capability names that grant them;
- the `plugin.toml` format, including `[capabilities]` and `[limits]`, the defaults a plugin gets when it declares no limit, and the ceilings the gateway enforces;
- the plugin contract: `init(config)` runs once per plugin and config, each request runs in a fresh instance starting from the memory `init` left, and nothing carries from one request to the next ([plugin guide](../contributing/plugins.md#instances-and-state));
- the WASI functions a plugin may import ([plugin guide](../contributing/plugins.md#wasi));
- the public Rust API of `barbacane-plugin-sdk` and the attributes of `barbacane-plugin-macros`, under semantic versioning. The SDK's data types and error enums are `#[non_exhaustive]`, so a minor release may add a field or a variant: build the types with their constructors and give a `match` on the enums a `_` arm.

A minor release may add host functions, capabilities and SDK items.

### The control plane and the CLI

- The control-plane REST API, as its OpenAPI spec describes it (served at `/openapi`): paths, methods, parameters, request and response fields, status codes, and the problem types it answers with. A minor release may add operations, optional parameters and response fields, so a client ignores fields it does not know.
- The `barbacane` and `barbacane-control` commands and flags ([CLI reference](cli.md)), their exit codes, and `--output json`. Table output is for people and may change.
- The data plane's reserved endpoints ([reference](endpoints.md)) and the admin API: paths and response fields.

### Configuration, metrics and errors

- `BARBACANE_*` environment variables ([reference](configuration.md)): names and meaning.
- Prometheus metrics ([observability](../guide/observability.md#metrics)): names, types and label names. A minor release may add metrics and labels, and label values (a new `outcome`, for one).
- Compiler error and warning codes (`E1xxx`, [CLI reference](cli.md#error-codes)) and the problem types the gateway answers with (`urn:barbacane:error:*`): a code keeps its meaning and is not reused.

## What is not covered

- The Rust APIs of every other crate: `barbacane`, `barbacane-compiler`, `barbacane-wasm`, `barbacane-telemetry`, `barbacane-control` and `barbacane-test`. They are published so the binaries can be installed with `cargo install`, not as libraries.
- The artifact's internal layout, beyond what [Artifacts](#artifacts) promises.
- Log message text, and the fields of log lines other than `timestamp`, `level`, `target`, `message`, `trace_id` and `request_id`.
- Performance characteristics, resource use, and defaults this page does not name.
- Anything the documentation marks as experimental.

## Deprecation

A covered item is deprecated in a minor release, which:

- lists it under **Deprecated** in the [CHANGELOG](https://github.com/barbacane-dev/barbacane/blob/main/CHANGELOG.md), with what to use instead;
- marks it in the documentation;
- has the compiler or the gateway warn when it is used, where they can see it.
- carries `#[deprecated(since, note)]` when it is a Rust item of the plugin SDK or the macros, so a plugin build warns.

A deprecated item may be removed two minor releases later at the earliest: deprecated in 1.2, removed in 1.4 or after. Its removal is listed under **Removed** in the CHANGELOG.

## Security fixes

Security fixes are released for the latest 1.x minor release only. An advisory names the affected versions and the release that fixes them. 0.12 receives security fixes until 1.0.0 is released.

## Exceptions

A minor or patch release may break something covered here when that is the only way to fix a security vulnerability, or when the documented behaviour contradicts what the gateway was built to enforce (a validation that let through what the spec forbids, for one). Such a change is listed in the CHANGELOG with the reason and what to do.
