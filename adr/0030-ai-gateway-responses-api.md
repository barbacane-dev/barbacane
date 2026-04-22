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
- The client's `model` field is passed to the upstream provider verbatim.
- The client's `model` field is passed to the upstream provider verbatim. No gateway-side override.
- Operators do not need to update the gateway when a provider ships a new model. Adding `claude-opus-5` on the Anthropic side requires zero Barbacane config change if `routes` matches `claude-*`.
- If the client omits `model`, the dispatcher returns `400 model_required`. Matches the upstream provider contracts (both OpenAI Chat Completions and Responses API mark `model` as required) and keeps the gateway out of the model-selection business.

#### Removal of `model` from `targets` and the flat config

The `model` field in `targets.<name>.<model>` and in the flat top-level config (inherited from ADR-0024) is **removed** by this ADR, not deprecated. Barbacane is pre-1.0 and the codebase convention is to avoid backward-compat shims at this stage. The migration path for existing ADR-0024 deployments is one line: delete the `model` field from each target. The dispatcher now derives provider credentials from the resolved target but always uses the client's `model` field for the outbound request.

The `targets` map itself is retained for the policy-driven routing case (logical tiers selected via `cel` writing `ai.target` into context). It simply no longer carries a `model` — it holds `provider`, `api_key`, and optionally `base_url`.

#### Caller-owned, gateway-gated

"The caller owns the model identifier" does not mean "the gateway forwards anything." The gateway is still responsible for enforcing policy on caller intent — exactly like an HTTP gateway lets the client pick the URL but returns `403` on paths the client is not authorized to hit. For AI traffic there are two distinct gating layers, and they live in two distinct places:

| Layer | Scope | Example | Where it lives |
|---|---|---|---|
| **Catalog policy** | Static, operator-level — "this gateway serves these models, period" | *"Never route to `claude-opus-*`, it's too expensive"* | `ai-proxy` dispatcher config (`routes.allow` / `routes.deny`) |
| **Consumer policy** | Dynamic, tenant/tier-aware — depends on the caller identity | *"Free tier can't use `gpt-4o`, premium can"* | Middleware (`cel` or an `ai-prompt-guard` option) |

The catalog policy is enforced by the dispatcher before dispatch: a route match is a necessary but not sufficient condition, the requested model must also pass the route's `allow`/`deny` filter. On failure: `403` with `error.type = "model_not_permitted"` and `error.message` naming the offending model.

The consumer policy is enforced by middleware, which already has access to `request.claims` / `request.headers` and to the request body (including the `model` field). The existing `cel` middleware covers this without any new plugin:

```yaml
- name: cel
  config:
    expression: "request.body.model.startsWith('gpt-4o') && request.claims.tier != 'premium'"
    on_match:
      deny:
        status: 403
        code: "model_not_permitted"
```

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
  - Metric counter `ai_proxy.responses.store_downgrade_total` incremented — lets operators quantify stateful-API usage and decide whether to prioritize the session capability work
- **Generates a synthetic `id`** in the response (format `resp_<uuid>`), satisfying the Responses API contract even though the ID will not be retrievable. Clients that read the ID only as an opaque tracking handle are unaffected; clients that try to reuse it fail explicitly at the next point.
- **Rejects `previous_response_id`** with `400` and `error.type = "previous_response_id_not_supported"`. This is the only genuinely stateful feature a client can invoke, and the rejection lands exactly where the client is trying to use state the gateway cannot provide. No silent behavior change, no distant mystery failure.
- **Related endpoints** (`GET /v1/responses/{id}`, `POST /v1/responses/{id}/cancel`, etc.) are not included in the ai-gateway spec fragment shipped by Barbacane (see section 4). Absence of the route → `404` from the standard router, no special handler. This keeps the spec-driven invariant: the gateway's capabilities are visible in the compiled `.bca` artifact.
- **Translates** `input[]` items to Anthropic Messages `content` blocks:
  - `input_text` / `input_image` → `text` / `image` content blocks
  - `function_call` + `function_call_output` → `tool_use` + `tool_result` blocks
  - `reasoning` items → dropped on the request side (Anthropic does not accept client-supplied reasoning input)
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

Resolution order in the dispatcher:

1. `ai.target` context key (set by `cel` middleware — existing ADR-0024 behavior) — wins if present
2. `routes` pattern match against the request's `model` field — new
3. `default_target` → `targets[name]` — existing
4. Flat `provider` + `model` — existing

Patterns are glob (`*`, `?`, `[...]`), not full regex — simpler schema, covers the 95% case. First match wins.

The `model` field is **intentionally absent from `routes` entries**, enforcing the principle above: routes declare which provider a model family belongs to, not what models exist. Operators do not track new model names in the gateway config. The `targets` map remains available for the policy-driven routing case (ADR-0024) but no longer carries a `model` either — see section 0.

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

The dispatcher's own config is the source of truth — there is no cross-plugin lookup to invent. With `model` removed from `targets` (section 0), the dispatcher has no local list of model identifiers to advertise, so discovery works by querying each configured provider's own `/models` endpoint and aggregating the results (cached, default TTL 5 minutes). Entries are filtered by the route `allow` / `deny` rules that would apply to a request with that model, so a deny-listed model never appears in `/v1/models` — no accidental advertising of models the gateway would refuse.

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

Requiring the operator to declare three operations in their spec and bind `ai-proxy` to each, with the same config block duplicated three times, is friction we do not want to impose. The fix is **not** a native handler; it is a reusable spec fragment.

Barbacane ships a standard OpenAPI fragment at `schemas/ai-gateway.yaml` declaring the three operations pre-bound to `ai-proxy`. Operators include it in their spec via `$ref` or the compiler's merge directive (whichever Barbacane already supports for reusable operation bundles), and supply the dispatcher config once. The shape an operator writes looks like:

```yaml
# tenant-spec.yaml
paths:
  $merge: ./schemas/ai-gateway.yaml
x-barbacane-shared-configs:
  ai-proxy:
    routes:
      - pattern: "claude-*"
        provider: anthropic
        api_key: "${ANTHROPIC_API_KEY}"
    # …
```

The compiler resolves the three operations, injects the shared `ai-proxy` config into each binding, and emits a single `.bca` artifact. From the runtime's perspective there is nothing special — three normal operations, one plugin, standard middleware pipeline.

If the exact mechanism (`$merge`, `$ref` to a shared operation set, a new `x-barbacane-include`) is not already supported, adding it is the work item this ADR creates for the compiler — it is strictly smaller than a native handler carve-out and generalizes beyond AI gateway use cases.

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
