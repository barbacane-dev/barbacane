//! The WAF pipeline stage.
//!
//! Runs after spec validation and before the middleware chain, which is the
//! order the two models want: the spec is a positive model that says what a
//! request may look like, and the rule set is a negative model that recognises
//! attack shapes regardless of whether they are schema-valid. A schema-valid
//! `?q=1' OR 1=1--` passes validation and is exactly what the rule set exists
//! to catch.
//!
//! The rule set arrives already parsed and validated from the artifact, so
//! this module compiles it once at startup and evaluates it per request.

use std::path::Path;

use barbacane_compiler::artifact::{Manifest, WafMode};
use parapet::transaction::EngineMode;
use parapet::{RuleSet, Transaction, Verdict};

/// A compiled rule set plus the policy that decides what it does.
pub struct WafStage {
    rules: RuleSet,
    mode: EngineMode,
    /// Seeded as `tx.blocking_paranoia_level`, which gates whole CRS rule
    /// files. Left unset it reads as 0 and CRS skips almost everything.
    paranoia_level: u8,
    inbound_threshold: i64,
    outbound_threshold: i64,
    /// Largest response body, in bytes, that phase-4 rules inspect.
    max_response_body: usize,
}

/// An inspection in progress, held between the request and response phases.
///
/// CRS accumulates anomaly scores in `TX` across phases, and the outbound
/// blocking rule reads what the inbound rules wrote, so response inspection
/// has to continue the same transaction rather than start a new one.
pub struct WafInspection<'r> {
    tx: Transaction<'r>,
}

/// What the stage decided about a request.
pub enum WafDecision {
    /// Nothing interrupted the request.
    Allow,
    /// A rule interrupted it.
    Block {
        /// Status to return.
        status: u16,
        /// The rule that interrupted, for logging and metrics.
        rule_id: u32,
        /// The rule's message, if it had one.
        message: String,
    },
}

impl WafStage {
    /// Build the stage from an artifact, if it carries a rule set.
    ///
    /// Compiles the rule set once. This is the cost the compile-time design
    /// buys down: without it the automata would be rebuilt per request.
    pub fn from_artifact(
        artifact_path: &Path,
        manifest: &Manifest,
    ) -> Result<Option<Self>, String> {
        if !manifest.waf.enabled {
            return Ok(None);
        }
        let sealed = barbacane_compiler::artifact::load_waf_rules(artifact_path)
            .map_err(|e| format!("cannot read the WAF rule set from the artifact: {e}"))?;
        let Some(sealed) = sealed else {
            return Err(
                "the manifest enables the WAF but the artifact carries no rule set".to_string(),
            );
        };

        // The manifest is authenticated by this point, but the archive
        // members are not: check the extracted bytes against the checksums
        // before compiling them, exactly as plugin WASM is checked.
        barbacane_compiler::artifact::verify_waf_rules(manifest, &sealed)
            .map_err(|e| format!("WAF rule set integrity check failed: {e}"))?;

        // The sealed rule set is its own data loader: `@pmFromFile` phrase
        // lists travel in the artifact beside the rules, so the gateway needs
        // no filesystem access to compile them.
        //
        // The compiler already refused anything unenforceable, so a failure
        // here means the artifact and this binary disagree about what SecLang
        // this build supports. Refusing to start is correct: the alternative
        // is running with rules missing and no signal.
        let rules = RuleSet::compile(&sealed.directives, &sealed).map_err(|e| {
            format!(
                "the artifact's WAF rule set cannot be compiled by this build: {e}. \
                 The artifact was built by a different version; recompile it."
            )
        })?;

        Ok(Some(WafStage {
            rules,
            mode: match manifest.waf.mode {
                WafMode::Blocking => EngineMode::Blocking,
                WafMode::DetectionOnly => EngineMode::DetectionOnly,
            },
            paranoia_level: manifest.waf.paranoia_level,
            inbound_threshold: manifest.waf.inbound_threshold,
            outbound_threshold: manifest.waf.outbound_threshold,
            // Saturate rather than truncate: a cap larger than the address
            // space means "no effective limit", never a wrapped small value
            // that would silently stop inspecting bodies it should.
            max_response_body: usize::try_from(manifest.waf.max_response_body)
                .unwrap_or(usize::MAX),
        }))
    }

