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
        }))
    }

    /// How many rules the stage carries.
    pub fn rule_count(&self) -> usize {
        self.rules.rule_count()
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
    pub fn inspect_request(
        &self,
        method: &str,
        uri: &str,
        protocol: &str,
        headers: &[(String, String)],
        body: &[u8],
        content_type: Option<&str>,
        remote_addr: Option<&str>,
    ) -> WafDecision {
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
        let verdict = tx.process_request_headers();
        if let decision @ WafDecision::Block { .. } = Self::decide(&tx, verdict) {
            return decision;
        }

        if !body.is_empty() {
            tx.set_request_body(body, content_type);
        }
        let verdict = tx.process_request_body();
        Self::decide(&tx, verdict)
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
