//! Validation benchmarks for the request validator.
//!
//! Run with: cargo bench -p barbacane-validator

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

use barbacane_spec_parser::{ContentSchema, Parameter, RequestBody};
use barbacane_validator::OperationValidator;

/// Create parameters for benchmarking.
fn create_parameters() -> Vec<Parameter> {
    vec![
        Parameter {
            name: "id".to_string(),
            location: "path".to_string(),
            required: true,
            schema: Some(json!({
                "type": "string",
                "format": "uuid"
            })),
        },
        Parameter {
            name: "page".to_string(),
            location: "query".to_string(),
            required: false,
            schema: Some(json!({
                "type": "integer",
                "minimum": 1
            })),
        },
        Parameter {
            name: "limit".to_string(),
            location: "query".to_string(),
            required: false,
            schema: Some(json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 100
            })),
        },
        Parameter {
            name: "x-api-key".to_string(),
            location: "header".to_string(),
            required: true,
            schema: Some(json!({
                "type": "string",
                "minLength": 32
            })),
        },
    ]
}

/// Create a request body schema for benchmarking.
fn create_request_body() -> RequestBody {
    let mut content = BTreeMap::new();
    content.insert(
        "application/json".to_string(),
        ContentSchema {
            schema: Some(json!({
                "type": "object",
                "required": ["name", "email"],
                "properties": {
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 100
                    },
                    "email": {
                        "type": "string",
                        "format": "email"
                    },
                    "age": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 150
                    },
                    "tags": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "maxItems": 10
                    }
                }
            })),
        },
    );

    RequestBody {
        required: true,
        content,
    }
}

fn bench_validator_creation(c: &mut Criterion) {
    let params = create_parameters();
    let body = create_request_body();

    c.bench_function("validator_creation", |b| {
        b.iter(|| {
            black_box(OperationValidator::new(&params, Some(&body)));
        });
    });
}

fn bench_path_param_validation(c: &mut Criterion) {
    let params = create_parameters();
    let validator = OperationValidator::new(&params, None);

    let valid_params = vec![(
        "id".to_string(),
        "550e8400-e29b-41d4-a716-446655440000".to_string(),
    )];

    let invalid_params = vec![("id".to_string(), "not-a-uuid".to_string())];

    let mut group = c.benchmark_group("path_param_validation");

    group.bench_function("valid_uuid", |b| {
        b.iter(|| {
            black_box(validator.validate_path_params(&valid_params));
        });
    });

    group.bench_function("invalid_uuid", |b| {
        b.iter(|| {
            black_box(validator.validate_path_params(&invalid_params));
        });
    });

    group.finish();
}

fn bench_query_param_validation(c: &mut Criterion) {
    let params = create_parameters();
    let validator = OperationValidator::new(&params, None);

    let valid_query = "page=1&limit=50";
    let invalid_query = "page=0&limit=1000"; // Below minimum, above maximum

    let mut group = c.benchmark_group("query_param_validation");

    group.bench_function("valid_params", |b| {
        b.iter(|| {
            black_box(validator.validate_query_params(Some(valid_query)));
        });
    });

    group.bench_function("invalid_params", |b| {
        b.iter(|| {
            black_box(validator.validate_query_params(Some(invalid_query)));
        });
    });

    group.finish();
}

fn bench_body_validation(c: &mut Criterion) {
    let params = create_parameters();
    let body = create_request_body();
    let validator = OperationValidator::new(&params, Some(&body));

    // Small valid body
    let small_body = json!({
        "name": "John Doe",
        "email": "john@example.com"
    });

    // Large valid body
    let large_body = json!({
        "name": "John Doe",
        "email": "john@example.com",
        "age": 30,
        "tags": ["tag1", "tag2", "tag3", "tag4", "tag5"]
    });

    // Invalid body
    let invalid_body = json!({
        "name": "",
        "email": "not-an-email"
    });

    let mut group = c.benchmark_group("body_validation");

    for (name, body_json) in [
        ("small_valid", &small_body),
        ("large_valid", &large_body),
        ("invalid", &invalid_body),
    ] {
        let body_bytes = serde_json::to_vec(body_json).unwrap();

        group.bench_with_input(
            BenchmarkId::new("json", name),
            &body_bytes,
            |b, body_bytes| {
                b.iter(|| {
                    black_box(validator.validate_body(Some("application/json"), body_bytes));
                });
            },
        );
    }

    group.finish();
}

fn bench_full_request_validation(c: &mut Criterion) {
    let params = create_parameters();
    let body = create_request_body();
    let validator = OperationValidator::new(&params, Some(&body));

    let path_params = vec![(
        "id".to_string(),
        "550e8400-e29b-41d4-a716-446655440000".to_string(),
    )];

    let query_string = "page=1&limit=50";

    let headers: HashMap<String, String> = [
        (
            "x-api-key".to_string(),
            "12345678901234567890123456789012".to_string(),
        ),
        ("content-type".to_string(), "application/json".to_string()),
    ]
    .into_iter()
    .collect();

    let body_json = json!({
        "name": "John Doe",
        "email": "john@example.com"
    });
    let body_bytes = serde_json::to_vec(&body_json).unwrap();

    c.bench_function("full_request_validation", |b| {
        b.iter(|| {
            black_box(validator.validate_request(
                &path_params,
                Some(query_string),
                &headers,
                Some("application/json"),
                &body_bytes,
            ));
        });
    });
}

criterion_group!(
    benches,
    bench_validator_creation,
    bench_path_param_validation,
    bench_query_param_validation,
    bench_body_validation,
    bench_full_request_validation
);
criterion_main!(benches);
