//! Differential fuzzing of the YAML module against libyaml.
//!
//! Each input is read by `barbacane_compiler::yaml` and by `yaml_serde`
//! (libyaml). The target fails on a difference that can change what a
//! deployment reads, and that belongs to no class in
//! `barbacane_compiler::yaml::differential::KNOWN`: a document both read to
//! different values, or one libyaml reads and the module refuses. A document
//! only libyaml refuses was never a working spec, so it is not a finding.
//! libFuzzer's `-timeout` and `-rss_limit_mb` bound the time and memory one
//! input may take on both sides together, and an input past either is kept
//! as a finding too.
//!
//! Run: `cargo +nightly fuzz run yaml_differential -- -dict=yaml.dict`

#![no_main]

use barbacane_compiler::yaml::differential::{classify, compare, Difference};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let ours = barbacane_compiler::yaml::from_str::<Value>(input);
    let reference = yaml_serde::from_str::<Value>(input).map_err(|e| e.to_string());
    if let Some(difference) = compare(&ours, &reference) {
        if matches!(difference, Difference::OnlyReferenceFailed { .. }) {
            return;
        }
        assert!(
            classify(input, &difference).is_some(),
            "a difference of no known class: {difference:?}"
        );
    }
});
