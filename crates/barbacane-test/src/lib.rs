//! Test harnesses for Barbacane gateway and plugins.
//!
//! Provides `TestGateway` for full-stack integration tests.

#[cfg(test)]
pub mod cli;
pub mod gateway;

pub use gateway::{
    assert_status, find_workspace_binary, generate_test_certificates, TestCertificates, TestError,
    TestGateway,
};

/// Whether a test has what it needs to run: a service, a binary, a database.
/// Without it, a local run skips the test and says why; a CI run (`CI` set)
/// fails it, since a job that lacks what its tests need is broken, and a test
/// that skips there passes without having run.
pub fn require(available: bool, what: &str) -> bool {
    if available {
        return true;
    }
    assert!(
        !under_ci(),
        "{what}: unavailable, and under CI a test does not skip for want of it"
    );
    eprintln!("skip: {what}: unavailable");
    false
}

/// [`require`] for a value a test builds: `value` when there is one; without
/// one, `None` on a local run and a failure under CI.
pub fn required<T>(value: Option<T>, what: &str) -> Option<T> {
    require(value.is_some(), what).then_some(value).flatten()
}

/// Whether the tests run under CI: `CI` is set to anything but `false`, as
/// GitHub Actions and most CI services set it.
fn under_ci() -> bool {
    std::env::var("CI").is_ok_and(|value| !value.is_empty() && value != "false")
}

#[cfg(test)]
mod require_tests {
    use super::require;

    /// One test touches `CI`, so no other test sees it change.
    #[test]
    fn a_missing_requirement_skips_locally_and_fails_under_ci() {
        let previous = std::env::var_os("CI");

        // SAFETY: this module's only test; no other thread reads `CI` here.
        unsafe { std::env::remove_var("CI") };
        assert!(require(true, "x"));
        assert!(!require(false, "x"), "a local run skips");

        unsafe { std::env::set_var("CI", "true") };
        assert!(require(true, "x"));
        let failed = std::panic::catch_unwind(|| require(false, "PostgreSQL"));
        assert!(failed.is_err(), "a CI run fails");

        unsafe { std::env::set_var("CI", "false") };
        assert!(!require(false, "x"), "`CI=false` is a local run");

        match previous {
            Some(value) => unsafe { std::env::set_var("CI", value) },
            None => unsafe { std::env::remove_var("CI") },
        }
    }
}
