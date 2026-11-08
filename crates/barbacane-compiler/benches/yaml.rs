//! The YAML module against libyaml, over the specs in `BARBACANE_CORPUS_DIR`.
//!
//! The module reads a document twice, once to bound what its aliases expand
//! to and once to build the value; both passes are measured.
//!
//! Run with: BARBACANE_CORPUS_DIR=<dir> cargo bench -p barbacane-compiler --bench yaml

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::Value;

fn bench_yaml(c: &mut Criterion) {
    let Ok(dir) = std::env::var("BARBACANE_CORPUS_DIR") else {
        eprintln!("skip: BARBACANE_CORPUS_DIR is not set");
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("corpus directory")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml" | "json")
            )
        })
        .collect();
    paths.sort();

    let mut group = c.benchmark_group("yaml");
    group.sample_size(10);
    for path in paths {
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .to_string();
        let text = std::fs::read_to_string(&path).expect("read");
        group.bench_function(format!("module/{name}"), |b| {
            b.iter(|| black_box(barbacane_compiler::yaml::from_str::<Value>(&text).expect("parse")))
        });
        group.bench_function(format!("libyaml/{name}"), |b| {
            b.iter(|| black_box(yaml_serde::from_str::<Value>(&text).expect("parse")))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_yaml);
criterion_main!(benches);
