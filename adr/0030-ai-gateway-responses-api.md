# ADR-0030: AI Gateway — OpenAI Responses API Support & Dynamic Model Routing

**Status:** Proposed
**Date:** 2026-04-22

## Context

[ADR-0024](0024-ai-gateway-plugin.md) established the `ai-proxy` dispatcher with a unified OpenAI-compatible API and translation to Anthropic Messages. That decision pinned the client-facing contract to **OpenAI Chat Completions** (`POST /v1/chat/completions`). Two gaps have surfaced since:

### Gap 1 — Chat Completions is not the only OpenAI format clients use

OpenAI's **Responses API** (`POST /v1/responses`, GA March 2025) is a distinct protocol from Chat Completions. It is the format used by:

- `codex-cli` (OpenAI's coding agent)
- The OpenAI Agents SDK
- Most new OpenAI-native tooling released after early 2025

Key differences from Chat Completions:

| | Chat Completions | Responses API |
|---|---|---|
| Request shape | `messages[]` (role + string/array content) | `input[]` (typed items: `input_text`, `input_image`, `function_call`, `function_call_output`, `reasoning`…) |
| Tool calls | Inlined in assistant messages, correlated via `tool_call_id` | First-class items in `input` / `output` arrays |
| Structured outputs | JSON mode / JSON schema on response | Native typed output items |
| State model | Stateless (client sends full history) | **Optionally stateful** (`store: true` + `previous_response_id` chains turns server-side) |

Crucially, the Responses item model is a **closer match to Anthropic's Messages API content blocks** than Chat Completions is. Translating Responses → Anthropic is a more natural mapping: typed items map to content blocks quasi 1-to-1, tool use semantics align, and reasoning traces have a direct counterpart. The Chat Completions → Messages translation in `ai-proxy` v1 has known rough edges around multi-turn tool use precisely because of this impedance mismatch.

Users who need to point OpenAI-format clients at Anthropic models today (e.g. running `codex-cli` against Claude) cannot use Barbacane — they have to run a separate proxy. This has been reported by contributors and is the motivation for this ADR.

### Gap 2 — Model identifiers are pinned in gateway config

The current `ai-proxy` config requires each target to declare a `model` string:

```yaml
targets:
  premium:
    provider: anthropic
    model: claude-opus-4-6       # pinned here
```

The dispatcher honors the client's `model` field if present ([plugins/ai-proxy/src/lib.rs:457-460](../plugins/ai-proxy/src/lib.rs#L457-L460)), but the config schema still makes `model` required per target, and there is no way to route based on the model name the client sent. Operators who want "route any `claude-*` request to Anthropic, any `gpt-*` request to OpenAI" have to declare every model explicitly and keep the list in sync with provider releases.

This is ergonomic friction, not a correctness issue — but it compounds with Gap 1 (a single Responses-aware deployment is likely to expose many models simultaneously) and is cheap to fix.

### Non-goal: stateful Responses API

The stateful mode of the Responses API (`store: true` + `previous_response_id`, plus the companion `GET /v1/responses/{id}` retrieval and cancel endpoints) requires the gateway to persist conversation state across requests keyed by a response identifier it issues itself. Barbacane's WASM plugin runtime has no session-scoped storage primitive today (only per-request `context` and a global `cache` KV). Adding one is a larger refactor that deserves its own ADR. This ADR is **stateless-only**: clients must send the full `input[]` each turn.

The v1 behavior on `store: true` is deliberately permissive rather than rejecting — see section 2 below. The real boundary the gateway can't cross is `previous_response_id`, which is where the hard rejection lives.

## Decision

### 0. Principle: the model identifier is caller-owned

The `model` string in an AI request is part of the **client's** contract, not the gateway's. The gateway routes, authenticates, rate-limits, and translates — it does not decide which model a request runs against. This principle governs the rest of this ADR:

- Gateway config declares **providers** (where to go, with what credentials) and **routing rules** (how to pick a provider from a request), never an authoritative model list.
- The client's `model` field is passed to the upstream provider verbatim. No gateway-side override.
- Operators do not need to update the gateway when a provider ships a new model. Adding `claude-opus-5` on the Anthropic side requires zero Barbacane config change if `routes` matches `claude-*`.
- If the client omits `model`, the dispatcher returns `400 model_required`. Matches the upstream provider contracts (both OpenAI Chat Completions and Responses API mark `model` as required) and keeps the gateway out of the model-selection business.

#### Removal of `model` from `targets` and the flat config

The `model` field in `targets.<name>.<model>` and in the flat top-level config (inherited from ADR-0024) is **removed** by this ADR, not deprecated. Barbacane is pre-1.0 and the codebase convention is to avoid backward-compat shims at this stage. The migration path for existing ADR-0024 deployments is one line: delete the `model` field from each target. The dispatcher now derives provider credentials from the resolved target but always uses the client's `model` field for the outbound request.

The `targets` map itself is retained for the policy-driven routing case (logical tiers selected via `cel` writing `ai.target` into context). It simply no longer carries a `model` — it holds `provider`, `api_key`, and optionally `base_url`.

##### Migration UX

`additionalProperties: false` in `plugins/ai-proxy/config-schema.json` means a leftover `model:` field is rejected by the compiler at lint time, not silently ignored at runtime. The error surfaces through the existing `vacuum:barbacane` ruleset as a JSON Schema validation failure (E1015 / config-validation), pointing at the offending operation's spec line. The implementing PR ships a CHANGELOG entry under `### Changed` documenting the breaking change and the one-line migration, and the `vacuum:barbacane` error message names the field explicitly (`"model" is no longer accepted on ai-proxy targets — see ADR-0030`) so the failure is self-explanatory without forcing operators to read this ADR.

#### Caller-owned, gateway-gated

"The caller owns the model identifier" does not mean "the gateway forwards anything." The gateway is still responsible for enforcing policy on caller intent — exactly like an HTTP gateway lets the client pick the URL but returns `403` on paths the client is not authorized to hit. For AI traffic there are two distinct gating layers, and they live in two distinct places:

| Layer | Scope | Example | Where it lives |
|---|---|---|---|
| **Catalog policy** | Static, operator-level — "this gateway serves these models, period" | *"Never route to `claude-opus-*`, it's too expensive"* | `ai-proxy` dispatcher config (`routes.allow` / `routes.deny`) |
| **Consumer policy** | Dynamic, tenant/tier-aware — depends on the caller identity | *"Free tier can't use `gpt-4o`, premium can"* | Middleware (`cel` or an `ai-prompt-guard` option) |

The catalog policy is enforced by the dispatcher before dispatch: a route match is a necessary but not sufficient condition, the requested model must also pass the route's `allow`/`deny` filter. On failure: `403` with `error.type = "model_not_permitted"` and `error.message` naming the offending model.

The consumer policy is enforced by middleware, which already has access to `request.claims` / `request.headers`. The existing `cel` middleware covers this — with one prerequisite: today the plugin exposes `request.body` as a raw string ([plugins/cel/src/lib.rs:124-127](../plugins/cel/src/lib.rs#L124-L127)), which makes JSON-field access unusable in policy expressions. The fix is small and local to the `cel` plugin: when the inbound `content-type` is `application/json` (or a `+json` suffix), parse the body once and bind it under a separate name in the CEL context so existing string-based policies continue to evaluate unchanged.

This ADR commits to that as a **prerequisite work item** before consumer-policy AI examples can ship. With the extension in place:

```yaml
- name: cel
  config:
    expression: "request.body_json.model.startsWith('gpt-4o') && request.claims.tier != 'premium'"
    on_match:
      deny:
        status: 403
        code: "model_not_permitted"
```

Naming (`request.body_json` vs. auto-overloading `request.body`), parse-error failure mode, and whether the parsed value is cached across stacked CEL middlewares are details for the implementing PR — settled on the `cel` plugin side, not in `ai-proxy`. No new capability is required: `cel` already declares `body_access = true` ([plugins/cel/plugin.toml](../plugins/cel/plugin.toml)).

Splitting the two layers matters because they have different audiences (platform team vs. tenant team), different change cadences (rarely vs. per-contract), and different failure modes (misconfiguration blocks everyone vs. one tenant). Collapsing them into a single config section would make both harder to reason about.

### 1. `ai-proxy` becomes protocol-aware

The existing `ai-proxy` dispatcher gains a second client-facing endpoint. It exposes:

- `POST /v1/chat/completions` — existing Chat Completions surface (ADR-0024)
- `POST /v1/responses` — new Responses API surface (this ADR)

Path-based dispatch inside the plugin chooses the translation layer. Targets, credentials, fallback chain, metrics, and context propagation are **shared** across both protocols — they live one layer below the protocol adapter.

Source layout refactor in `plugins/ai-proxy/src/`:

```
lib.rs                    dispatch entry + target resolution (shared)
protocols/
  chat_completion.rs      existing translate_{to,from}_anthropic (extracted)
  responses.rs            new — stateless Responses ↔ Messages translation
providers/
  openai.rs               passthrough for both protocols
  anthropic.rs            translate per incoming protocol
  ollama.rs               passthrough (Chat Completions only for now)
```

**Rationale for extending `ai-proxy` rather than shipping a second plugin:** targets, fallback, `/v1/models`, and observability are protocol-agnostic. A second plugin would duplicate all of that and force operators to pick which protocol surface they want per deployment. Keeping a single plugin lets a single `ai-proxy` instance serve mixed-format clients against the same target pool.

### 2. Responses API translation — stateless only

The `responses.rs` adapter:

- **Accepts** `POST /v1/responses` regardless of the `store` value (`true`, `false`, or omitted). The gateway always processes the request statelessly — upstream to Anthropic, no persistence. Rationale: most clients send `store: true` as an unexamined default (it is the OpenAI server-side default) without ever using the stateful features; rejecting `store: true` would break those clients gratuitously. The gateway fails at the real boundary (see next point), not at a flag the client didn't mean to invoke.
- **Emits observability signals** when `store ≠ false`:
  - HTTP response header `Warning: 299 - "store ignored; gateway is stateless, see ADR-0030"`
  - Metric counter `barbacane_plugin_ai_proxy_responses_store_downgrades_total` incremented — lets operators quantify stateful-API usage and decide whether to prioritize the session capability work
- **Generates a synthetic `id`** in the response (format `resp_<uuid-v7>`). UUIDv7 (RFC 9562) is time-ordered, so a `resp_*` grep across access logs comes out in chronological order without a separate sort key — and the embedded timestamp leaks no information the response wasn't already carrying via `created_at`. Clients that read the ID only as an opaque tracking handle are unaffected; clients that try to reuse it fail explicitly at the next point.
- **Rejects `previous_response_id`** with `400` and `error.type = "previous_response_id_not_supported"`. This is the only genuinely stateful feature a client can invoke, and the rejection lands exactly where the client is trying to use state the gateway cannot provide. No silent behavior change, no distant mystery failure.
- **Related endpoints** (`GET /v1/responses/{id}`, `POST /v1/responses/{id}/cancel`, etc.) are not included in the ai-gateway spec fragment shipped by Barbacane (see section 4). Absence of the route → `404` from the standard router, no special handler. This keeps the spec-driven invariant: the gateway's capabilities are visible in the compiled `.bca` artifact.
- **Translates** `input[]` items to Anthropic Messages `content` blocks:
  - `input_text` / `input_image` → `text` / `image` content blocks
  - `function_call` + `function_call_output` → `tool_use` + `tool_result` blocks
  - `reasoning` items → dropped on the request side (Anthropic does not accept client-supplied reasoning input). Because dropping reasoning silently can degrade output quality on multi-turn agent flows in ways the client cannot detect, the dispatcher emits `Warning: 299 - "reasoning items dropped; Anthropic upstream does not accept client-supplied reasoning input"` and increments `barbacane_plugin_ai_proxy_responses_reasoning_dropped_total` whenever it strips at least one reasoning item from `input[]`. Documented as a known fidelity gap.
- **Translates** Anthropic response back to Responses API shape:
  - `content` blocks → `output[]` items of matching types
  - Anthropic `usage` → Responses `usage` (`input_tokens` / `output_tokens` map directly; no `prompt_tokens` rename needed since Responses already uses the new naming)
- **Pins** the Anthropic API version to `2024-10-22` (same as ADR-0024 Chat Completions path), subject to the contract-test-and-bump process from ADR-0024.
- **Streaming:** Responses API SSE (`response.output_item.delta` events, etc.) maps to Anthropic SSE (`content_block_delta`). Initial release buffers Anthropic responses and emits a single terminal Responses event, matching the current Chat Completions path ([plugins/ai-proxy/src/lib.rs:286-292](../plugins/ai-proxy/src/lib.rs#L286-L292)). True token-by-token Responses SSE translation is deferred to the same future work item as Chat Completions SSE.

### 3. Dynamic model routing (`routes` table)

A new `routes` section in `ai-proxy` config resolves the provider/credentials by matching the `model` field in the client request against glob patterns:

```yaml
x-barbacane-dispatch:
  name: ai-proxy
  config:
    routes:
      - pattern: "claude-*"
        provider: anthropic
        api_key: "${ANTHROPIC_API_KEY}"
        deny: ["claude-opus-*"]        # catalog policy: no Opus on this gateway
      - pattern: "gpt-*"
        provider: openai
        api_key: "${OPENAI_API_KEY}"
        allow: ["gpt-4o-mini", "gpt-4o"]
      - pattern: "o[1-4]*"
        provider: openai
        api_key: "${OPENAI_API_KEY}"
      - pattern: "*"
        provider: ollama
        base_url: http://ollama:11434
```

`allow` and `deny` are optional glob lists evaluated against the client's `model` field after the route matches. Semantics: if `allow` is set, the model must match at least one entry; if `deny` is set, the model must not match any entry; both can be combined (deny is evaluated after allow). A request that fails either check gets a `403` — it does not fall through to the next route pattern, because that would silently escalate a denied model to a different provider.

#### Catalog policy applies on every resolution path

A subtlety the first draft of this ADR left implicit: catalog `allow` / `deny` is meant as an *operator-level* statement ("this gateway never serves `claude-opus-*`, period"). That intent does not vary based on *how* the dispatcher arrived at a target. So the rule is: **`routes.<>.allow` / `deny` are evaluated against the inbound `model` field on every resolution path that produces a target carrying `allow` / `deny`** — including when the target was selected via `ai.target` from a `cel` middleware (path 1 below).

Concretely, the resolution algorithm is:

1. Resolve the target via the precedence list below — this picks the `provider` + credentials.
2. If the resolved target carries `allow` and/or `deny` rules, evaluate them against `request.body.model`. Failure → `403 model_not_permitted`.
3. Dispatch.

Resolution precedence:

1. `ai.target` context key (set by `cel` middleware — existing ADR-0024 behavior) — wins if present. Resolves to `targets[<value>]`. If that target was declared in the `routes` table (i.e. the operator merged the two by giving a route pattern a `name`), its `allow` / `deny` apply. If it was declared as a plain `targets.<name>` without `allow`/`deny`, none apply (consumer-policy CEL is the right enforcement layer for that case).
2. `routes` pattern match against the request's `model` field — new. `allow` / `deny` apply.
3. `default_target` → `targets[name]` — existing. `allow` / `deny` from that target apply if declared.
4. Flat `provider` + `model` — existing. No `allow` / `deny` (single-target config).

The point is: catalog policy is attached to **the target**, not to the resolution path. A `cel` misconfig that sets `ai.target: anthropic-tier` cannot leak a denied model, because the `allow` / `deny` on that target still gate the dispatch. This is the safe-by-default reading; operators who want CEL to fully bypass catalog policy can put their CEL logic on a target that simply has no `allow`/`deny`.

#### Patterns

Patterns are glob (`*`, `?`, `[...]`), not full regex — simpler schema, covers the 95% case. First match wins.

Glob syntax, case-sensitivity, and anchoring are constrained at the **plugin's JSON schema** level (a `pattern` regex on `routes[].pattern`) so that invalid syntax is rejected at compile time by `vacuum:barbacane`, not at runtime. The implementing PR picks the underlying glob library (`globset` is the obvious fit) and the schema mirrors that library's accepted character set. This keeps the ADR out of the business of pinning a specific library version.

The `model` field is **intentionally absent from `routes` entries**, enforcing the principle above: routes declare which provider a model family belongs to, not what models exist. Operators do not track new model names in the gateway config. The `targets` map remains available for the policy-driven routing case (ADR-0024) but no longer carries a `model` either — see section 0.

#### `allow` / `deny` does not fall through — escape hatch

Because a denied model returns `403` and does not fall through, this config:

```yaml
routes:
  - pattern: "gpt-*"
    provider: openai
    api_key: "${OPENAI_API_KEY}"
    allow: ["gpt-4o", "gpt-4o-mini"]
  - pattern: "*"
    provider: ollama
    base_url: http://ollama:11434
```

returns `403` for `gpt-3.5-turbo` — it does **not** route to ollama. This is the safe default (a "deny" is not silently escalated to a different provider), but it is mildly surprising. The escape hatch is to make the route pattern more specific so non-matching OpenAI models miss the route entirely and reach the catch-all:

```yaml
routes:
  - pattern: "gpt-4o*"           # only matches what the allow-list would accept
    provider: openai
    api_key: "${OPENAI_API_KEY}"
  - pattern: "*"
    provider: ollama
    base_url: http://ollama:11434
```

Now `gpt-3.5-turbo` falls through to ollama, while `gpt-4o-mini` still routes to OpenAI. The trade-off is explicit: the `allow` list is the operator's promise *"only these from this provider"*; tightening the pattern is the operator's promise *"anything else doesn't belong to this provider at all."* The two read differently and should.

When no route matches the client's `model` (or when the request omits `model` entirely), the dispatcher returns `400` with `error.type = "model_required"` or `"no_route"` respectively. The gateway does not silently pick a default.

### 4. `/v1/models` served by the `ai-proxy` dispatcher

Model discovery (`GET /v1/models`) stays **inside the spec-driven routing model**: it is served by the same `ai-proxy` dispatcher, bound to a `GET /v1/models` operation declared in the tenant spec. No native data plane handler, no carve-out around the middleware pipeline.

An initial draft of this ADR proposed a native handler injected outside the spec. That was rejected as inconsistent with Barbacane's core invariant — **every data plane path is declared in the spec and flows through the standard dispatch + middleware pipeline** — and it also created a cross-plugin config problem (a discovery plugin would need to read the `ai-proxy` plugin's targets, which is not a capability Barbacane exposes).

#### Dispatcher behavior per path

The `ai-proxy` dispatcher selects its behavior from `req.path` (the same dispatch key already used in section 1):

| Incoming path | Behavior |
|---|---|
| `POST /v1/chat/completions` | Chat Completions translation (ADR-0024) |
| `POST /v1/responses` | Responses API translation (this ADR, section 2) |
| `GET  /v1/models` | Return the model catalog derived from the dispatcher's own config |

The dispatcher's own config is the source of truth — there is no cross-plugin lookup to invent. With `model` removed from `targets` (section 0), the dispatcher has no local list of model identifiers to advertise, so discovery works by querying each configured provider's own `/models` endpoint and aggregating the results. Entries are filtered by the route `allow` / `deny` rules that would apply to a request with that model, so a deny-listed model never appears in `/v1/models` — no accidental advertising of models the gateway would refuse.

##### Caching

Aggregation is cached via the existing `cache` capability (the same KV that backs the [`cache` middleware plugin](../plugins/cache/plugin.toml) — `host_cache_get` / `host_cache_set` host functions). `ai-proxy` adds `cache = true` to its capability manifest; the cache key is derived from the dispatcher's resolved provider set + an optional `models_cache` config block:

```yaml
config:
  models_cache:
    ttl: 300        # seconds, default 300
    enabled: true   # set false to query providers on every request
```

Scope and failure mode — both worth flagging because they are non-obvious:

- **Per-data-plane-instance, not cross-replica.** The `cache` capability is process-local. Each replica builds and serves its own `/v1/models` snapshot. Two replicas can momentarily disagree if a provider has just added or removed a model; this is acceptable for a discovery endpoint that is itself eventually-consistent upstream.
- **Cache-miss thundering herd.** A simultaneous burst of `/v1/models` requests after a TTL expiry can fan out to every provider. Mitigated by single-flight inside the dispatcher (only one upstream call per (provider, instance) at a time; concurrent callers wait on the in-flight result). Implementation detail, not a config knob.
- **Partial provider failure.** If one provider's `/models` call fails (5xx, timeout, connection error), the dispatcher returns a partial response — `200 OK` with the available providers' models in `data[]`, plus a `partial: true` field on the response and a `warnings: [{provider, status}]` array describing each failure. Rationale: a discovery endpoint that hard-fails when one of three providers is flaky causes more user-visible breakage than it prevents, and the partial flag lets sophisticated clients react. The failure is also tagged via the counter `barbacane_plugin_ai_proxy_models_provider_failures_total{provider}` so operators see it without polling clients.

Response shape (OpenAI-compatible):

```json
{
  "object": "list",
  "data": [
    {"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"},
    {"id": "gpt-4o",            "object": "model", "owned_by": "openai"}
  ]
}
```

#### Operator ergonomics: the ai-gateway spec fragment

Requiring the operator to declare three operations in their spec and bind `ai-proxy` to each — with the same config block duplicated three times — is friction we do not want to impose. The fix is **not** a native handler; it is a reusable spec fragment.

##### What the compiler already gives us

The compiler already supports multi-file specs. A `barbacane.yaml` manifest can point `specs:` at a folder; every `*.yaml` / `*.yml` / `*.json` in that folder is parsed and **all operations across all files merge into a single artifact** ([crates/barbacane-compiler/src/manifest.rs:268-322](../crates/barbacane-compiler/src/manifest.rs#L268-L322), [crates/barbacane-compiler/src/artifact.rs:417-426](../crates/barbacane-compiler/src/artifact.rs#L417-L426)). Duplicate routes and `operationId`s are rejected; otherwise files are interchangeable. Plugin config values that look like `env://VAR_NAME` are resolved at runtime from the environment ([crates/barbacane-wasm/src/secrets.rs:79-85](../crates/barbacane-wasm/src/secrets.rs#L79-L85)).

Together, these mean Barbacane can ship `schemas/ai-gateway.yaml` as a **standalone spec file** that operators drop into their `specs/` folder alongside their tenant spec. The shipped file declares the three operations and binds `ai-proxy` with `env://`-resolved provider keys. No new compiler mechanism is required to make this work — what we are calling "the ai-gateway spec fragment" is just a regular spec the compiler already knows how to consume.

```
my-api/
├── barbacane.yaml          # specs: ./specs/
└── specs/
    ├── api.yaml            # tenant spec
    └── ai-gateway.yaml     # shipped by Barbacane; declares the three operations
```

##### What's still duplicated, and what to do about it

The remaining friction is config-level, not file-level: each of the three operations in the shipped fragment carries its own `x-barbacane-dispatch: { name: ai-proxy, config: { … } }` block, and an operator who wants to override a route or add a target has to edit three blocks in lockstep. The current pattern for solving this *for middlewares* is the root-level `x-barbacane-middlewares`, which the compiler merges with each operation's chain ([crates/barbacane-compiler/src/spec_parser/parser.rs:109](../crates/barbacane-compiler/src/spec_parser/parser.rs#L109)). Dispatchers do not have a root-level equivalent — `x-barbacane-dispatch` is operation-only ([crates/barbacane-compiler/src/spec_parser/model.rs:52](../crates/barbacane-compiler/src/spec_parser/model.rs#L52)).

Two options for v1 of this ADR, both small:

1. **Bake the config into the shipped fragment with `env://` placeholders for credentials.** Operators set `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `OLLAMA_BASE_URL` in their environment and don't touch the file. Routes and price tables are in the shipped fragment; tweaking either means forking the file. Zero compiler changes. Works today.
2. **Add a root-level `x-barbacane-dispatch-defaults` block** keyed by plugin name, parallel to root-level `x-barbacane-middlewares`. Operations that bind to that plugin without their own `config` inherit the defaults. The shipped fragment carries `x-barbacane-dispatch: { name: ai-proxy }` (no config); the operator's tenant spec carries `x-barbacane-dispatch-defaults: { ai-proxy: { config: { … } } }`. Generalizes beyond AI gateway. Modest compiler addition, mirroring the well-established middleware pattern.

**This ADR's recommendation is to ship option 1 in v1 and treat option 2 as a follow-up improvement.** Option 1 unblocks `/v1/models` without any compiler change; option 2 is a clean ergonomic win when an operator is ready to invest. Operators on option 1 who want to customize routes or pricing fork the shipped fragment into their own `specs/` folder — a one-line cost for what should be a rare action.

Either path keeps the spec-driven invariant: the compiled `.bca` artifact lists every operation explicitly, regardless of how it got there.

The point of calling this out is: do not let the implementation PR sneak a half-designed merge mechanism in alongside the Responses API work. The two are decoupled and benefit from being decided separately.

#### Disabling `/v1/models`

An operator who does not want to expose `/v1/models` simply does not include that operation in their spec (or imports a variant of the fragment without it). No config flag needed — the spec is the source of truth. This is another reason the spec-driven approach wins: removing a route is a spec edit, not a config flag, and the absence is auditable via the compiled `.bca` artifact.

## Consequences

### Easier

- Running `codex-cli` and other Responses-API-native tools against Anthropic models through Barbacane, without a sidecar proxy.
- Tool-use-heavy workloads translate more cleanly (item-based → content-block mapping is near-isomorphic).
- Adding a new Anthropic or OpenAI model: no gateway config change if the operator is using `routes`.
- Model discovery for AI-aware clients (`/v1/models`) via an importable spec fragment, without operator-written plugins and without breaking spec-driven invariants.
- Mixed-format deployments: a single `ai-proxy` instance serves both Chat Completions and Responses clients against the same target pool.

### Harder

- `ai-proxy` is now protocol-aware. Translation bugs can be specific to one surface and not the other, doubling the contract-test matrix (Chat Completions × {OpenAI, Anthropic}, Responses × {OpenAI, Anthropic}).
- Streaming fidelity remains a known limitation on both protocols until real SSE translation lands.
- The `routes` table introduces a third resolution path, increasing the surface area of "why did my request go to provider X?" debugging. Mitigation: the dispatcher emits a metric label `resolution = {context|routes|default|flat}` and a debug log line.
- **Breaking change for ADR-0024 deployments**: the `model` field is removed from `targets.<name>` and from the flat top-level config. Existing specs must be updated — delete the field from each target. Justified by the project's pre-1.0 status and the codebase convention of avoiding backward-compat shims at this stage.

### Deferred (explicitly out of scope)

- **Stateful Responses API** (`previous_response_id`, `GET /v1/responses/{id}`, cancel). Requires a session-scoped storage capability in the WASM runtime. Track as a future ADR. The `400 previous_response_id_not_supported` rejection in v1 is the forward-compatibility hook — a future release can remove it without breaking existing clients that were already failing on it.
- **True SSE translation** for either protocol against Anthropic. Inherits the deferral from ADR-0024.
- **Ollama Responses API support.** Ollama's OpenAI-compat surface is Chat Completions only as of 2026-04. If/when Ollama adds Responses, it is a one-line passthrough addition.

## References

- [OpenAI Responses API reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
- [ADR-0023: WASM plugin streaming](0023-wasm-plugin-streaming.md) — prerequisite for SSE
- [ADR-0024: AI Gateway Plugin](0024-ai-gateway-plugin.md) — predecessor, defines Chat Completions path
