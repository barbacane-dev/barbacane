//! Where the YAML module reads a document differently from libyaml.
//!
//! libyaml, through `yaml_serde`, is the reference. A test reads every YAML
//! and JSON file in the repository, and each fixture under
//! `tests/fixtures/yaml-differential/`, with both, and fails on a difference
//! that belongs to no class in [`KNOWN`], or on a class that no longer
//! differs. The `yaml_differential` fuzz target uses the same list.

use serde_json::Value;

/// How two reads of a document differ.
#[derive(Debug, Clone, PartialEq)]
pub enum Difference {
    /// Only the module refused the document.
    OnlyOursFailed { error: String },
    /// Only the reference refused the document.
    OnlyReferenceFailed { error: String },
    /// Both read it, and the values first differ at `pointer`.
    Value {
        pointer: String,
        ours: Value,
        reference: Value,
    },
}

/// How `ours` and `reference` differ, or `None` when both refuse the document
/// or both read the same value.
pub fn compare(
    ours: &Result<Value, String>,
    reference: &Result<Value, String>,
) -> Option<Difference> {
    match (ours, reference) {
        (Err(_), Err(_)) => None,
        (Err(error), Ok(_)) => Some(Difference::OnlyOursFailed {
            error: error.clone(),
        }),
        (Ok(_), Err(error)) => Some(Difference::OnlyReferenceFailed {
            error: error.clone(),
        }),
        (Ok(ours), Ok(reference)) => {
            first_difference(ours, reference, String::new()).map(|(pointer, ours, reference)| {
                Difference::Value {
                    pointer,
                    ours,
                    reference,
                }
            })
        }
    }
}

/// The first JSON pointer at which `a` and `b` differ, with both values there.
fn first_difference(a: &Value, b: &Value, pointer: String) -> Option<(String, Value, Value)> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for key in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                let at = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (x.get(key), y.get(key)) {
                    (Some(x), Some(y)) => {
                        if let Some(found) = first_difference(x, y, at) {
                            return Some(found);
                        }
                    }
                    (x, y) => {
                        return Some((
                            at,
                            x.cloned().unwrap_or(Value::Null),
                            y.cloned().unwrap_or(Value::Null),
                        ))
                    }
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (x, y))| first_difference(x, y, format!("{pointer}/{i}"))),
        _ if a == b => None,
        _ => Some((pointer, a.clone(), b.clone())),
    }
}

/// A class of construct the module reads differently from libyaml, on purpose.
pub struct KnownDivergence {
    /// Short name of the class.
    pub name: &'static str,
    /// Why the module reads it this way.
    pub reason: &'static str,
    /// A document of the class. It must keep differing, so an entry cannot
    /// outlive the difference it excuses.
    pub example: &'static str,
    /// Whether a difference found on `input` belongs to the class.
    pub matches: fn(input: &str, difference: &Difference) -> bool,
}

/// Every class of difference accepted.
pub static KNOWN: &[KnownDivergence] = &[
    KnownDivergence {
        name: "integers past 64 bits",
        reason: "libyaml refuses the whole document when an integer does not fit in 64 bits; \
                 the module reads it as the nearest float",
        example: "a: 18446744073709551616\n",
        matches: |_, d| reference_refused(d, "number out of range"),
    },
    KnownDivergence {
        name: "local tags",
        reason: "libyaml refuses a node carrying a local tag such as `!custom`; the module \
                 reads the node and ignores the tag",
        example: "a: !custom value\n",
        matches: |_, d| reference_refused(d, "invalid type: enum"),
    },
    KnownDivergence {
        name: "`!!null` on a quoted scalar",
        reason: "YAML 1.2 makes a node tagged `!!null` null whatever its text; libyaml refuses \
                 `!!null \"\"`",
        example: "a: !!null \"\"\n",
        matches: |_, d| reference_refused(d, "expected null"),
    },
    KnownDivergence {
        name: "surrogate pairs in escapes",
        reason: "a JSON string may escape a character outside the Basic Multilingual Plane as \
                 a surrogate pair (`\\ud83d\\ude00`); libyaml refuses it, the module reads \
                 the character",
        example: "{\"a\": \"\\ud83d\\ude00\"}\n",
        matches: |_, d| reference_refused(d, "invalid Unicode character escape"),
    },
    KnownDivergence {
        name: "upper-case radix prefixes",
        reason: "serde-saphyr reads `0X1F`, `0O17` and `0B101` as integers; YAML 1.2 and \
                 libyaml keep them as strings, since the core schema's prefixes are lower case",
        example: "a: 0X1F\n",
        matches: |_, d| match d {
            Difference::Value {
                ours: Value::Number(_),
                reference: Value::String(text),
                ..
            } => {
                let digits = text.trim_start_matches(['+', '-']);
                digits.starts_with("0X") || digits.starts_with("0O") || digits.starts_with("0B")
            }
            _ => false,
        },
    },
    KnownDivergence {
        name: "null-like mapping keys",
        reason: "serde-saphyr refuses a plain `null`, `~` or empty mapping key where a string \
                 is expected; libyaml reads the key as its text",
        example: "null: 1\n",
        matches: |_, d| {
            matches!(d, Difference::OnlyOursFailed { error }
                if error.contains("cannot deserialize null into string"))
        },
    },
];