    /// Largest response body, in bytes, that phase-4 rules inspect. A buffered
    /// response at or under this is collected and inspected; a larger or
    /// streamed one has its body skipped.
    pub fn response_body_cap(&self) -> usize {
        self.max_response_body
    }

    /// How many rules the stage carries.
    pub fn rule_count(&self) -> usize {
        self.rules.rule_count()
    }

    /// How many rules run on the request side (phases 1 and 2) versus the
    /// response side (phases 3, 4 and 5). Reported at startup.
    pub fn rules_by_direction(&self) -> (usize, usize) {
        use parapet::Phase::*;
        let request =
            self.rules.rules_in_phase(RequestHeaders) + self.rules.rules_in_phase(RequestBody);
        let response = self.rules.rules_in_phase(ResponseHeaders)
            + self.rules.rules_in_phase(ResponseBody)
            + self.rules.rules_in_phase(Logging);
        (request, response)
    }

    /// Whether the stage will actually interrupt a request.
    pub fn is_blocking(&self) -> bool {
        self.mode == EngineMode::Blocking
    }

    /// Inspect a request through phases 1 and 2.
    ///
    /// `remote_addr` feeds `REMOTE_ADDR`, which CRS reputation and rate rules
    /// read; passing the proxy's own address there would make those rules
    /// judge the wrong client.
    #[allow(clippy::too_many_arguments)]
    pub fn inspect_request<'r>(
        &'r self,
        method: &str,
        uri: &str,
        protocol: &str,
        headers: &[(String, String)],
        body: &[u8],
        content_type: Option<&str>,
        remote_addr: Option<&str>,
    ) -> (WafDecision, WafInspection<'r>) {
        let mut tx = Transaction::new(&self.rules, self.mode);

        // CRS gates entire rule files on these, and an unset variable reads as
        // zero, so seeding them is not optional.
        tx.set_tx(
            "blocking_paranoia_level",
            self.paranoia_level.to_string().into_bytes(),
        );
        tx.set_tx(
            "detection_paranoia_level",
            self.paranoia_level.to_string().into_bytes(),
        );
        tx.set_tx(
            "inbound_anomaly_score_threshold",
            self.inbound_threshold.to_string().into_bytes(),
        );
        tx.set_tx(
            "outbound_anomaly_score_threshold",
            self.outbound_threshold.to_string().into_bytes(),
        );

        if let Some(addr) = remote_addr {
            tx.set_remote_addr(addr.as_bytes().to_vec());
        }

        tx.process_uri(method, uri, protocol);
        for (name, value) in headers {
            tx.add_request_header(name, value);
        }
        // HTTP/2 carries the authority in the :authority pseudo-header, which
        // hyper exposes on the URI rather than as a Host header, so an h2
        // request arrives with no Host header. Synthesize one from the URI
        // authority when absent, so Host-based rules (CRS 920280 "missing Host",
        // 920350 "Host is a numeric IP", ...) see the value an HTTP/1.1 client
        // would send. add_request_header also records it in REQUEST_HEADERS_NAMES.
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"))
        {
            if let Some(authority) = authority_of(uri) {
                tx.add_request_header("Host", authority);
            }
        }
        let verdict = tx.process_request_headers();
        if let decision @ WafDecision::Block { .. } = Self::decide(&tx, verdict) {
            return (decision, WafInspection { tx });
        }

        if !body.is_empty() {
            tx.set_request_body(body, content_type);
        }
        let verdict = tx.process_request_body();
        let decision = Self::decide(&tx, verdict);
        (decision, WafInspection { tx })
    }

    fn decide(tx: &Transaction<'_>, verdict: Verdict) -> WafDecision {
        match verdict {
            Verdict::Allow => WafDecision::Allow,
            Verdict::Deny { status, rule_id } => WafDecision::Block {
                status,
                rule_id,
                message: tx
                    .matches()
                    .iter()
                    .rev()
                    .find(|m| m.id == Some(rule_id))
                    .map(|m| m.message.clone())
                    .unwrap_or_default(),
            },
        }
    }
}

