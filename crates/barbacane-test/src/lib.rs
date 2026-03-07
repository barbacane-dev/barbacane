//! Test harnesses for Barbacane gateway and plugins.
//!
//! Provides `TestGateway` for full-stack integration tests.

#[cfg(test)]
pub mod cli;
pub mod gateway;

pub use gateway::{
    assert_status, generate_test_certificates, TestCertificates, TestError, TestGateway,
};
