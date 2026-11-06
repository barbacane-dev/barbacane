//! The group list auth plugins publish in the `x-auth-consumer-groups` header
//! and the `auth.groups` context key.
//!
//! Group names are joined with `,`, each with `%` written as `%25` and `,` as
//! `%2C`, so a name that contains a comma (an LDAP DN, a claim value) stays one
//! group. A name without either character is written unchanged.
//!
//! ```
//! use barbacane_plugin_sdk::groups;
//! let value = groups::encode(["admins", "cn=ops,dc=example"]);
//! assert_eq!(value, "admins,cn=ops%2Cdc=example");
//! assert_eq!(groups::decode(&value), ["admins", "cn=ops,dc=example"]);
//! ```

/// Join group names into one value. Empty names are skipped.
pub fn encode<I, S>(groups: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = String::new();
    for group in groups {
        let group = group.as_ref();
        if group.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        for c in group.chars() {
            match c {
                '%' => out.push_str("%25"),
                ',' => out.push_str("%2C"),
                c => out.push(c),
            }
        }
    }
    out
}

/// Split a value written by [`encode`] back into group names. Whitespace
/// around each entry is not significant, and empty entries are dropped.
pub fn decode(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(unescape)
        .collect()
}

/// Reverse the two escapes [`encode`] writes. Any other `%` is kept as is.
fn unescape(group: &str) -> String {
    let mut out = String::with_capacity(group.len());
    let mut rest = group;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        match tail.as_bytes().get(1..3) {
            Some([b'2', b'C' | b'c']) => {
                out.push(',');
                rest = &tail[3..];
            }
            Some([b'2', b'5']) => {
                out.push('%');
                rest = &tail[3..];
            }
            _ => {
                out.push('%');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_without_a_comma_or_percent_are_unchanged() {
        assert_eq!(encode(["admin", "editor"]), "admin,editor");
        assert_eq!(decode("admin,editor"), ["admin", "editor"]);
    }

    #[test]
    fn a_name_with_commas_stays_one_group() {
        let dn = "cn=ops,ou=groups,dc=example,dc=org";
        let value = encode([dn, "viewer"]);
        assert_eq!(value, "cn=ops%2Cou=groups%2Cdc=example%2Cdc=org,viewer");
        assert_eq!(decode(&value), [dn, "viewer"]);
    }

    #[test]
    fn percent_signs_round_trip() {
        for name in ["100%", "%2C", "a%25b", "%", "x,%2C,y"] {
            assert_eq!(decode(&encode([name])), [name], "{name}");
        }
    }

    #[test]
    fn other_escapes_and_multibyte_text_are_kept() {
        assert_eq!(decode("%41,é%2Cü,%é"), ["%41", "é,ü", "%é"]);
    }

    #[test]
    fn whitespace_and_empty_entries_are_dropped() {
        assert_eq!(decode(" a , ,b,"), ["a", "b"]);
        assert_eq!(encode(["", "a", ""]), "a");
        assert!(decode("").is_empty());
    }
}
