//! Spec compilation benchmarks.
//!
//! Measures the cost of compiling OpenAPI specs with varying numbers of
//! operations into .bca artifacts. Includes spec parsing, validation,
//! schema checking, JSON serialization, and tar.gz packaging.
//!
//! Run with: cargo bench -p barbacane-compiler --bench compilation

use std::io::Write;
use std::path::Path;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use barbacane_compiler::{compile_with_options, CompileOptions};

/// Generate an OpenAPI 3.1 spec YAML with N operations.
///
/// Each operation has a path param, query param, and request body schema.
fn generate_spec(operation_count: usize) -> String {
    let mut yaml = String::from(
        r#"openapi: "3.1.0"
info:
  title: Benchmark API
  version: "1.0.0"
paths:
"#,
    );

    for i in 0..operation_count {
        let resource = format!("resource{}", i);
        yaml.push_str(&format!(
            r#"  /{resource}/{{id}}:
    get:
      operationId: get_{resource}
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
            format: uuid
        - name: limit
          in: query
          schema:
            type: integer
            minimum: 1
            maximum: 100
      requestBody:
        required: false
        content:
          application/json:
            schema:
              type: object
              properties:
                name:
                  type: string
                  minLength: 1
                  maxLength: 255
                email:
                  type: string
                  format: email
"#,
            resource = resource,
        ));
    }

    yaml
}

fn bench_spec_compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("spec_compilation");
    let options = CompileOptions {
        allow_plaintext: true,
        ..CompileOptions::default()
    };

    for op_count in [10, 50, 100] {
        let spec_yaml = generate_spec(op_count);

        group.bench_with_input(
            BenchmarkId::new("compile", format!("{}_ops", op_count)),
            &spec_yaml,
            |b, spec_yaml| {
                // Write spec to temp file (setup, not measured per-iter)
                let temp_dir = tempfile::TempDir::new().unwrap();
                let spec_path = temp_dir.path().join("api.yaml");
                let mut f = std::fs::File::create(&spec_path).unwrap();
                f.write_all(spec_yaml.as_bytes()).unwrap();
                let output_path = temp_dir.path().join("output.bca");

                b.iter(|| {
                    let spec_ref: &Path = &spec_path;
                    let result =
                        compile_with_options(&[spec_ref], black_box(&output_path), &options);
                    black_box(result.unwrap());
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_spec_compilation);
criterion_main!(benches);
