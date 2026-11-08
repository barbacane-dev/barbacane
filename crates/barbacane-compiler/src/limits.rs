//! Resource limits a plugin declares in the `[limits]` table of its
//! `plugin.toml`.
//!
//! The artifact records them with the plugin's capabilities, under
//! `artifact_hash`, and the data plane gives each instance of the plugin those
//! limits. A field the plugin leaves out takes the data plane's default.

use serde::{Deserialize, Serialize};

/// The most linear memory a plugin may declare, in MiB.
pub const MAX_MEMORY_MIB: u64 = 1024;
/// The most fuel a plugin may declare for one call.
pub const MAX_FUEL: u64 = 10_000_000_000;
/// The longest wall-clock budget a plugin may declare for one call, in ms.
pub const MAX_TIMEOUT_MS: u64 = 10_000;

/// The keys `[limits]` accepts, with their ceilings.
const KEYS: [(&str, u64); 3] = [
    ("memory_mib", MAX_MEMORY_MIB),
    ("fuel", MAX_FUEL),
    ("timeout_ms", MAX_TIMEOUT_MS),
];

/// Limits a plugin declares. `None` takes the data plane's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredLimits {
    /// Linear memory, in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
    /// Fuel for one call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel: Option<u64>,
    /// Wall-clock budget for one call, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl DeclaredLimits {
    /// Whether the plugin declares no limit.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Read a `[limits]` table. Every key must be one of `memory_mib`, `fuel`
    /// and `timeout_ms`, with an integer from 1 to its ceiling.
    pub fn from_toml(table: &toml::Table) -> Result<Self, String> {
        let mut limits = Self::default();
        for (key, value) in table {
            let Some((_, ceiling)) = KEYS.iter().find(|(name, _)| name == key) else {
                return Err(format!(
                    "unknown key `{key}`; [limits] takes memory_mib, fuel and timeout_ms"
                ));
            };
            let value = value
                .as_integer()
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| format!("`{key}` must be a positive integer, not {value}"))?;
            let slot = match key.as_str() {
                "memory_mib" => &mut limits.memory_mib,
                "fuel" => &mut limits.fuel,
                _ => &mut limits.timeout_ms,
            };
            *slot = Some(value);
            check(key, value, *ceiling)?;
        }
        Ok(limits)
    }

    /// Check each declared value against its ceiling. The data plane runs the
    /// same check on the limits an artifact records.
    pub fn check(&self) -> Result<(), String> {
        for ((key, ceiling), value) in
            KEYS.iter()
                .zip([self.memory_mib, self.fuel, self.timeout_ms])
        {
            if let Some(value) = value {
                check(key, value, *ceiling)?;
            }
        }
        Ok(())
    }

    /// The form `artifact_hash` covers: each limit, `-` when unset.
    pub(crate) fn hash_input(&self) -> String {
        let show = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
        format!(
            "memory_mib:{},fuel:{},timeout_ms:{}",
            show(self.memory_mib),
            show(self.fuel),
            show(self.timeout_ms)
        )
    }
}

fn check(key: &str, value: u64, ceiling: u64) -> Result<(), String> {
    if value == 0 {
        return Err(format!("`{key}` must be at least 1"));
    }
    if value > ceiling {
        return Err(format!(
            "`{key}` = {value} is above the gateway's ceiling of {ceiling}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<DeclaredLimits, String> {
        DeclaredLimits::from_toml(&toml.parse::<toml::Table>().expect("valid TOML"))
    }

    #[test]
    fn every_key_is_read() {
        assert_eq!(
            parse("memory_mib = 4\nfuel = 5000\ntimeout_ms = 20"),
            Ok(DeclaredLimits {
                memory_mib: Some(4),
                fuel: Some(5000),
                timeout_ms: Some(20),
            })
        );
        assert_eq!(
            parse("timeout_ms = 20"),
            Ok(DeclaredLimits {
                timeout_ms: Some(20),
                ..Default::default()
            })
        );
        assert!(parse("").expect("empty table").is_empty());
    }

    #[test]
    fn the_ceilings_are_inclusive() {
        let at_ceiling = format!(
            "memory_mib = {MAX_MEMORY_MIB}\nfuel = {MAX_FUEL}\ntimeout_ms = {MAX_TIMEOUT_MS}"
        );
        assert!(parse(&at_ceiling).is_ok());
        for (key, ceiling) in KEYS {
            let error = parse(&format!("{key} = {}", ceiling + 1)).unwrap_err();
            assert!(error.contains(key) && error.contains("ceiling"), "{error}");
        }
    }

    #[test]
    fn an_unknown_key_zero_a_negative_or_a_non_integer_is_refused() {
        for (toml, expected) in [
            ("memory = 4", "unknown key `memory`"),
            ("memory_mib = 0", "at least 1"),
            ("fuel = -1", "positive integer"),
            ("timeout_ms = 1.5", "positive integer"),
            ("timeout_ms = \"20\"", "positive integer"),
        ] {
            let error = parse(toml).unwrap_err();
            assert!(error.contains(expected), "{toml}: {error}");
        }
    }

    #[test]
    fn check_applies_the_same_bounds_to_recorded_limits() {
        assert!(DeclaredLimits::default().check().is_ok());
        let over = DeclaredLimits {
            memory_mib: Some(MAX_MEMORY_MIB + 1),
            ..Default::default()
        };
        assert!(over.check().unwrap_err().contains("memory_mib"));
        let zero = DeclaredLimits {
            fuel: Some(0),
            ..Default::default()
        };
        assert!(zero.check().unwrap_err().contains("fuel"));
    }

    #[test]
    fn the_hash_input_names_every_limit() {
        assert_eq!(
            DeclaredLimits::default().hash_input(),
            "memory_mib:-,fuel:-,timeout_ms:-"
        );
        let some = DeclaredLimits {
            memory_mib: Some(4),
            fuel: None,
            timeout_ms: Some(20),
        };
        assert_eq!(some.hash_input(), "memory_mib:4,fuel:-,timeout_ms:20");
    }
}