/// Whether `difference` is the reference refusing a document the module
/// reads, with an error containing `message`.
fn reference_refused(difference: &Difference, message: &str) -> bool {
    matches!(difference, Difference::OnlyReferenceFailed { error } if error.contains(message))
}

/// The class `difference` belongs to, if any.
pub fn classify(input: &str, difference: &Difference) -> Option<&'static KnownDivergence> {
    KNOWN
        .iter()
        .find(|class| (class.matches)(input, difference))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn ours(input: &str) -> Result<Value, String> {
        crate::yaml::from_str(input)
    }

    fn reference(input: &str) -> Result<Value, String> {
        yaml_serde::from_str(input).map_err(|e| e.to_string())
    }

    /// Why `input` fails the gate, if it does.
    fn check(input: &str) -> Option<String> {
        let ours = ours(input);
        if let Some(difference) = compare(&ours, &reference(input)) {
            if classify(input, &difference).is_none() {
                return Some(format!("{difference:?}"));
            }
        }
        // What the module writes, libyaml reads back as the same value.
        if let Ok(value) = ours {
            let written = match crate::yaml::to_string(&value) {
                Ok(written) => written,
                Err(e) => return Some(format!("cannot write it back: {e}")),
            };
            match reference(&written) {
                Ok(read_back) if read_back == value => {}
                Ok(read_back) => {
                    return Some(format!(
                        "written back, libyaml reads it differently: {:?}",
                        first_difference(&value, &read_back, String::new())
                    ))
                }
                Err(e) => return Some(format!("written back, libyaml refuses it: {e}")),
            }
        }
        None
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Every YAML and JSON file in the repository, build output and
    /// dependencies left out.
    fn repository_files(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !matches!(name.as_str(), "target" | "node_modules" | ".git") {
                    repository_files(&path, found);
                }
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml" | "json")
            ) {
                found.push(path);
            }
        }
    }

    fn gate(files: &[PathBuf]) {
        let failures: Vec<String> = files
            .iter()
            .filter_map(|path| {
                let input = std::fs::read_to_string(path).ok()?;
                check(&input).map(|why| format!("{}: {why}", path.display()))
            })
            .collect();
        assert!(
            failures.is_empty(),
            "{} files read differently from libyaml:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn every_repository_file_reads_as_libyaml_reads_it() {
        let mut files = Vec::new();
        repository_files(&root(), &mut files);
        assert!(files.len() > 100, "found only {} files", files.len());
        gate(&files);
    }

    #[test]
    fn every_fixture_reads_as_libyaml_reads_it_or_as_a_known_class() {
        let mut files = Vec::new();
        repository_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yaml-differential"),
            &mut files,
        );
        assert!(!files.is_empty());
        gate(&files);
    }

    #[test]
    fn every_known_class_still_differs() {
        for class in KNOWN {
            let difference = compare(&ours(class.example), &reference(class.example));
            let Some(difference) = difference else {
                panic!("{}: its example no longer differs", class.name);
            };
            assert!(
                std::ptr::eq(classify(class.example, &difference).unwrap(), class),
                "{}: its example is classified elsewhere",
                class.name
            );
        }
    }
}
