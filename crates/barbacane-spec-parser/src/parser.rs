use crate::error::ParseError;
use crate::model::ApiSpec;

/// Parse an OpenAPI or AsyncAPI spec from a YAML/JSON string.
pub fn parse_spec(_input: &str) -> Result<ApiSpec, ParseError> {
    todo!("M1: implement spec parsing")
}

/// Parse a spec from a file path.
pub fn parse_spec_file(path: &std::path::Path) -> Result<ApiSpec, ParseError> {
    let content = std::fs::read_to_string(path)?;
    parse_spec(&content)
}