impl WafInspection<'_> {
    /// Inspect the response through phases 3, 4 and 5.
    ///
    /// The body is inspected only when one is supplied. A streamed or oversized
    /// response passes `None`: its headers are inspected (phase 3) but its body
    /// is not, since it has already reached the client by the time a phase-4
    /// rule could block. CRS phase-4 rules are mostly data-leakage detection:
    /// stack traces, SQL errors, source code and credentials in a response body.
    ///
    /// Phases run in order and the engine stops phase processing after a
    /// disruptive verdict, so a phase-3 block skips phases 4 and 5 and a
    /// phase-4 block skips phase 5. Phase 5 is logging and correlation on the
    /// non-blocked path.
    pub fn inspect_response(
        &mut self,
        status: u16,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> WafDecision {
        self.tx.set_response_status(status);
        for (name, value) in headers {
            self.tx.add_response_header(name, value);
        }
        if let Some(body) = body {
            self.tx.set_response_body(body);
        }

        self.tx.process_response_headers();
        self.tx.process_response_body();
        let verdict = self.tx.process_logging();
        WafStage::decide(&self.tx, verdict)
    }

    /// The ids of every rule that matched, for the match counter. Includes
    /// rules that only scored, which is what detection-only mode needs.
    pub fn matched_rule_ids(&self) -> Vec<u32> {
        self.tx.matched_ids()
    }

    /// The inbound anomaly score the request accumulated, for logging.
    pub fn inbound_score(&self) -> i64 {
        self.tx.anomaly_score("blocking_inbound_anomaly_score")
    }
}

