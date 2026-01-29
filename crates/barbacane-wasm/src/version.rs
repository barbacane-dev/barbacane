//! Plugin version resolution.
//!
//! Supports version specifiers:
//! - `name` - latest version
//! - `name@1.0.0` - exact version
//! - `name@^1.0.0` - semver compatible (>=1.0.0, <2.0.0)
//! - `name@~1.0.0` - patch compatible (>=1.0.0, <1.1.0)

use semver::{Version, VersionReq};

/// A parsed plugin reference with name and optional version constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRef {
    /// Plugin name (kebab-case).
    pub name: String,
    /// Version constraint (None = latest).
    pub version: Option<VersionConstraint>,
}

/// A version constraint for plugin resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionConstraint {
    /// Exact version match.
    Exact(Version),
    /// Semver range (e.g., ^1.0.0, ~1.0.0, >=1.0.0).
    Range(VersionReq),
}

impl PluginRef {
    /// Parse a plugin reference string.
    ///
    /// # Examples
    ///
    /// ```
    /// use barbacane_wasm::version::PluginRef;
    ///
    /// let ref1 = PluginRef::parse("rate-limit").unwrap();
    /// assert_eq!(ref1.name, "rate-limit");
    /// assert!(ref1.version.is_none());
    ///
    /// let ref2 = PluginRef::parse("rate-limit@1.0.0").unwrap();
    /// assert_eq!(ref2.name, "rate-limit");
    /// assert!(ref2.version.is_some());
    /// ```
    pub fn parse(s: &str) -> Result<Self, VersionError> {
        if let Some(at_pos) = s.find('@') {
            let name = s[..at_pos].to_string();
            let version_str = &s[at_pos + 1..];

            if name.is_empty() {
                return Err(VersionError::EmptyName);
            }

            let version = parse_version_constraint(version_str)?;

            Ok(PluginRef {
                name,
                version: Some(version),
            })
        } else {
            if s.is_empty() {
                return Err(VersionError::EmptyName);
            }

            Ok(PluginRef {
                name: s.to_string(),
                version: None,
            })
        }
    }

    /// Check if a specific version matches this constraint.
    pub fn matches(&self, version: &Version) -> bool {
        match &self.version {
            None => true, // No constraint = matches any version
            Some(VersionConstraint::Exact(v)) => v == version,
            Some(VersionConstraint::Range(req)) => req.matches(version),
        }
    }

    /// Select the best matching version from a list of available versions.
    ///
    /// Returns the highest version that matches the constraint.
    pub fn select_version<'a>(&self, available: &'a [Version]) -> Option<&'a Version> {
        let mut matching: Vec<_> = available.iter().filter(|v| self.matches(v)).collect();

        // Sort descending to get highest version first
        matching.sort_by(|a, b| b.cmp(a));

        matching.first().copied()
    }
}

/// Parse a version constraint string.
fn parse_version_constraint(s: &str) -> Result<VersionConstraint, VersionError> {
    // Check for range prefixes
    if s.starts_with('^') || s.starts_with('~') || s.starts_with('>') || s.starts_with('<') || s.starts_with('=')
    {
        let req = VersionReq::parse(s).map_err(|e| VersionError::InvalidRange(e.to_string()))?;
        Ok(VersionConstraint::Range(req))
    } else {
        // Try parsing as exact version
        let version =
            Version::parse(s).map_err(|e| VersionError::InvalidVersion(e.to_string()))?;
        Ok(VersionConstraint::Exact(version))
    }
}

/// Errors that can occur during version parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// Plugin name is empty.
    EmptyName,
    /// Invalid version string.
    InvalidVersion(String),
    /// Invalid version range.
    InvalidRange(String),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionError::EmptyName => write!(f, "plugin name cannot be empty"),
            VersionError::InvalidVersion(e) => write!(f, "invalid version: {}", e),
            VersionError::InvalidRange(e) => write!(f, "invalid version range: {}", e),
        }
    }
}

