//! Differential fuzzing of the YAML module against libyaml.
//!
//! Each input is read by `barbacane_compiler::yaml` and by `yaml_serde`
//! (libyaml). A difference that belongs to no class in
//! `barbacane_compiler::yaml::differential::KNOWN` is a finding: the target
//! panics and libFuzzer keeps the input. libFuzzer's `-timeout` and
//! `-rss_limit_mb` bound the time and memory one input may take on both sides
//! together, and an input past either is kept as a finding too.
//!
//! Run: `cargo +nightly fuzz run yaml_differential -- -dict=yaml.dict`

#![no_main]

use barbacane_compiler::yaml::differential::{classify, compare};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let ours = barbacane_compiler::yaml::from_str::<Value>(input);
    let reference = yaml_serde::from_str::<Value>(input).map_err(|e| e.to_string());
    if let Some(difference) = compare(&ours, &reference) {
        assert!(
            classify(input, &difference).is_some(),
            "a difference of no known class: {difference:?}"
        );
    }
});
