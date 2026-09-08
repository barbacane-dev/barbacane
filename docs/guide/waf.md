# Web application firewall

Barbacane can run a ModSecurity-compatible rule set, such as the OWASP Core
Rule Set, as a native stage in the request pipeline. The rule set is validated
when you compile the artifact and sealed into it, so the gateway never parses
rules at request time and an unknown directive fails the build rather than
becoming a rule that silently never fires.

This complements spec validation rather than duplicating it. The spec is a
*positive* model: it says what a request may look like, and anything outside
the schema is rejected. The rule set is a *negative* model: it recognises
attack shapes whether or not they are schema-valid. A perfectly schema-valid
`?q=1' OR 1=1--` passes validation, and is exactly what the rule set catches.

## Enabling it

Declare `x-barbacane-waf` at the root of your spec:

```yaml
x-barbacane-waf:
  ruleset: ./crs-4.9.0/rules   # directory of SecLang .conf files
  paranoia_level: 1            # 1-4
  mode: blocking               # blocking | detection-only
  thresholds:
    inbound: 5
    outbound: 4
  unsupported_rules: fail      # fail (default) | skip
```

`ruleset` is resolved relative to the spec. Point it at a directory containing
the `.conf` files and, for CRS, the setup file:

```bash
curl -sL https://github.com/coreruleset/coreruleset/archive/refs/tags/v4.9.0.tar.gz | tar xz
mkdir -p crs-4.9.0/rules
cp coreruleset-4.9.0/rules/*.conf coreruleset-4.9.0/rules/*.data crs-4.9.0/rules/
cp coreruleset-4.9.0/crs-setup.conf.example crs-4.9.0/rules/000-crs-setup.conf
```

The setup file is not optional. Without it CRS blocks every request at rule
901001, which exists precisely to stop a half-configured deployment from
looking like it works.

Pin the CRS version. A floating reference makes two builds of the same spec
enforce different rules, and the rule set is covered by the artifact hash and
signature, so the artifact is only reproducible if its inputs are.

## What compilation does

`barbacane compile` parses the rule set, refuses anything it cannot enforce,
and seals the validated form into the artifact along with the `@pmFromFile`
phrase lists the rules reference. The rule set and the policy are both folded
into `artifact_hash`, so a signed artifact cannot be switched from blocking to
detection-only, or have its paranoia level lowered, without invalidating the
signature.

A rule the build cannot enforce fails the build:

```text
error[E1080]: x-barbacane-waf: 4 rule(s) in the rule set cannot be enforced by
this build: rule 942100 (line 46): operator @detectSQLi is not implemented yet
...
Set `unsupported_rules: skip` to build without them. The artifact then records
their ids and the gateway will not enforce them.
```

That is the default because shipping the rest of a rule set as though it were
complete is how a rule becomes a bypass. `unsupported_rules: skip` is the
explicit opt-in, and it is not silent: the compiler warns with the rule ids,
the ids go into the manifest where the hash covers them, and the gateway logs
them at WARN on every boot.

## Current limitations

Read these before enabling it in production.

**`@detectSQLi` and `@detectXSS` are not implemented.** Four CRS rules use
them (941100, 941101, 942100, 942101) and they are the libinjection
classifiers, not peripheral rules. With `unsupported_rules: skip` the rest of
CRS runs without them. Regex-based SQLi and XSS rules still fire, so a
`UNION SELECT` or a `<script>` tag is still caught; a tautology such as
`1' OR '1'='1` may not be.

**Response-phase rules are not evaluated.** A CRS artifact carries around 152
rules in phases 3 to 5, mostly outbound data-leakage detection, and they are
present in the artifact but not run. Request-phase rules, around 439 of them,
are enforced. The gateway warns about this at startup. Outbound inspection is
implemented in the engine but not yet wired into the response path, which has
several exits and one unresolved question: a streamed response has already
reached the client before a phase-4 rule could act on it.

**Cost is around 2 ms per request.** Measured with full CRS at paranoia level
1 through a real gateway: 1.9 ms mean, 97% under 2.5 ms. One core sustains on
the order of 500 requests per second of inspection. Cost scales with the
number of inspected values, so a request with ten query parameters costs
roughly twice one with a single parameter.

That is what a full rule set costs, and it is comparable to other CRS
implementations, but it dominates any per-request budget it shares: spec
validation is about 1.2 µs by comparison. Enable it where it earns its cost
rather than globally by reflex, and prefer a lower paranoia level over a higher
one until you have tuned for false positives.

**Rule-set tuning is your responsibility.** A WAF is not plug-and-play.
Run in `detection-only` first, watch **`barbacane_waf_matched_total`** by rule
id, and add exclusions before switching to `blocking`. Note the metric:
`barbacane_waf_blocked_total` stays at zero in detection-only mode, because
nothing is blocked, so it tells you nothing while you are tuning. Upstream says the same
thing, and it is the single most common reason a WAF gets turned off again.

## Observing it

Three metrics on the admin endpoint:

| Metric | Meaning |
|---|---|
| `barbacane_waf_matched_total{method,path,rule_id}` | Rules that matched, whether or not the request was blocked. **This is the tuning signal**, and the only one that moves in detection-only mode. A single noisy rule is usually the whole false-positive problem. |
| `barbacane_waf_blocked_total{method,path,rule_id}` | Requests actually interrupted, by the rule that did it. Zero in detection-only mode. |
| `barbacane_waf_allowed_total{method,path}` | Requests inspected and allowed. With the above, the block rate. |
| `barbacane_waf_duration_seconds{method,path}` | Time spent inspecting, so the WAF's share of latency is visible rather than inferred. |

A blocked request is logged at WARN with the rule id, the accumulated anomaly
score and the rule's message, and answers `403` with an RFC 9457 body. The rule
id and message appear in the response body only in dev mode: in production they
tell an attacker exactly which rule to shape the next payload around.

## How blocking actually happens

With CRS, individual rules mostly do not block. They add to an anomaly score,
and a final rule blocks when the score crosses `thresholds.inbound`. So the
rule id in a block log is usually 949110, the blocking-evaluation rule, and the
rules that contributed are in the score:

```json
{"level":"WARN","message":"WAF blocked request","rule_id":949110,
 "status":403,"inbound_score":18,"path":"/search"}
```

This is why `thresholds.inbound` is the main tuning dial: lowering it blocks
more, raising it blocks less, and neither requires touching the rules.
