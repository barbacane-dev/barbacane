//! `openapi.yaml` is the contract of the control-plane API: the server serves
//! every operation it describes and nothing else, and the CLI is generated
//! from it.

use std::collections::BTreeSet;

use serde_json::Value;

const SPEC: &str = include_str!("../openapi.yaml");
const ROUTER: &str = include_str!("../src/api/router.rs");

/// Routes outside the API the spec describes: the spec itself and its viewer.
const NOT_IN_SPEC: [(&str, &str); 2] = [("GET", "/openapi"), ("GET", "/docs")];

const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// `/projects/{id}` and `/projects/{projectId}` are the same route.
fn normalized(path: &str) -> String {
    let mut out = String::new();
    let mut in_param = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push_str("{}");
            }
            '}' => in_param = false,
            _ if !in_param => out.push(c),
            _ => {}
        }
    }
    out
}

/// `(METHOD, path)` for every `.route("path", method(...))` in the router.
fn routes() -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for chunk in ROUTER.split(".route(").skip(1) {
        let Some(rest) = chunk.trim_start().strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.find('"') else {
            continue;
        };
        let path = &rest[..end];
        let method: String = rest[end + 1..]
            .trim_start()
            .trim_start_matches(',')
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        assert!(
            METHODS.contains(&method.as_str()),
            "unexpected route shape for {path}: {method}"
        );
        found.insert((method.to_uppercase(), normalized(path)));
    }
    found
}

fn spec() -> Value {
    barbacane_compiler::yaml::from_str(SPEC).expect("openapi.yaml parses")
}

fn operations(spec: &Value) -> Vec<(String, String, &Value)> {
    let mut found = Vec::new();
    for (path, item) in spec["paths"].as_object().expect("paths") {
        for method in METHODS {
            if let Some(operation) = item.get(method) {
                found.push((method.to_uppercase(), path.clone(), operation));
            }
        }
    }
    found
}

#[test]
fn the_server_serves_every_operation_the_spec_describes_and_nothing_else() {
    let spec = spec();
    let described: BTreeSet<(String, String)> = operations(&spec)
        .into_iter()
        .map(|(method, path, _)| (method, normalized(&path)))
        .collect();
    let mut served = routes();
    assert!(served.len() > 40, "found only {} routes", served.len());
    for (method, path) in NOT_IN_SPEC {
        assert!(
            served.remove(&(method.to_string(), path.to_string())),
            "{method} {path} is no longer served; drop it from NOT_IN_SPEC"
        );
    }

    let undocumented: Vec<_> = served.difference(&described).collect();
    let unserved: Vec<_> = described.difference(&served).collect();
    assert!(
        undocumented.is_empty(),
        "served but not in openapi.yaml: {undocumented:?}"
    );
    assert!(
        unserved.is_empty(),
        "in openapi.yaml but not served: {unserved:?}"
    );
}

#[test]
fn every_operation_has_a_unique_operation_id_and_a_tag() {
    let spec = spec();
    let mut ids = BTreeSet::new();
    for (method, path, operation) in operations(&spec) {
        let id = operation["operationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
        assert!(ids.insert(id.to_string()), "operationId {id} is used twice");
        assert!(
            operation["tags"].as_array().is_some_and(|t| t.len() == 1),
            "{method} {path} needs exactly one tag"
        );
    }
}

#[test]
fn the_route_scan_reads_every_shape_the_router_uses() {
    let found = routes();
    for expected in [
        ("GET", "/health"),
        ("POST", "/specs"),
        ("PATCH", "/specs/{}/operations"),
        ("DELETE", "/projects/{}/plugins/{}"),
    ] {
        assert!(
            found.contains(&(expected.0.to_string(), expected.1.to_string())),
            "{expected:?} in {found:?}"
        );
    }
}
