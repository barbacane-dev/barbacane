# AI Gateway

Barbacane ships an OpenAI-compatible AI gateway built from one dispatcher and four middlewares. This page is a quickstart — it walks through the minimum viable configuration, the three protocol surfaces, and the layering of policy concerns. For the full reference of each component, follow the cross-links.

## What you get

| Surface | Endpoint | Purpose |
|---|---|---|
| Chat Completions | `POST /v1/chat/completions` | OpenAI Chat Completions; Anthropic translated to/from Messages |
| Responses API (stateless) | `POST /v1/responses` | OpenAI Responses; synthetic `resp_<uuid-v7>` ids; `previous_response_id` returns 400 |
| Model catalog | `GET /v1/models` | Aggregated catalog across every unique provider declared in the config |

All three are bound to the same [`ai-proxy`](dispatchers.md#ai-proxy) dispatcher. The dispatcher routes a request to a provider by glob-matching the **client-supplied** `model` field — the gateway never declares its own (ADR-0030 §0).

## Quickstart — drop-in spec fragment

The simplest way to bring up the full gateway is to drop the shipped spec fragment into your project's `specs/` folder:

```bash
mkdir -p specs/
cp /path/to/barbacane/schemas/ai-gateway.yaml specs/ai-gateway.yaml
```

Multi-file spec discovery picks it up at compile time alongside your tenant spec. The fragment declares the three operations bound to `ai-proxy` with a YAML anchor for the dispatcher config and reads provider credentials from environment variables via `env://` references:

```bash
export OPENAI_API_KEY=sk-...
export ANTHROPIC_API_KEY=sk-ant-...
export OLLAMA_BASE_URL=http://localhost:11434  # optional; default
```

Default routing in the fragment:

| Glob | Provider |
|---|---|
| `claude-*` | Anthropic |
| `gpt-*` | OpenAI |
| `o[1-4]*` | OpenAI (reasoning series) |
| `*` | Ollama (catch-all — see caveat) |

> **Catch-all caveat.** The fragment ships with `pattern: "*"` → Ollama as the last route. This is convenient for local dev but in production it means typos in `model` (e.g. `gtp-4o`) silently route to Ollama instead of returning a clean `400 no_route`. Drop the catch-all from your copy of the fragment if you want strict validation.

To customise (Azure target, restricted catalog, named tiers), copy the fragment into your `specs/` folder and edit your copy — it's a regular OpenAPI document.

## Layering policy on top

The dispatcher owns *provider routing* and *catalog policy*. Layer middlewares on the same operation for *content* and *cost* policy:

```yaml
# Stack on top of the operations declared in schemas/ai-gateway.yaml
paths:
  /v1/chat/completions:
    post:
      x-barbacane-middlewares:
        - name: jwt-auth
          config:
            issuer: "https://auth.example.com"

        # Per-tier model gating using request body + claims (cel body_json)
        - name: cel
          config:
            expression: >
              request.body_json.model.startsWith('gpt-4')
              && request.claims.tier != 'premium'
            on_match:
              deny:
                status: 403
                code: model_not_permitted_for_tier
                message: "gpt-4* is restricted to the premium tier"

        # Tier-driven profile selection for the AI middlewares
        - name: cel
          config:
            expression: "request.claims.tier == 'premium'"
            on_match:
              set_context:
                ai.policy: premium

        - name: ai-prompt-guard
          config:
            default_profile: standard
            profiles:
              standard: { max_messages: 50, blocked_patterns: ["(?i)ignore previous instructions"] }
              premium:  { max_messages: 200 }

        - name: ai-token-limit
          config:
            default_profile: standard
            partition_key: "header:x-auth-sub"
            profiles:
              standard: { quota: 10000,  window: 60 }
              premium:  { quota: 100000, window: 60 }

        - name: ai-cost-tracker
          config:
            prices:
              openai/gpt-4o:                      { prompt: 0.0025, completion: 0.01 }
              anthropic/claude-sonnet-4-20250514: { prompt: 0.003,  completion: 0.015 }
              ollama/mistral:                     { prompt: 0.0,    completion: 0.0 }
```

### Where each concern lives

| Decision | Place | Mechanism |
|---|---|---|
| Which provider serves a model | dispatcher | [`routes`](dispatchers.md#ai-proxy) glob |
| Which models a target may serve | dispatcher | per-target [`allow` / `deny`](dispatchers.md#ai-proxy) lists |
| Which target a request goes to (caller-driven) | upstream `cel` | [`set_context: { ai.target: ... }`](middlewares/authorization.md#policy-driven-routing-cel-stacking) |
| Per-tier "this caller can't use that model" | upstream `cel` | [`on_match.deny`](middlewares/authorization.md#match-and-deny-per-tier-model-gating) on `request.body_json.model` |
| Prompt validation, token budgets, response redaction | AI middlewares | [`ai-policy` profile selection](middlewares/ai-gateway.md) |
| Per-call cost in USD | `ai-cost-tracker` | reads `ai.provider` / `ai.model` / `ai.prompt_tokens` / `ai.completion_tokens` set by the dispatcher |

The dispatcher's `allow`/`deny` is enforced on every resolution path — context-driven dispatch included — so a `cel` misconfig cannot leak a denied model. Reach for `cel` + `body_json` only when the rule depends on caller attributes the dispatcher doesn't see (claims, headers, time-of-day) or when a custom error code is needed.

## Operating notes

- **Stateless Responses API.** `previous_response_id` returns 400 `previous_response_id_not_supported`. `store: true` is permissive but emits `Warning: 299` and increments `barbacane_plugin_ai_proxy_responses_store_downgrades_total`. Stateful storage is on the [roadmap](../../ROADMAP.md).
- **`/v1/models` partial failures.** A single flaky upstream returns `200 OK` with `partial: true` + a `warnings: []` array rather than a 5xx. Discovery clients should handle the partial case rather than retry on the aggregator. Per-provider timeout is `models_timeout_ms` (default 5000), distinct from the LLM `timeout` so one hung provider doesn't block discovery.
- **Streaming.** SSE chat-completion streams pass through unchanged. Streamed Responses on OpenAI passthrough do **not** rewrite the in-event `id` — true SSE re-encoding is deferred. For strict synthetic-id enforcement, drop `"stream": true`.
- **Ollama Responses.** Returns `400 responses_not_supported_for_provider` — Ollama's OpenAI-compat surface is Chat Completions only.

## Going deeper

- [`ai-proxy` dispatcher](dispatchers.md#ai-proxy) — full configuration reference, resolution chain, metrics, error codes
- [AI gateway middlewares](middlewares/ai-gateway.md) — `ai-prompt-guard`, `ai-token-limit`, `ai-response-guard`, `ai-cost-tracker`
- [`cel` middleware](middlewares/authorization.md#cel) — `request.body_json`, `on_match.set_context`, `on_match.deny`
- [Spec configuration](spec-configuration.md) — multi-file `specs/` discovery
- ADRs — [0024 (AI gateway plugin)](../../adr/0024-ai-gateway-plugin.md), [0030 (Responses API + dynamic routing)](../../adr/0030-ai-gateway-responses-api.md)