/// The authority (host and optional port) of an absolute URI, or `None` for an
/// origin-form URI (`/path`) that carries no authority. HTTP/2 request URIs are
/// absolute, so this recovers the `:authority` value hyper places on the URI.
fn authority_of(uri: &str) -> Option<&str> {
    let after_scheme = uri.split_once("://")?.1;
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    // Strip any userinfo; the h2 :authority has none, but be defensive.
    let authority = after_scheme[..end].rsplit('@').next().unwrap_or("");
    (!authority.is_empty()).then_some(authority)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A rule set with one request-phase rule and response-phase rules in
    /// phases 3, 4 and 5. Rule 5 matches on every response, so its presence in
    /// `matched_ids` shows phase 5 ran.
    fn stage(mode: EngineMode) -> WafStage {
        let source = r#"
SecRule ARGS "@rx attack" "id:1,phase:2,deny,status:403,msg:'inbound'"
SecRule RESPONSE_BODY "@rx secret" "id:2,phase:4,deny,status:403,msg:'outbound leak'"
SecRule RESPONSE_HEADERS:X-Debug "@rx ." "id:3,phase:3,deny,status:403,msg:'debug header'"
SecRule REMOTE_ADDR "@rx ." "id:5,phase:5,pass,nolog,msg:'phase 5 ran'"
"#;
        let directives = parapet::parse(source, "test.conf").expect("must parse");
        let rules = RuleSet::compile(&directives, &parapet::NoDataLoader).expect("must compile");
        WafStage {
            rules,
            mode,
            paranoia_level: 1,
            inbound_threshold: 5,
            outbound_threshold: 4,
            max_response_body: 1_048_576,
        }
    }

    fn inspect<'r>(stage: &'r WafStage, uri: &str) -> (WafDecision, WafInspection<'r>) {
        stage.inspect_request(
            "GET",
            uri,
            "HTTP/1.1",
            &[("Host".to_string(), "example.test".to_string())],
            b"",
            None,
            Some("203.0.113.7"),
        )
    }

    #[test]
    fn counts_rules_by_direction() {
        // Reported at startup: one request-phase rule, three response-phase
        // rules (phases 3, 4 and 5).
        let (request, response) = stage(EngineMode::Blocking).rules_by_direction();
        assert_eq!(request, 1);
        assert_eq!(response, 3);
    }

    #[test]
    fn a_request_phase_rule_blocks() {
        let stage = stage(EngineMode::Blocking);
        let (decision, _) = inspect(&stage, "/?q=attack");
        match decision {
            WafDecision::Block {
                status,
                rule_id,
                message,
            } => {
                assert_eq!(status, 403);
                assert_eq!(rule_id, 1);
                assert_eq!(message, "inbound");
            }
            WafDecision::Allow => panic!("should have blocked"),
        }
    }

    #[test]
    fn a_request_body_rule_blocks() {
        // The phase-2 ARGS rule must also see JSON body fields: the body
        // inspection path (set_request_body + process_request_body) runs when a
        // body is supplied. A JSON body is flattened into ARGS by content type.
        let stage = stage(EngineMode::Blocking);
        let (decision, _) = stage.inspect_request(
            "POST",
            "/submit",
            "HTTP/1.1",
            &[("Host".to_string(), "example.test".to_string())],
            br#"{"comment":"attack"}"#,
            Some("application/json"),
            Some("203.0.113.7"),
        );
        assert!(
            matches!(decision, WafDecision::Block { rule_id: 1, .. }),
            "an attack in a JSON body should match the ARGS rule"
        );
    }

    #[test]
    fn matched_ids_and_score_are_exposed() {
        let stage = stage(EngineMode::Blocking);
        let (_, inspection) = inspect(&stage, "/?q=attack");
        assert!(
            inspection.matched_rule_ids().contains(&1),
            "the matched rule id should be reported"
        );
        // The scoring rule ran, so a non-negative inbound score is available.
        assert!(inspection.inbound_score() >= 0);
    }

    #[test]
    fn a_clean_request_is_allowed() {
        let stage = stage(EngineMode::Blocking);
        assert!(matches!(inspect(&stage, "/?q=fine").0, WafDecision::Allow));
    }

    /// A stage whose only rule mirrors CRS 920280: block when there is no Host
    /// header.
    fn host_rule_stage() -> WafStage {
        let source = r#"SecRule &REQUEST_HEADERS:Host "@eq 0" "id:920280,phase:2,deny,status:403,msg:'missing host'""#;
        let directives = parapet::parse(source, "host.conf").expect("must parse");
        let rules = RuleSet::compile(&directives, &parapet::NoDataLoader).expect("must compile");
        WafStage {
            rules,
            mode: EngineMode::Blocking,
            paranoia_level: 1,
            inbound_threshold: 5,
            outbound_threshold: 4,
            max_response_body: 1_048_576,
        }
    }

    #[test]
    fn http2_authority_is_exposed_as_host_header() {
        // HTTP/2 carries the authority in :authority, which hyper puts on the
        // URI rather than a Host header, so an h2 request arrives with no Host
        // header. It must not trip the missing-Host rule.
        let stage = host_rule_stage();
        let (decision, _) = stage.inspect_request(
            "GET",
            "https://api.example.test/path",
            "HTTP/2.0",
            &[], // h2: no Host header
            b"",
            None,
            Some("203.0.113.7"),
        );
        assert!(
            matches!(decision, WafDecision::Allow),
            "Host synthesized from the URI authority should satisfy 920280"
        );
    }

    #[test]
    fn a_genuinely_missing_host_still_matches_920280() {
        // No Host header and no authority in the URI (origin-form) is a real
        // missing-Host request and must still be caught.
        let stage = host_rule_stage();
        let (decision, _) = stage.inspect_request(
            "GET",
            "/path",
            "HTTP/1.1",
            &[],
            b"",
            None,
            Some("203.0.113.7"),
        );
        assert!(
            matches!(
                decision,
                WafDecision::Block {
                    rule_id: 920280,
                    ..
                }
            ),
            "a request with neither Host header nor authority must match 920280"
        );
    }

    #[test]
    fn detection_only_records_without_blocking() {
        let stage = stage(EngineMode::DetectionOnly);
        assert!(!stage.is_blocking());
        assert!(matches!(
            inspect(&stage, "/?q=attack").0,
            WafDecision::Allow
        ));
    }

    #[test]
    fn a_response_body_rule_blocks() {
        let stage = stage(EngineMode::Blocking);
        let (decision, mut inspection) = inspect(&stage, "/?q=fine");
        assert!(matches!(decision, WafDecision::Allow));

        let decision = inspection.inspect_response(200, &[], Some(b"this leaks a secret"));
        match decision {
            WafDecision::Block {
                rule_id, message, ..
            } => {
                assert_eq!(rule_id, 2);
                assert_eq!(message, "outbound leak");
            }
            WafDecision::Allow => panic!("the response body rule should have blocked"),
        }
    }

    #[test]
    fn a_response_header_rule_blocks() {
        let stage = stage(EngineMode::Blocking);
        let (_, mut inspection) = inspect(&stage, "/?q=fine");
        let decision = inspection.inspect_response(
            200,
            &[("X-Debug".to_string(), "stacktrace".to_string())],
            Some(b"clean"),
        );
        assert!(matches!(decision, WafDecision::Block { rule_id: 3, .. }));
    }

    #[test]
    fn a_clean_response_is_allowed() {
        let stage = stage(EngineMode::Blocking);
        let (_, mut inspection) = inspect(&stage, "/?q=fine");
        assert!(matches!(
            inspection.inspect_response(200, &[], Some(b"nothing to see")),
            WafDecision::Allow
        ));
    }

    #[test]
    fn a_streamed_response_can_be_inspected_without_a_body() {
        // Callers that stream pass None: the body has already gone to the
        // client, so only headers can be judged.
        let stage = stage(EngineMode::Blocking);
        let (_, mut inspection) = inspect(&stage, "/?q=fine");
        assert!(matches!(
            inspection.inspect_response(200, &[], None),
            WafDecision::Allow
        ));
    }

    #[test]
    fn phase_five_runs_on_a_non_blocked_response() {
        let stage = stage(EngineMode::Blocking);
        let (_, mut inspection) = inspect(&stage, "/?q=fine");
        assert!(matches!(
            inspection.inspect_response(200, &[], Some(b"nothing to see")),
            WafDecision::Allow
        ));
        assert!(
            inspection.matched_rule_ids().contains(&5),
            "phase 5 must run when the response was not blocked"
        );
    }

    /// The engine stops phase processing after a disruptive verdict, so phases
    /// after the blocking one do not run.
    #[test]
    fn a_phase_three_block_stops_before_later_phases() {
        let stage = stage(EngineMode::Blocking);
        let (_, mut inspection) = inspect(&stage, "/?q=fine");
        let decision = inspection.inspect_response(
            200,
            &[("X-Debug".to_string(), "stacktrace".to_string())],
            Some(b"this leaks a secret"),
        );
        assert!(matches!(decision, WafDecision::Block { rule_id: 3, .. }));
        let matched = inspection.matched_rule_ids();
        assert!(
            !matched.contains(&2),
            "phase 4 must not run after a phase-3 block"
        );
        assert!(
            !matched.contains(&5),
            "phase 5 must not run after a phase-3 block"
        );
    }
}
