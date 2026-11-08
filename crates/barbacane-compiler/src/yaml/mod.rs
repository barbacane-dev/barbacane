//! Reading and writing YAML, for every crate in the workspace.
//!
//! Every crate reads and writes YAML through this module; only the data
//! plane's build script, which reads the control plane's spec, names the YAML
//! crate itself. Documents are parsed by
//! serde-saphyr, in safe Rust, with options that read them as libyaml does;
//! [`differential`] lists where the two still differ, and a test holds every
//! YAML and JSON file in the repository to that list.

pub mod differential;
mod measure;
mod value;

use std::cell::Cell;

use serde::de::{DeserializeOwned, DeserializeSeed};
use serde::Serialize;
use serde_saphyr::{Budget, DuplicateKeyPolicy, MergeKeyPolicy, NonFiniteFloatPolicy, Options};

pub use value::{Mapping, Value};

/// Deepest nesting a document may have, as in libyaml.
const MAX_DEPTH: usize = 128;

/// Most nodes a document may have. The published GitHub spec has about
/// 490,000; aliases are bounded separately by the expansion budget.
const MAX_NODES: usize = 5_000_000;

/// Most parser events a document may produce. The published GitHub spec
/// produces about 580,000.
const MAX_EVENTS: usize = 10_000_000;

/// The options every read uses.
fn options() -> Options {
    let mut options = Options::default();
    // Only `true` and `false` are booleans, so `on:` and `yes` stay strings.
    options.strict_booleans = true;
    // libyaml keeps the last value of a duplicated key.
    options.duplicate_keys = DuplicateKeyPolicy::LastWins;
    // libyaml keeps `<<` as an ordinary key.
    options.merge_keys = MergeKeyPolicy::AsOrdinary;
    // `!!binary` text stays as written in a string.
    options.ignore_binary_tag_for_string = true;
    // `.inf` and `.nan` are read, and become `null` in a JSON value.
    options.non_finite_float_policy = NonFiniteFloatPolicy::PassThrough;
    // libyaml keeps `0755`, `007`, `1_000` and `0xFF_FF` as strings.
    options.leading_zero_numbers_as_strings = true;
    options.underscored_numbers_as_strings = true;
    let mut budget = Budget::default();
    budget.max_depth = MAX_DEPTH;
    budget.max_nodes = MAX_NODES;
    budget.max_events = MAX_EVENTS;
    options.budget = Some(budget);
    options
}

/// Stack for reading and writing. serde-saphyr recurses once per level of
/// nesting, and a build without optimisations needs about 3 MiB for the
/// deepest document [`MAX_DEPTH`] admits, more than a tokio worker has.
const STACK: usize = 16 * 1024 * 1024;

/// Run `work` on a thread with a [`STACK`]-sized stack.
fn on_large_stack<R: Send>(work: impl FnOnce() -> Result<R, String> + Send) -> Result<R, String> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, work)
            .map_err(|e| format!("cannot start the YAML thread: {e}"))?
            .join()
            .map_err(|_| "the YAML thread panicked".to_string())?
    })
}

/// Read a YAML or JSON document whose value stays within the expansion
/// budget: one whose aliases do not make it stand for a value more than 16
/// times its size, plus 1 MiB.
pub fn from_str<T: DeserializeOwned + Send>(input: &str) -> Result<T, String> {
    on_large_stack(|| {
        let remaining = Cell::new(measure::budget(input.len()));
        serde_saphyr::with_deserializer_from_str_with_options(input, options(), |deserializer| {
            measure::Measure {
                remaining: &remaining,
            }
            .deserialize(deserializer)
        })
        .map_err(|e| e.to_string())?;
        serde_saphyr::from_str_with_options(input, options()).map_err(|e| e.to_string())
    })
}

/// Write a value as a YAML document.
pub fn to_string<T: Serialize + Sync>(value: &T) -> Result<String, String> {
    on_large_stack(|| serde_saphyr::to_string(value).map_err(|e| e.to_string()))
}

/// A serializable value as an ordered [`Value`], fields in declaration order.
pub fn to_value<T: Serialize + Sync>(value: &T) -> Result<Value, String> {
    from_str(&to_string(value)?)
}
