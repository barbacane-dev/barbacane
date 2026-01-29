//! OpenAPI 3.x and AsyncAPI 3.x spec parser.
//!
//! Reads YAML/JSON specs, extracts `paths`, `servers`, and `x-barbacane-*`
//! vendor extensions. Auto-detects the format from root `openapi` or `asyncapi` field.

pub mod error;
pub mod model;
pub mod parser;

pub use error::ParseError;
pub use model::{
    ApiSpec, ContentSchema, DispatchConfig, MiddlewareConfig, Operation, Parameter, RequestBody,
    SpecFormat,
};
pub use parser::{parse_spec, parse_spec_file};
