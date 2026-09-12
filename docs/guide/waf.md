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
  max_response_body: 1048576   # bytes; phase-4 body inspection cap (default 1 MiB)
  audit: relevant-only         # off | relevant-only (default) | on
  unsupported_rules: fail      # fail (default) | skip
```

`thresholds.inbound` blocks a request when its accumulated inbound anomaly score
crosses it; `thresholds.outbound` does the same for the response, scored by
phase 3 and 4 rules. `max_response_body` bounds phase-4 body inspection: a
buffered response body at or under it is inspected, a larger or streamed one is
not (see [Response-phase inspection](#response-phase-inspection)). `audit`
controls per-transaction audit logging (see [Audit logging](#audit-logging)).
All of these are covered by `artifact_hash`.

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

A rule the build cannot enforce fails the build. Stock CRS v4.9.0 compiles in
full; a failure comes from a custom rule, for example an unknown directive, a
missing `@pmFromFile` data file, or an invalid regex. `unsupported_rules: skip`
covers only rules whose operator will not compile; a parse error or a missing
data file always fails the build:

```text
error[E1080]: x-barbacane-waf: 1 rule(s) in the rule set cannot be enforced by
this build: rule 900500 (line 12): @rx (: unclosed group
...
Set `unsupported_rules: skip` to build without them. The artifact then records
their ids and the gateway will not enforce them.
```

That is the default because shipping the rest of a rule set as though it were
complete is how a rule becomes a bypass. `unsupported_rules: skip` is the
explicit opt-in, and it is not silent: the compiler warns with the rule ids,
the ids go into the manifest where the hash covers them, and the gateway logs
them at WARN on every boot.

## Response-phase inspection

Response-phase rules (CRS phases 3, 4 and 5) run on the response the same
transaction started on the request, so outbound blocking rules read the scores
the inbound rules accumulated. Phase 3 inspects response headers, phase 4
inspects the response body, and phase 5 is logging and correlation.

Phase 4 body inspection has a bound. A buffered response body at or under
`max_response_body` (default 1 MiB) is collected and inspected. A response that
is streamed, or whose body is larger than the cap, has its headers inspected
(phase 3) but its body skipped: it has already begun reaching the client by the
time a phase-4 rule could act on it. Each skip is counted in
`barbacane_waf_response_body_skipped_total` rather than dropped silently, so a
rule set that promises outbound body inspection can be checked against what the
gateway actually inspects. A WebSocket upgrade has no response phases.

## Audit logging

The WAF writes one structured record per transaction on the `waf.audit` tracing
target, carrying the request id, client address, method, path, the rules that
matched (id, message, logdata, tags, matched variable), the inbound and outbound
anomaly scores, the verdict, and the response status. Route that target to its
own sink to feed a SIEM.

`audit` controls when a record is written:

| Value | Behaviour |
|---|---|
| `off` | Never. |
| `relevant-only` (default) | Only when the transaction was blocked or matched at least one rule that logs. Near-zero volume on clean traffic; the CRS crs-setup default. |
| `on` | Every inspected transaction. |

A rule's `nolog` action keeps it out of the record, matching how it keeps a rule
out of the ModSecurity audit log; the rule still matched and still scored. The
number of records written is exported as `barbacane_waf_audit_total`.

A transaction the WAF allowed but a later gateway check rejects (schema
validation, payload size) before dispatch is not audited in this version.

## Current limitations

Read these before enabling it in production.

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
nothing is blocked, so it tells you nothing while you are tuning. The allowed
counter and the duration histogram do still record, so you can size the cost
before you switch blocking on, but only the matched counter tells you which
rule is responsible. Upstream says the same
thing, and it is the single most common reason a WAF gets turned off again.

## Observing it

Six metrics on the admin endpoint:

| Metric | Meaning |
|---|---|
| `barbacane_waf_matched_total{method,path,rule_id}` | Rules that matched, whether or not the request was blocked. **This is the tuning signal**: a single noisy rule is usually the whole false-positive problem, and this is the only metric that identifies which rule. |
| `barbacane_waf_blocked_total{method,path,rule_id}` | Requests actually interrupted, by the rule that did it. Zero in detection-only mode. |
| `barbacane_waf_allowed_total{method,path}` | Requests inspected and allowed. With the above, the block rate. |
| `barbacane_waf_duration_seconds{method,path}` | Time spent inspecting, so the WAF's share of latency is visible rather than inferred. |
| `barbacane_waf_response_body_skipped_total{method,path}` | Responses whose body phase-4 rules did not inspect, because it was streamed or over `max_response_body`. Response headers were still inspected. |
| `barbacane_waf_audit_total{method,path}` | Per-transaction audit records written, governed by the `audit` policy. |

`barbacane_waf_matched_total` counts every rule that matched, including the
control-flow rules CRS uses to gate paranoia levels. Those are `pass,nolog`
rules whose only job is to skip a block of higher-paranoia rules, and they
match on most requests, so they dominate the counter by volume. Filter them out
when reading the tuning signal: the rule ids that matter are the ones that
carry a score.

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
