# ADR-0031: WAF Integration (ModSecurity / OWASP CRS Compatible)

**Status:** Accepted
**Date:** 2026-09-07 (accepted 2026-09-11)

## Context

Barbacane enforces a *positive* security model: the OpenAPI/AsyncAPI spec declares what a request may look like, and anything outside the schema is rejected in ~1.2 µs. A WAF is the *negative* model: a rule set that recognises known attack shapes (SQLi, XSS, RCE, LFI, protocol abuse) regardless of whether the payload is schema-valid. The two are complementary, not redundant. A schema-valid `?q=1' OR 1=1--` passes validation and is exactly what a WAF exists to catch.

Three things make this worth deciding now:

1. **Procurement.** PCI DSS v4 and most enterprise security questionnaires ask for a WAF in front of public HTTP surfaces. Competing gateways answer yes (AWS WAF, Cloudflare, Envoy via `coraza-proxy-wasm`, Traefik via its Coraza plugin, APISIX via ModSecurity). Barbacane currently answers with `ip-restriction` and `bot-detection`, which are not a WAF.
2. **The ecosystem converged on one rule language.** ModSecurity's SecLang plus the OWASP Core Rule Set is the de facto standard. Anything we build must consume CRS unmodified, or it has no distribution story.
3. **[Coraza](https://github.com/corazawaf/coraza) exists and is Apache-2.0**, as is CRS. A Go implementation of SecLang with a WASM build already ships. The question is whether we can reuse it.

### The off-the-shelf artifact does not fit our runtime

`coraza-proxy-wasm` is built against the **proxy-wasm ABI** (`proxy-wasm-go-sdk` v0.24), TinyGo `-target=wasip1 -gc=custom -scheduler=none -opt=2`, build tags `no_fs_access memoize_builders`, with CRS embedded via `Include @owasp_crs/*.conf`. Barbacane's plugin ABI is unrelated: JSON in linear memory, exports `init` / `alloc` / `on_request` / `on_response` / `dispatch`, imports `host_*` (see [ADR-0016](0016-plugin-development-contract.md) and `crates/barbacane-wasm/src/instance.rs`). The binary cannot be loaded as-is.

### Runtime prerequisites (P1-P3)

Independent of which option we choose, three current invariants block *any* heavyweight plugin. They are generic runtime debt and should be fixed regardless.

**P1. Instances are created and `init()`-ed per request, per middleware.**
`InstancePool::get_instance` (`crates/barbacane-wasm/src/pool.rs:223`) constructs a fresh `PluginInstance` and calls `init(config)` on every invocation from `crates/barbacane/src/main.rs:1576`. Modules are AOT-compiled once and cached, so this is cheap for today's plugins. It is fatal for a WAF: `init` would parse and compile ~27 CRS rule files (regex programs plus multi-pattern automata) on every request. **P1 is the hard blocker.**

**P2. The memory cap is engine-global and too small.**
`PluginLimits` defaults to 16 MB with a 100 ms / 100 M-fuel budget (`crates/barbacane-wasm/src/limits.rs`), and the data plane installs a single limit for all plugins (`crates/barbacane/src/main.rs:733`). Upstream reports roughly 40 MB of resident memory per Coraza WAF instance with CRS loaded ([coraza#975](https://github.com/corazawaf/coraza/issues/975): 50 instances ≈ 2 GB). Raising the global ceiling to accommodate one plugin hands 64 MB to all 33 of them. Limits must become per-plugin, declared in `plugin.toml` and carried in `PluginCapabilities`.

**P3. The WASI surface is seven hand-written stubs.**
`random_get`, `clock_time_get`, `fd_write`, `sched_yield`, `environ_get`, `environ_sizes_get`, `proc_exit` (`crates/barbacane-wasm/src/instance.rs:2213-2371`), with no `define_unknown_imports_as_trap`. A TinyGo `wasip1` binary also imports `args_get`, `args_sizes_get`, `fd_close`, `fd_seek`, `fd_fdstat_get` and `poll_oneoff`; instantiation fails on the first missing one. `wasmtime-wasi` is already a declared workspace dependency that nothing uses.

Secondary: fuel accounting. CRS *evaluation* is sub-millisecond, but a Go guest runs `nottinygc` collection cycles inside request handling, which burns fuel non-deterministically. A Go-based plugin needs its own fuel budget and tolerance for GC pauses inside the 100 ms wall clock.

## Options considered

### Option A: implement the proxy-wasm host ABI

Add a proxy-wasm host implementation to `barbacane-wasm` and load `coraza-proxy-wasm` unmodified.

- **Upside:** upstream binary, upstream releases, zero WAF code of our own.
- **Cost:** roughly 50 `proxy_*` host functions, the proxy-wasm context lifecycle (`proxy_on_context_create` / `proxy_on_done` / `proxy_on_delete`), the header-map pairs wire encoding, phase dispatch to `proxy_on_request_headers` / `_body` / `proxy_on_response_*`, plus `proxy_on_vm_start` / `proxy_on_configure`. This is a second, permanent, externally-versioned plugin ABI in the trust boundary, maintained forever, used by one plugin.
- **Also:** it does not solve P1 or P2, and it imports the open memory-leak reports on that filter ([#249](https://github.com/corazawaf/coraza-proxy-wasm/issues/249), [#219](https://github.com/corazawaf/coraza-proxy-wasm/issues/219)).

**Rejected.** The ABI surface is out of proportion to the benefit, and Barbacane's ABI stability guarantees would then be entangled with Envoy's.

### Option B: Coraza sidecar over `host_http_call`

A `waf-coraza` middleware plugin that forwards request metadata (and body, with `body_access = true`) to a Coraza sidecar and acts on the verdict.

Direct precedent: `plugins/opa-authz` does exactly this for OPA, another Go engine we chose not to embed (`capabilities.host_functions = ["http_call"]`).

- **Upside:** no runtime changes required, full unmodified CRS, days of work, and it answers the procurement question immediately. The sidecar is a thin Go service around Coraza's `http.Handler` plus a check endpoint.
- **Downside:** one extra network hop per request (roughly 0.3-1 ms over loopback or a Unix socket, versus Barbacane's ~1.2 µs validation path), an extra process to deploy and monitor, request bodies crossing a process boundary, and it contradicts the edge-ready single-binary story. Response body inspection means shipping the body out and back a second time.

### Option C: TinyGo plugin wrapping the Coraza library on Barbacane's ABI

Do not reuse the proxy-wasm binary; reuse the Go *library*. A small TinyGo module (~300 lines) embeds `coraza/v3` plus `coraza-wasilibs` plus CRS and exports `init` / `alloc` / `on_request` / `on_response`.

The phase mapping is clean, and `execute_on_request` / `execute_on_response` operate on the same `instances` slice (`crates/barbacane-wasm/src/chain.rs`), so a Coraza `Transaction` survives the request/response round trip in guest memory:

| Coraza phase | Barbacane hook |
|---|---|
| `ProcessConnection`, `ProcessURI`, `ProcessRequestHeaders` | `on_request` |
| `ProcessRequestBody` | `on_request` (requires `body_access = true`) |
| `ProcessResponseHeaders`, `ProcessResponseBody` | `on_response` |
| `ProcessLogging` | `on_response` |

- **Upside:** in-process, no sidecar, upstream engine and upstream CRS semantics, no new host ABI.
- **Cost:** requires P1, P2 and P3 all delivered. Adds TinyGo 0.34.0 (pinned; upstream states higher versions are unsupported) to the release toolchain. Ships a ~12 MB WASM blob in the artifact (within the 64 MB `MAX_PLUGIN_WASM_BYTES` cap in `crates/barbacane-compiler/src/download.rs`) and holds ~40 MB of guest memory per instance. Brings a GC into the request path, which is precisely what "no garbage collector, no latency surprises" promises customers we do not have.

### Option D: native FFI to an existing engine

Two shapes exist:
- `coraza-rs` / `coraza-sys` ([ferronweb/coraza-rs](https://github.com/ferronweb/coraza-rs), Apache-2.0): compiles Coraza's Go code into a static library via cgo and binds it with bindgen. Requires Go 1.21+, a C compiler and libclang at build time, and links the Go runtime into `barbacane`.
- the `modsecurity` crate: a safe wrapper over libmodsecurity (C++), requiring that library at link time.

**Rejected**, on the precedent set by [ADR-0028](0028-ldap-auth-http-proxy.md): C/C++ (or here, cgo) FFI in the trust boundary, system libraries at link time, and a non-self-contained binary on edge targets. Linking the Go runtime into the data plane also reintroduces a GC into a process whose selling point is not having one.

### Rust ecosystem survey: is there a Coraza equivalent?

Checked before proposing Option E, because "write it ourselves" is only defensible if nobody has. Swept crates.io (`waf`, `seclang`, `modsecurity`, `coreruleset`, `owasp-crs`, `web application firewall`, sorted by downloads) and GitHub (`language:Rust` on `seclang`, `modsecurity`, `coreruleset`, `waf+crs`, plus the 244 top-starred Rust `waf` repositories).

The result: **there is no Rust equivalent of Coraza.** The landscape falls into four buckets, none of which is a mature pure-Rust SecLang engine.

| Bucket | Representative crates / repos | Verdict |
|---|---|---|
| FFI over libmodsecurity (C++) | `modsecurity` 1.0.0 (22.9k dl), `modsecurity-sys`, `modsecurity-rs`, `rust-modsecurity` (18★), `actix-modsecurity` | Most downloaded and most mature, but all C++ FFI. Rejected as Option D. |
| FFI over Coraza (cgo static lib) | `coraza` / `coraza-sys` 3.7.0 (423 dl), [ferronweb/coraza-rs](https://github.com/ferronweb/coraza-rs) | Links the Go runtime into the binary. Rejected as Option D. |
| Rust WAFs with their own rule format | [openprx/prx-waf](https://github.com/openprx/prx-waf) (16★, Pingora, 463 commits: own YAML plus a "basic subset" `.conf` parser, does not load unmodified CRS), [AarambhDevHub/pingora-waf](https://github.com/AarambhDevHub/pingora-waf) (48★, YAML feature toggles, no SecLang), `waf-proxy` / `Light-WAF`, `eva-ics/gateryx` (82★) | Not SecLang engines. No CRS supply chain, no FTW conformance, so no procurement answer. |
| Pure-Rust SecLang attempts | [zentinelproxy/zentinel-modsec](https://github.com/zentinelproxy/zentinel-modsec) (16★, 45 commits, Apache-2.0, 91.6% of 5,033 CRS cases in `DetectionOnly`), `zentinel-agent-zentinelsec` (4★), `seclang` 0.0.2 (29 downloads, a stub) | The only genuine attempts. One is a useful design reference; none is a dependency for a security-critical path. |

Two conclusions follow, and they pull in opposite directions:

- **The gap is real, and it is a moat.** Every Rust proxy that advertises a WAF today either shells out to C++ via FFI or ships a bespoke rule format that cannot consume the CRS supply chain. A native Rust SecLang engine in a spec-driven gateway has no equivalent to compare against.
- **The gap is also a warning.** Nobody has finished this in Rust. The most credible attempt reports 91.6% conformance in detection-only mode after 45 commits, which is a fair bracket on the effort: the parser and the common operators come quickly, and the remaining conformance is where the months go. This is the evidence behind making conformance a release gate and keeping Option C as the retreat.

### Option E: implement the SecLang engine natively in Rust

A new workspace crate, `barbacane-waf`: a SecLang parser and rule engine that consumes unmodified CRS, compiled **at `barbacane compile` time** and sealed into the `.bca` artifact.

This is the option that fits the architecture rather than working around it.

**Why it is tractable in Rust specifically:**

- **CRS is already RE2-shaped, and this is now measured, not assumed.** Coraza evaluates `@rx` with Go's `regexp` (RE2), so CRS maintains RE2 compatibility as a hard constraint. 268 of 273 unique CRS `@rx` patterns compile unmodified under the Rust `regex` crate, and all 273 do after a lexical repair pass. See the feasibility probe below. The `regex` crate also gives the same linear-time guarantee, so there is no ReDoS surface from operator-supplied rules.
- **`@pm` / `@pmFromFile` is Aho-Corasick**, and `aho-corasick` is a first-class Rust crate.
- **`@detectSQLi` / `@detectXSS` is libinjection**, adopted as a pure-Rust port ([`barbacane-dev/libinjectionrs`](https://github.com/barbacane-dev/libinjectionrs), BSD-3-Clause), so no C dependency. The 2026-09-08 audit found it diverged from the C original on 1,631 of 162,963 inputs with an open false-positive surface, verdict "do not adopt yet". The fork closed that: it is differential-tested against the C library through an FFI harness over the same corpus and now matches on every input, both verdicts and fingerprints (0 of 162,963 each), with any divergence failing CI. Six char-semantics classes were fixed following the C control flow, and a long differential-fuzzing campaign hardens the surface beyond the corpus.
- **`@ipMatch` is `ipnet`.** Multipart parsing is `multer`. JSON body parsing is `serde_json`, already a dependency.
- **A conformance suite exists for free.** The CRS regression corpus is 322 YAML files (roughly 5,000 cases) driven by [`go-ftw`](https://github.com/coreruleset/go-ftw). We do not have to invent a definition of "CRS compatible"; we have to pass someone else's.
- **Prior art to read, not to depend on:** `zentinel-modsec`, per the survey above. A useful existence proof and a source of design decisions, not a dependency we would take on for a security-critical path at that maturity.

**Why compile-time rule compilation is the point.**

Barbacane already compiles the spec into a sealed, hashed, signable artifact and pre-compiles per-operation validators held in server state (`crates/barbacane/src/main.rs:654-655`, `:932`). A WAF rule set is config of exactly the same kind, and putting it through the same pipeline yields four things no other integration can offer:

1. **Per-request rule compilation disappears.** The artifact carries the rule set already parsed, validated and canonicalised; the data plane builds the regex programs and Aho-Corasick automata once at startup and reuses them for every request. Per-request cost is evaluation only.

   Note the correction: an earlier draft of this ADR said the compiled automata themselves would be serialised into the artifact and memory-mapped. They cannot be. The `regex` crate does not expose a compiled program for serialisation, and neither does the `aho-corasick` builder in the form Parapet uses. Nothing that matters depends on it: the compile-time refusal of unknown directives, the rule-set hashing into `artifact_hash`, and the elimination of per-request compilation all hold when the artifact carries the validated rule set rather than the built automata. Only the startup cost moves, from zero to **316 ms**, once per process.

   That figure is measured (`parapet-conformance seal` against CRS v4.9.0), and it is higher than the 155 ms an earlier draft quoted, because 155 ms covers regex construction alone and the rest is the Aho-Corasick automata for the 23 `@pmFromFile` operators. The sealed rule set itself is 600 KiB of JSON, 1.03x the size of the source `.conf` files, which is what the artifact grows by.

   316 ms per process is accepted. Artifact reload is infrequent in practice, and the cost is paid off the request path either way, so this does not constrain the pipeline stage's design. Revisit only if reload frequency changes.
2. **Unknown SecLang becomes a compile error, never a silent skip.** This is the security-critical part. An engine that ignores a directive it does not understand converts a CRS rule into a bypass, silently. Barbacane can refuse to produce an artifact instead, which is the correct failure mode for a compiler and is impossible for a runtime-loading WAF.
3. **The rule set is covered by artifact integrity.** Rule hashes fold into `Manifest::artifact_hash` and therefore into the Ed25519 signature ([ADR-0021](0021-config-provenance.md)), so "which rules is this gateway running" is answerable from the artifact.
4. **Spec-aware rule scoping becomes possible.** Because the compiler holds the OpenAPI schema and the rule set at the same time, it can attach rule groups per operation, drop rules whose attack class the schema already rejects for that operation, and present path parameters and validated body fields to the engine as named `ARGS` rather than as one opaque blob. "Your spec tunes your WAF" is a claim that requires engine-level access, and it is the actual differentiator.

**Feasibility probe.** The largest assumption in Option E is that CRS's regex corpus compiles under the Rust `regex` crate. Measured rather than assumed.

Inputs, so the numbers below can be reproduced or challenged: CRS v4.9.0 (the release tag, `coreruleset/coreruleset`), `regex` 1.12.3, Go 1.27.0 for the RE2 cross-check, run 2026-09-07. The harness is in the Parapet repository under `crates/parapet-conformance/`, and its README carries the exact commands; CI reruns all of it against the pinned CRS version on every commit, so the figures cannot drift silently from the code.

Extracted every `@rx` pattern from `rules/*.conf` (660 `SecRule` plus 7 `SecAction` across 25 files): 299 patterns, 273 unique.

| Result | Count |
|---|---|
| Compile unmodified under `regex` 1.12.3 | 268 / 273 |
| Fail to compile | 5 |
| Failures needing PCRE-only features (lookaround, backreferences) | **0** |
| Failures from exceeding the default program size limit | 0 |
| Accepted by Go/RE2, the engine Coraza uses | 273 / 273 |

All 5 failures are `regex-syntax` lexical strictness, not CRS depending on PCRE semantics:

- **4 rules** (933170, 933180, 932170, 941380) use a literal `{` or `}` that does not form a counted repetition, for example `{{.*?}}` (941380) and `^\(\s*\)\s+{` (932170). PCRE and RE2 treat those as literals; `regex-syntax` requires them escaped.
- **1 rule** (921200) redundantly escapes punctuation inside a character class (`[^:\(\)\&\|\!\<\>\~]`), which PCRE accepts and `regex-syntax` rejects.

Go/RE2 accepting all 273 confirms the diagnosis: the gap is our lexer's strictness, not the rule set's expressiveness.

A repair pass of roughly 60 lines takes the corpus to **273 / 273**. It is structured as repair-on-failure (try the pattern as authored, rewrite only if that fails), so it is a no-op on the 268 by construction. An earlier normalise-always version silently rewrote 72 already-valid patterns, which is 72 unverified semantic changes, and is the wrong shape for a security control.

The 5 repairs were differential-tested against Go/RE2 verdicts for the original patterns over 297,129 inputs per-rule (the CRS regression payloads for those rule IDs, alphabet-biased random strings, and single-edit mutations of known positives, giving 1,838 to 6,026 matching inputs per rule): **0 disagreements**. The repairs are behaviourally identical to the originals under RE2.

Compiling the whole `@rx` corpus takes 155 ms in Rust (67 ms of it in rule 941170 alone) and 18 ms in Go. Negligible as a build step, and an independent confirmation of P1: a runtime-loading design would pay 155 ms of regex compilation per request before evaluating a single rule.

**Scope is bounded by what CRS uses, not by what SecLang defines.** The same sweep inventoried the rest of the surface:

| Surface | Distinct, in CRS v4.9.0 | Notes |
|---|---|---|
| Operators | **18** | `@rx` 299, `@lt` 182, `@eq` 50, `@ge` 42, `@pmFromFile` 23, `@gt` 12, `@pm` 10, `@within` 9, `@endsWith` 7, `@validateByteRange` 6, `@streq` 5, `@contains` 4, `@validateUrlEncoding` 3, `@ipMatch` 2, `@detectXSS` 2, `@detectSQLi` 2, `@unconditionalMatch` 1, `@validateUtf8Encoding` 1 |
| Transformations | **20** (plus `none`) | `urlDecodeUni` 135, `lowercase` 42, `htmlEntityDecode` 39, `jsDecode` 37, `utf8toUnicode` 31, `removeNulls` 27, `cssDecode` 23, `cmdLine` 12, tail of 12 more. Note `normalisePath` appears as a British-spelling alias of `normalizePath`. |
| Variables / collections | **32** | `REQUEST_COOKIES`, `TX`, `ARGS`, `XML`, `ARGS_NAMES`, `REQUEST_HEADERS`, `REQUEST_FILENAME`, `RESPONSE_BODY`, `MULTIPART_PART_HEADERS`, `FILES*`, down to `UNIQUE_ID` and `FILES_COMBINED_SIZE` at 1 use each (which are precisely the two gaps `zentinel-modsec` documents as unimplemented) |
| `ctl:` actions | **6** | `ruleRemoveByTag`, `forceRequestBodyVariable`, `auditEngine`, `ruleRemoveTargetById`, `requestBodyProcessor`, `ruleRemoveTargetByTag` |
| Phases | 5 | phase 2: 272 rules, phase 1: 170, phase 4: 100, phase 3: 39, phase 5: 13 |
| Directives in `rules/` | 4 | `SecRule`, `SecMarker`, `SecAction`, `SecComponentSignature` (plus the setup-file directives) |

Eighteen operators, not the 35+ SecLang defines. This is the scope of "CRS-compatible", and it is materially smaller than "implement SecLang".

The harness that produced these numbers (extractor, Rust checker, Go/RE2 cross-check, differential tester) is the seed of the conformance suite and belongs in the engine repository (see below), not here.

**What it costs.** A SecLang parser; the 18 operators and 20 transformations above; the 32-entry variable/collection model; the action set including `chain`, `skipAfter`, `setvar`, `capture`, the 6 `ctl:` actions and anomaly scoring; and the 5-phase model. The probe removes the regex engine from the risk column entirely and bounds the operator and transformation surface, so the estimate lands at 10-15k lines of Rust plus the conformance harness: still multi-month for one engineer, but no longer speculative.

The residual risk is unchanged and is not in the first 90%. It is in multipart parsing edge cases, `XML` selection (175 target references, the heaviest single collection after cookies and `TX`), persistent collections, and `MATCHED_VARS` semantics. A WAF at 92% CRS conformance has a security-relevant failure mode: a rule that does not fire is a silent bypass. Conformance is therefore a release gate, not a dashboard metric.

## Decision

Stage the work. Ship the prerequisites, ship a bridge, build the strategic option behind a conformance gate.

**Stage 0: runtime prerequisites (P1-P3).** Do this regardless of the WAF outcome.
- P1: reuse instances keyed on `(plugin, config)` so `init` runs once per key rather than once per request. Per-request instantiation is a cost every plugin pays today. The pooling is the easy half; the contract is the real work. A reused instance carries whatever the guest left in its linear memory, so reuse needs exclusive checkout for the duration of a request, request-scoped state created fresh per request rather than carried over, and a defined reset before an instance returns to the pool. Without that, one request can observe another's state, which is worse than the cost being fixed. Tracked in [#138](https://github.com/barbacane-dev/barbacane/issues/138).
- P2: per-plugin `[limits]` (memory, fuel, wall clock) in `plugin.toml`, carried through `PluginCapabilities` into the manifest and enforced per instance.
- P3: replace the seven stubs with `wasmtime-wasi` (already a workspace dependency), or keep the stubs and call `Linker::define_unknown_imports_as_traps(&module)`, which gives every unresolved import a trapping stub so instantiation succeeds and the trap fires only if the guest actually calls it. That turns "this plugin will not load" into "this plugin traps on the call it should not have made", which is both easier to diagnose and safer to default to.

**Stage 1: `waf-coraza` (Option B), marked experimental.** A middleware over `host_http_call` against a Coraza sidecar. No runtime changes needed, full CRS semantics, and it answers procurement while Stage 2 is built. It also tells us whether anyone actually turns a WAF on before we spend months on one.

**Stage 2: `barbacane-waf` (Option E) as the strategic target.** New workspace crate, SecLang compiled at `barbacane compile` time into a new `.bca` section, evaluated by a native pipeline stage alongside the existing validators. Configuration is declared on the operation in the usual way:

```yaml
paths:
  /orders:
    post:
      operationId: createOrder
      x-barbacane-waf:
        ruleset: ./crs-4.9.0/rules    # directory of SecLang .conf files
        paranoia_level: 1
        mode: blocking                # blocking | detection-only
        thresholds: { inbound: 5, outbound: 4 }
        unsupported_rules: fail       # fail (default) | skip
        spec_scoped: false            # Stage 3; see the warning below
        exclusions:
          - rule_id: 942100
            target: "ARGS:query"
```

`ruleset` is a path to a directory of SecLang files, resolved relative to the
spec that declares it, and the compiler hashes the sealed result into
`artifact_hash`. A floating version alias such as `owasp-crs@4`, which an
earlier draft used, is deliberately not the interface: it would make two builds
of the same spec enforce different rules. If a bundled-ruleset shorthand is
added later it has to resolve to an immutable pinned version, recorded in the
artifact, or reproducible builds are lost.

`spec_scoped` defaults to **off**, and should stay off until Stage 3 has an
audited mapping from rule to schema coverage. The feature drops WAF rules on
the grounds that the schema already rejects that class of input, so a wrong
mapping silently removes protection, which is the exact failure this ADR is
built to avoid elsewhere. The safe default when coverage is uncertain is to
keep the rule. Enabling it by default before that mapping exists and is tested
would trade the ADR's central property for a latency saving.

Release gates for Stage 2 GA:

- **`@detectSQLi` and `@detectXSS` implemented.** These are a GA prerequisite, not an optional extra. Without them the CRS rules 941100, 941101, 942100 and 942101 cannot be enforced, and those are the libinjection classifiers, not peripheral rules. Met: both operators are implemented in Parapet over `libinjectionrs`, whose corpus differential against the C library is at zero.
- **100% of the CRS regression suite** for the enabled paranoia level, in blocking mode, via `go-ftw`, **with an empty exclusion list**. An earlier draft of this ADR asked for 100% while two operators were refused, which is unreachable: those four rules account for 30 stages of the suite. Rather than define a reduced suite, GA requires the exclusion list to be empty, so the gate cannot be met by shrinking the target.
- Until GA, the shortfall is reported rather than hidden: the compiler refuses a rule set it cannot fully enforce unless the operator opts in with `unsupported_rules: skip`, and the artifact then records the skipped rule ids in the manifest, where `artifact_hash` and the signature cover them. An operator can prove which rules a running gateway is not enforcing.
- Unknown directive, operator, transformation, action or target is a hard compile error. No silent skips, ever.
- Rule evaluation budget enforced per request, with a documented p99 for CRS PL1.
- Rule set hashes folded into `artifact_hash`.

### Stage 2 lives in its own repository, under Apache-2.0 / MIT

The engine does not belong in this repository, and the decisive reason is licensing, not ecosystem goodwill.

**Barbacane is AGPLv3 plus a paid commercial license ([LICENSING.md](../LICENSING.md)), and it takes contributions under a DCO, not a CLA ([CONTRIBUTING.md](../CONTRIBUTING.md)).** DCO clause (a) certifies only that the contributor has the right to submit the work *under the license indicated in the file*, which here is AGPLv3. It assigns no copyright and grants no relicensing right. Selling a commercial license for code requires owning it or holding a grant that permits relicensing, and a DCO sign-off is neither.

So an outside contribution to in-tree AGPL code cannot be offered under `COMMERCIAL-LICENSE` without going back to that contributor for permission. Today that risk is small because contributions are few and mostly internal. A SecLang engine inverts it: rule semantics, CRS conformance fixes and operator edge cases are precisely the kind of work outside contributors show up for, and it is the largest single body of code in the project. Keeping it in-tree maximises the chance that the commercial offering accumulates code it cannot legally relicense.

An Apache-2.0 crate dissolves the problem rather than managing it:

- Apache-2.0 is one-way compatible with AGPLv3, so this repository can consume the crate with no friction.
- Apache-2.0 already permits commercial use, so commercial licensees are covered without Barbacane owning the copyright.
- Contributors need no CLA, and no copyright assignment has to be negotiated.
- `MIT OR Apache-2.0` is the Rust ecosystem norm and the precondition for anyone outside AGPL-land depending on it.

**The boundary.** The engine is the commodity; the compile-time integration is the differentiator. That line is also the repository line.

| Separate repo, Apache-2.0 / MIT | This repo, AGPLv3 + commercial |
|---|---|
| SecLang parser and rule AST | The `x-barbacane-waf` spec extension |
| The 18 operators, 20 transformations, variable model | Serialisation of the compiled program into the `.bca` section |
| Phase execution, anomaly scoring, `ctl:` handling | Rule-set hashing into `artifact_hash` and the signature |
| A host-agnostic transaction interface (headers, body, URI, client IP in; disruptive action out) | The native pipeline stage and its telemetry |
| The CRS conformance harness and `go-ftw` integration | **Spec-aware rule scoping (Stage 3)** |

The left column is `barbacane-dev/parapet`. Its README leads with the generic API and does not mention Barbacane above the fold, so that other proxies read it as a library rather than as a Barbacane component.

Nothing in the left column is a moat. Coraza already gives that away under Apache-2.0. Everything that makes Barbacane's WAF different from running Coraza behind any proxy sits in the right column and stays under the existing dual license.

**Accepted downside.** Apache-2.0 means Kong, Traefik, Pingoo and proprietary gateways can embed the engine. This is the correct trade: the alternative is an AGPL crate nobody outside AGPL-land adopts, which forfeits the contributors and the external conformance pressure that are the whole point of extracting it. Becoming the standard Rust SecLang engine is worth more than denying it to proxies that can already use Coraza.

**Sequencing.** Start the separate repository at the first commit. Retrofitting a relicense after outside contributions have landed means chasing per-contributor permission, which is the exact problem being avoided. During Stage 2 development this repository depends on it by git revision; the dependency moves to crates.io when the API stabilises. The conformance harness from the feasibility probe belongs there, not here.

**Naming and status.** The engine is **Parapet**, at `barbacane-dev/parapet`, dual-licensed `MIT OR Apache-2.0`. Repository created 2026-09-07; the `@rx` compatibility layer from the feasibility probe is its first commit, with the conformance harness alongside it.

The GitHub repository name is settled. The **crates.io name `parapet` is not obtainable**, and this is a policy fact rather than a matter of waiting. It is held by an abandoned placeholder (a 2016 "peer to peer build system", two versions, last published 2016-09-15, upstream repository untouched since 2018), and crates.io names are permanent:

- Names are allocated first-come and cannot be reused. Yanking does not release a name ([Cargo book, Publishing](https://doc.rust-lang.org/cargo/reference/publishing.html)).
- crates.io defines no squatting policy and will not reassign a name on those grounds.
- [RFC 3646](https://rust-lang.github.io/rfcs/3646-remove-crate-transfer-mediation-policy.html) removed the team's transfer mediation. The documented instruction when an owner is unresponsive is to pick a different name, and mediation requests are declined.

The only path is the current owner volunteering the name. Worth one polite ask, since the owner is an active member of the Rust community and the crate is a decade-dead placeholder, but it cannot be planned around and there is nothing to monitor: no institutional process will ever free the name, so a poll on the name's availability can only fire if the owner acts, in which case he replies directly.

**The crate therefore publishes as `parapet-waf`** unless that ask succeeds. The repository stays `parapet`. A repository and crate differing by a suffix is common and costs nothing. `parapet-core` and `parapets` are also free.

Note the asymmetry in switching cost: with one commit, no published crate and no dependents, renaming the whole project today is free, and it gets expensive the moment anything depends on it. Any preference for a clean single-word crate name (`crenel`, `mantlet` and `embrasure` are free on crates.io) should be exercised now rather than later.

Names deliberately avoided: anything containing `modsec` or `crs` (ModSecurity and OWASP are trademarks of their respective owners), and `coraza-rs` (implies affiliation with a project this shares no code with).

This is consistent with [ADR-0017](0017-repository-architecture.md), which keeps official *plugins* in the monorepo and lists "a community emerges that wants to contribute without core access" as a trigger to reconsider. A general-purpose library that happens to have Barbacane as its first consumer is a different axis from a Barbacane plugin, and the monorepo decision does not reach it.

**Stage 3: spec-aware scoping.** Rule scoping per operation, schema-informed rule elimination, and named `ARGS` derived from `path_params` and validated body fields.

**Option C is the documented fallback.** If Stage 2 conformance stalls below the gate, the TinyGo wrapper on Barbacane's ABI is the retreat position, and Stage 0 has already made it viable.

**Options A and D are rejected** for the reasons stated above.

## Consequences

**Easier**
- Barbacane answers "do you have a WAF" with a bundled, spec-integrated one instead of a referral to a cloud provider.
- Stage 0 removes per-request instantiation and a global memory ceiling for all 33 existing plugins, not just the WAF.
- Compile-time rule compilation puts rule sets under the same integrity, signing and provenance guarantees as routes.
- Positive (schema) and negative (CRS) models sit in one process, one artifact, one config surface.

**Harder**
- Stage 2 is the largest single piece of net-new logic in the project, and it is security-critical: a bypass in the engine is a vulnerability, not a bug.
- We take on CRS version tracking as an ongoing obligation, including the operational burden of false positives and per-deployment tuning. Upstream's own warning applies to us: a WAF is not plug-and-play.
- Two WAF implementations coexist during Stages 1 and 2, with different semantics and different failure modes, and the deprecation of `waf-coraza` has to be managed.
- `.bca` artifacts grow by the sealed rule set: 600 KiB for CRS v4.9.0, 1.03x the source `.conf` files.

**Licensing.** Coraza and CRS are both Apache-2.0, compatible as inputs to an AGPLv3 project. `libinjectionrs` is BSD-3-Clause. Option E derives from the SecLang *language* and the CRS *rule set*, not from Coraza's Go source, so no copyleft or attribution issue beyond CRS's Apache-2.0 notice.

## Alternatives considered

Summarised above: Option A (proxy-wasm host ABI) rejected as disproportionate permanent ABI surface; Option D (cgo/C++ FFI) rejected on the [ADR-0028](0028-ldap-auth-http-proxy.md) precedent; Option C retained as the fallback to Option E.

Also considered and rejected: a hand-rolled attack-signature middleware with no SecLang compatibility. Cheap to build, but a WAF that cannot consume CRS has no rule supply chain, no conformance suite, and no procurement answer.

## Competitive comparison

| Gateway | WAF | Model |
|---|---|---|
| Envoy | `coraza-proxy-wasm` | proxy-wasm plugin, Go/TinyGo, runtime rule load |
| Traefik | Coraza WASM plugin | WASM plugin, runtime rule load |
| APISIX | ModSecurity | native C++ library |
| Kong | Enterprise WAF | proprietary |
| AWS / Cloudflare | Managed WAF | separate service, per-request billing |
| Pingoo (Rust, 1k★) | built-in WAF | native Rust, own rule format, no SecLang (zero `SecRule` hits in-tree) |
| prx-waf / pingora-waf (Rust) | built-in WAF | native Rust, own YAML format, no unmodified CRS |
| **Barbacane (proposed)** | `barbacane-waf` | native Rust, unmodified CRS compiled into the signed artifact, scoped by the spec |

The Rust-native gateways that ship a WAF do so with their own rule format, which is cheaper to build and worthless as a procurement answer. No competitor, in any language, compiles the rule set at build time or scopes rules from an API contract, because none of them has a compile step or the contract.

## Open questions

- CRS distribution: bundle in the binary, fetch as an OCI artifact ([ADR-0027](0027-oci-artifact-distribution.md)), or both? OCI fits rule-set updates decoupled from Barbacane releases.
- Persistent collections (`IP`, `SESSION`, `USER`) require cross-request state, which the data plane is explicitly stateless about. Reuse the `rate_limit` / `cache` backends, or declare CRS's stateful rule groups unsupported and fail compilation on them?
- Does `x-barbacane-waf` belong on the operation, at the root, or both with operation-level override? Root-level default with per-operation override matches how `mcp` is handled.
- Response body inspection (CRS phase 4) interacts with streaming ([ADR-0023](0023-wasm-plugin-streaming.md)): a streamed response is already on the wire before phase 4 could block it. Detection-only for streamed responses, or refuse to combine the two?
- Is `waf-coraza` (Stage 1) worth publishing at all if Stage 2 is committed, given the deprecation cost?
- ~~Does the engine repository sit in the `barbacane-dev` org or under a neutral name?~~ Resolved: `barbacane-dev/parapet`, with a deliberately Barbacane-agnostic README.
- Barbacane's own dual-licensing exposure under DCO is broader than the WAF: does the project want a CLA for in-tree contributions generally, or to keep DCO and accept that contributed code is AGPL-only? Out of scope here, but this ADR is the second time it has come up.
- ~~Which libinjection route: adopt `libinjectionrs`, or port the fingerprint tables?~~ Resolved: adopt the [`barbacane-dev/libinjectionrs`](https://github.com/barbacane-dev/libinjectionrs) fork. The divergence surface the audit flagged is now bounded: the corpus differential against the C library is at zero on both verdicts and fingerprints, gated in CI, and a long differential-fuzzing campaign drives the surface beyond the corpus. Parapet implements `@detectSQLi` and `@detectXSS` over it, so the 30 regression stages those four rules cover are enforced rather than refused.

## Related ADRs

- [ADR-0009: Security Model](0009-security-model.md)
- [ADR-0016: Plugin Development Contract](0016-plugin-development-contract.md)
- [ADR-0017: Repository Architecture](0017-repository-architecture.md)
- [ADR-0021: Config Provenance](0021-config-provenance.md)
- [ADR-0023: WASM Plugin Streaming](0023-wasm-plugin-streaming.md)
- [ADR-0027: OCI Artifact Distribution](0027-oci-artifact-distribution.md)
- [ADR-0028: LDAP Auth via HTTP Proxy](0028-ldap-auth-http-proxy.md)
