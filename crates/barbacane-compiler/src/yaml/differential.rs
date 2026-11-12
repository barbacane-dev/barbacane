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
        name: "multi-line keys in flow mappings",
        reason: "YAML 1.2 lets a key in a flow mapping span lines, and its `:` start the next \
                 line (yaml-test-suite NJ66, 4MUZ); libyaml keeps YAML 1.1's rule that such a \
                 key fits on one line and refuses the document",
        example: "{ multi\n  line: value}\n",
        matches: |_, d| {
            reference_refused(d, "did not find expected ',' or '}'")
                && reference_refused(d, "while parsing a flow mapping")
        },
    },
    KnownDivergence {
        name: "under-indented quoted scalars",
        reason: "YAML 1.2 requires the continuation lines of a quoted scalar in block context \
                 to be indented past the node that holds it (yaml-test-suite QB6E), and a tab \
                 does not count as indentation; libyaml reads them, the module refuses the \
                 document",
        example: "quoted: \"a\nb\nc\"\n",
        matches: |_, d| {
            matches!(d, Difference::OnlyOursFailed { error }
                if error.contains("invalid indentation in multiline quoted scalar")
                    || error.contains("tab cannot be used as indentation"))
        },
    },
    KnownDivergence {
        name: "byte order marks inside a document",
        reason: "YAML 1.2 allows a byte order mark before a document and inside quoted \
                 scalars only; libyaml reads one that begins a node as content, where the \
                 module skips one before the document and refuses one inside it",
        example: "a: \u{feff}x\n",
        matches: |_, d| match d {
            Difference::OnlyOursFailed { error } => {
                error.contains("a BOM must not appear inside a document")
            }
            Difference::Value {
                reference: Value::String(text),
                ..
            } => text.contains('\u{feff}'),
            _ => false,
        },
    },
    KnownDivergence {
        name: "byte order marks starting the stream",
        reason: "YAML 1.2 lets a byte order mark start the stream; libyaml counts it as a \
                 column of the first line, so it takes a `---` after it for content and the \
                 first key for one indented past the keys after it, refusing a block mapping \
                 with more than one key. The shift can change any construct on that line, so \
                 every difference on such a document belongs here",
        example: "\u{feff}a: 1\nb: 2\n",
        matches: |input, _| input.starts_with('\u{feff}'),
    },
    KnownDivergence {
        name: "block scalars ending the input without a line break",
        reason: "the yaml-test-suite reads the end of the input as a line break (JEF9/02, \
                 L24T/01; yaml/yaml-test-suite#118), so a block scalar whose last line ends \
                 the input keeps one, `text\\n`, in a value or a key; libyaml returns `text`",
        example: "a: |\n  text",
        matches: |input, d| {
            !input.ends_with(['\n', '\r'])
                && match d {
                    Difference::Value {
                        ours: Value::String(ours),
                        reference: Value::String(reference),
                        ..
                    } => ours.strip_suffix('\n') == Some(reference.as_str()),
                    Difference::Value { pointer, .. } => {
                        pointer.ends_with('\n') && input.contains(['|', '>'])
                    }
                    _ => false,
                }
        },
    },
    KnownDivergence {
        name: "unknown tags on plain scalars",
        reason: "libyaml keeps a plain scalar under a tag it does not resolve (`!!foo 1`, an \
                 empty `!!foo`) as a string; the module ignores the tag and resolves the \
                 scalar as a number, a boolean or null",
        example: "a: !!foo 1\nb: !!foo\n",
        matches: |input, d| {
            input.contains('!')
                && matches!(d, Difference::Value {
                    ours: ours @ (Value::Null | Value::Number(_) | Value::Bool(_)),
                    reference: Value::String(text),
                    ..
                } if crate::yaml::from_str::<Value>(text).ok().as_ref() == Some(ours))
        },
    },
    KnownDivergence {
        name: "core tags on content of another type",
        reason: "YAML 1.2 makes a node tagged `!!int`, `!!float`, `!!bool`, `!!null` or \
                 `!!binary` whose content is not of that type invalid; libyaml reads it as a \
                 string, the module refuses the document",
        example: "!!int :\n",
        matches: |_, d| {
            matches!(d, Difference::OnlyOursFailed { error } if [
                "invalid u64",
                "invalid i64",
                "invalid boolean",
                "invalid floating point",
                "unsupported tag `!!",
                "cannot deserialize tagged scalar into string",
            ]
            .iter()
            .any(|message| error.contains(message)))
        },
    },
    KnownDivergence {
        name: "malformed tags",
        reason: "a tag such as `!!str!` or `!!!str` is not a YAML 1.2 tag; libyaml reads one, \
                 the module refuses the document",
        example: "a: !!str! x\n",
        matches: |_, d| {
            matches!(d, Difference::OnlyOursFailed { error }
                if error.contains("while scanning a tag") || error.contains("while parsing a tag"))
        },
    },
    KnownDivergence {
        name: "comments not preceded by whitespace",
        reason: "YAML 1.2 requires whitespace before a comment, as in `| #`; libyaml reads \
                 `|#`, the module refuses the document",
        example: "a: |#\n  x\n",
        matches: |_, d| {
            matches!(d, Difference::OnlyOursFailed { error }
                if error.contains("comments must be separated from other tokens by whitespace"))
        },
    },
    KnownDivergence {
        name: "next-line and Unicode line separators",
        reason: "YAML 1.2 no longer breaks lines at U+0085, U+2028 or U+2029; libyaml, \
                 following YAML 1.1, does, the module keeps them as characters",
        example: "a: \"x\u{85}y\"\n",
        matches: |input, d| {
            input.contains(['\u{85}', '\u{2028}', '\u{2029}'])
                && matches!(
                    d,
                    Difference::Value { .. } | Difference::OnlyOursFailed { .. }
                )
        },
    },
    KnownDivergence {
        name: "bare carriage returns",
        reason: "the readers disagree on block scalars and keys in a document that breaks \
                 lines with a carriage return alone; a spec uses LF or CRLF",
        example: "?\r?\r>\r",
        matches: |input, _| has_bare_carriage_return(input),
    },
    KnownDivergence {
        name: "signed .nan",
        reason: "YAML 1.2 spells not-a-number `.nan` with no sign, so `-.nan` is a string, as \
                 libyaml reads it; serde-saphyr reads not-a-number, null in a JSON value \
                 (bourumir-wyngs/serde-saphyr#271)",
        example: "a: -.nan\n",
        matches: |_, d| {
            matches!(d, Difference::Value {
                ours: Value::Null,
                reference: Value::String(text),
                ..
            } if matches!(
                text.strip_prefix(['+', '-']),
                Some(".nan" | ".NaN" | ".NAN")
            ))
        },
    },
    KnownDivergence {
        name: "anchor names with punctuation",
        reason: "YAML 1.2 lets an anchor name hold any character but whitespace and flow \
                 indicators (`&a:b`); libyaml ends it at the first character other than a \
                 letter, digit, `-` or `_`, and reads the rest as content",
        example: "a: &x:y\n",
        matches: |input, d| {
            has_punctuated_anchor(input)
                && matches!(
                    d,
                    Difference::Value { .. } | Difference::OnlyOursFailed { .. }
                )
        },
    },
    KnownDivergence {
        name: "keys equal as integers",
        reason: "YAML 1.2 makes two keys equal when their tags and canonical forms are, so \
                 `10` and `0xA`, both the integer 10, are one key, and the last is kept; libyaml \
                 compares keys by their text and keeps both",
        example: "10: a\n0xA: b\n",
        matches: |input, d| {
            has_radix_key(input)
                && matches!(d, Difference::Value { pointer, ours: Value::Null, .. }
                    if pointer.rsplit('/').next().is_some_and(is_integer_literal))
        },
    },
    KnownDivergence {
        name: "block content not indented past its key",
        reason: "YAML 1.2 indents a node that starts on the line after its key (`a:` or `?`) \
                 past the key; libyaml reads an unindented block scalar there, the module \
                 refuses the document",
        example: "a:\n>\n x\n",
        matches: |_, d| {
            matches!(d, Difference::OnlyOursFailed { error }
                if error.contains("simple key expected ':'"))
        },
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
    KnownDivergence {
        name: "`?` followed by content in flow collections",
        reason: "YAML 1.2 takes `?` for an explicit key only when whitespace follows it, so \
                 `{?a: 1}` has the key `?a`; in a flow collection libyaml takes it for one \
                 whatever follows, and reads the key `a`",
        example: "{?a: 1}\n",
        matches: |input, d| {
            input.contains(['{', '['])
                && matches!(d, Difference::Value { pointer, ours, .. }
                    if pointer.rsplit('/').next().is_some_and(starts_with_question_mark)
                        || ours.as_str().is_some_and(starts_with_question_mark))
        },
    },
    KnownDivergence {
        name: "floats past the double range",
        reason: "YAML 1.2 resolves a plain scalar such as `1e400` as a float, which overflows \
                 to infinity, null in a JSON value; libyaml keeps it as a string",
        example: "a: 1e400\n",
        matches: |_, d| {
            matches!(d, Difference::Value {
                ours: Value::Null,
                reference: Value::String(text),
                ..
            } if is_float_literal(text) && text.parse::<f64>().is_ok_and(f64::is_infinite))
        },
    },
    KnownDivergence {
        name: "unindented lines in a top-level block scalar",
        reason: "YAML 1.2 lets a block scalar at the top of a document hold lines at column 0 \
                 (spec example 9.3), so `|` then `#x` is the text `#x`; libyaml requires them \
                 indented and reads `#x` as a comment",
        example: "|\n#x\n",
        matches: |input, d| {
            input.lines().any(|line| line.starts_with('#'))
                && matches!(d, Difference::Value {
                    pointer,
                    ours: Value::String(_),
                    reference: Value::String(_),
                } if pointer.is_empty())
        },
    },
    KnownDivergence {
        name: "reused anchor names",
        reason: "YAML 1.2 resolves an alias to the last node before it with that anchor, as \
                 the module does; yaml_serde numbers anchors by how many names it has seen, so \
                 once a name is reused an alias can resolve to another anchor's node, or miss \
                 a recursive reference the module refuses (yaml/yaml-serde#18)",
        example: "a: &a 1\nb: &b 2\nc: &a 3\nd: &c 4\nx: *a\n",
        matches: |input, d| {
            has_reused_anchor(input)
                && match d {
                    Difference::Value { .. } => true,
                    Difference::OnlyOursFailed { error } => error.contains("recursive references"),
                    Difference::OnlyReferenceFailed { .. } => false,
                }
        },
    },
];