impl std::error::Error for VersionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_only() {
        let r = PluginRef::parse("rate-limit").unwrap();
        assert_eq!(r.name, "rate-limit");
        assert!(r.version.is_none());
    }

    #[test]
    fn parse_exact_version() {
        let r = PluginRef::parse("rate-limit@1.0.0").unwrap();
        assert_eq!(r.name, "rate-limit");
        assert!(matches!(r.version, Some(VersionConstraint::Exact(_))));
    }

    #[test]
    fn parse_caret_range() {
        let r = PluginRef::parse("rate-limit@^1.0.0").unwrap();
        assert_eq!(r.name, "rate-limit");
        assert!(matches!(r.version, Some(VersionConstraint::Range(_))));
    }

    #[test]
    fn parse_tilde_range() {
        let r = PluginRef::parse("rate-limit@~1.0.0").unwrap();
        assert_eq!(r.name, "rate-limit");
        assert!(matches!(r.version, Some(VersionConstraint::Range(_))));
    }

    #[test]
    fn parse_gte_range() {
        let r = PluginRef::parse("rate-limit@>=1.0.0").unwrap();
        assert_eq!(r.name, "rate-limit");
        assert!(matches!(r.version, Some(VersionConstraint::Range(_))));
    }

    #[test]
    fn parse_empty_name_fails() {
        let r = PluginRef::parse("");
        assert!(matches!(r, Err(VersionError::EmptyName)));
    }

    #[test]
    fn parse_empty_name_with_at_fails() {
        let r = PluginRef::parse("@1.0.0");
        assert!(matches!(r, Err(VersionError::EmptyName)));
    }

    #[test]
    fn parse_invalid_version_fails() {
        let r = PluginRef::parse("rate-limit@not-a-version");
        assert!(matches!(r, Err(VersionError::InvalidVersion(_))));
    }

    #[test]
    fn matches_no_constraint() {
        let r = PluginRef::parse("rate-limit").unwrap();
        assert!(r.matches(&Version::parse("1.0.0").unwrap()));
        assert!(r.matches(&Version::parse("2.0.0").unwrap()));
        assert!(r.matches(&Version::parse("0.1.0").unwrap()));
    }

    #[test]
    fn matches_exact_version() {
        let r = PluginRef::parse("rate-limit@1.0.0").unwrap();
        assert!(r.matches(&Version::parse("1.0.0").unwrap()));
        assert!(!r.matches(&Version::parse("1.0.1").unwrap()));
        assert!(!r.matches(&Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn matches_caret_range() {
        let r = PluginRef::parse("rate-limit@^1.0.0").unwrap();
        assert!(r.matches(&Version::parse("1.0.0").unwrap()));
        assert!(r.matches(&Version::parse("1.5.0").unwrap()));
        assert!(r.matches(&Version::parse("1.9.9").unwrap()));
        assert!(!r.matches(&Version::parse("2.0.0").unwrap()));
        assert!(!r.matches(&Version::parse("0.9.0").unwrap()));
    }

    #[test]
    fn matches_tilde_range() {
        let r = PluginRef::parse("rate-limit@~1.2.0").unwrap();
        assert!(r.matches(&Version::parse("1.2.0").unwrap()));
        assert!(r.matches(&Version::parse("1.2.5").unwrap()));
        assert!(!r.matches(&Version::parse("1.3.0").unwrap()));
        assert!(!r.matches(&Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn select_version_highest() {
        let r = PluginRef::parse("rate-limit@^1.0.0").unwrap();
        let versions = vec![
            Version::parse("1.0.0").unwrap(),
            Version::parse("1.2.0").unwrap(),
            Version::parse("1.5.0").unwrap(),
            Version::parse("2.0.0").unwrap(),
        ];

        let selected = r.select_version(&versions);
        assert_eq!(selected, Some(&Version::parse("1.5.0").unwrap()));
    }

    #[test]
    fn select_version_no_match() {
        let r = PluginRef::parse("rate-limit@^3.0.0").unwrap();
        let versions = vec![
            Version::parse("1.0.0").unwrap(),
            Version::parse("2.0.0").unwrap(),
        ];

        let selected = r.select_version(&versions);
        assert!(selected.is_none());
    }

    #[test]
    fn select_version_latest_when_no_constraint() {
        let r = PluginRef::parse("rate-limit").unwrap();
        let versions = vec![
            Version::parse("1.0.0").unwrap(),
            Version::parse("2.0.0").unwrap(),
            Version::parse("1.5.0").unwrap(),
        ];

        let selected = r.select_version(&versions);
        assert_eq!(selected, Some(&Version::parse("2.0.0").unwrap()));
    }
}
