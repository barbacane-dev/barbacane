/// Full-stack test harness.
///
/// Compiles a spec into an in-memory artifact, boots the data plane
/// on a random port, and provides HTTP request helpers.
pub struct TestGateway {
    _placeholder: (),
}

impl TestGateway {
    /// Create a TestGateway from a spec YAML/JSON file.
    pub async fn from_spec(_path: &str) -> Self {
        todo!("M1: implement TestGateway")
    }
}