/// Whether `input` gives the same anchor name to more than one node.
fn has_reused_anchor(input: &str) -> bool {
    let mut names = std::collections::HashSet::new();
    input.split('&').skip(1).any(|rest| {
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '{' | '}'))
            .unwrap_or(rest.len());
        end > 0 && !names.insert(&rest[..end])
    })
}

/// Whether `text` starts with `?` followed by a character other than
/// whitespace.
fn starts_with_question_mark(text: &str) -> bool {
    text.strip_prefix('?')
        .is_some_and(|rest| rest.starts_with(|c: char| !c.is_whitespace()))
}

/// Whether `text` is written as a decimal float: digits, `.`, `e`, `E`, `+`
/// and `-` only, with at least one digit.
fn is_float_literal(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
}

/// Whether `input` breaks a line with a carriage return not followed by a
/// line feed.
fn has_bare_carriage_return(input: &str) -> bool {
    let bytes = input.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
}

/// Whether `input` has an anchor whose name holds a character libyaml does
/// not take in one: anything but a letter, digit, `-` or `_`.
fn has_punctuated_anchor(input: &str) -> bool {
    input.split('&').skip(1).any(|rest| {
        rest.chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, ',' | '[' | ']' | '{' | '}'))
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    })
}

/// Whether `input` has a mapping key written in hexadecimal, octal or binary.
fn has_radix_key(input: &str) -> bool {
    input.lines().any(|line| {
        let key = line
            .trim_start_matches([' ', '-', '{', ','])
            .split(':')
            .next()
            .unwrap_or("");
        ["0x", "0o", "0b", "0X", "0O", "0B"].iter().any(|prefix| {
            key.strip_prefix(prefix).is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit())
            })
        })
    })
}

/// Whether `text` is an integer as YAML writes one: decimal, or `0x`, `0o`
/// or `0b` followed by digits.
fn is_integer_literal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let digits = ["0x", "0o", "0b", "0X", "0O", "0B"]
        .iter()
        .find_map(|p| digits.strip_prefix(p))
        .unwrap_or(digits);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit())
}

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
    fn classifying_never_panics() {
        let differences = [
            Difference::OnlyOursFailed { error: "x".into() },
            Difference::OnlyReferenceFailed { error: "x".into() },
            Difference::Value {
                pointer: "/0".into(),
                ours: Value::Null,
                reference: Value::String("x".into()),
            },
        ];
        for input in [
            "\u{feff}---",
            "\u{feff}0x1: a",
            "é: 1",
            "0é: 1",
            "&é: 1",
            "&é &é",
            "\r",
            "",
        ] {
            for difference in &differences {
                let _ = classify(input, difference);
            }
        }
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
