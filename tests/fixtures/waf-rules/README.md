# WAF integration test rule set

A compact SecLang rule set in OWASP CRS idiom, used by
`crates/barbacane-test/tests/waf.rs` to exercise the gateway's WAF stage:
config parsing, compile-time sealing, `@pmFromFile` data loading, header and
body collection, anomaly-score accumulation and the block response.

It is not CRS and is not a detection benchmark. CRS conformance is measured by
parapet's go-ftw suite. Rule ids live in the `10011xx`-`10018xx` range so they
cannot be confused with real CRS rules, and the blocking rule `1009110`
mirrors the role of CRS `949110`. Scores accumulate in
`tx.blocking_inbound_anomaly_score`, as they do in CRS 4.

Scores are tuned to the `inbound: 5` threshold the fixture specs declare:

| Rule    | Class                    | Score |
|---------|--------------------------|-------|
| 1001100 | SQL injection            | 5     |
| 1001200 | Cross-site scripting     | 5     |
| 1001300 | Path traversal / LFI     | 5     |
| 1001400 | Command injection        | 5     |
| 1001500 | Scanner user agent       | 5     |
| 1001600 | `debug` argument         | 3     |
| 1001610 | `X-Trace` header         | 3     |
| 1001810 | Paranoia level 2 only    | 5     |

Rule `1001700` denies directly with `status:406` instead of scoring, and
`1001800` skips to `END-FIXTURE-PL2` below paranoia level 2.

The two 3-point rules are individually below the threshold, so either alone is
recorded and allowed and both together block.
