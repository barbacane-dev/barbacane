use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::Value;

use super::error::ParseError;
use super::model::{
    ApiSpec, ContentSchema, DispatchConfig, Message, MiddlewareConfig, Operation, Parameter,
    RequestBody, ResponseContent, SecurityRequirement, SecurityScheme, SpecFormat,
};

/// Resolve a JSON Reference like `#/components/schemas/User` from the spec root.
///
/// Only local references (`#/...`) are supported. Returns `None` for external refs.
fn resolve_ref<'a>(root: &'a Value, ref_path: &str) -> Option<&'a Value> {
    if !ref_path.starts_with("#/") {
        return None;
    }
    let mut current = root;
    for segment in ref_path[2..].split('/') {
        let unescaped = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&unescaped)?;
    }
    Some(current)
}

/// Definitions a schema carries for the references that cannot be inlined.
///
/// A schema travels alone in the artifact, with no document to resolve against,
/// so a reference that survives inlining has to point inside the schema itself.
#[derive(Default)]
struct SchemaDefs {
    /// Reference pointer to the `$defs` key standing in for it.
    names: BTreeMap<String, String>,
    /// `$defs` key to its body, absent while that body is still being resolved.
    bodies: BTreeMap<String, Option<Value>>,
}

impl SchemaDefs {
    /// The `$defs` key for a reference, derived from its last segment and made
    /// unique so two pointers ending in the same name stay distinct.
    fn name_for(&mut self, ref_str: &str) -> String {
        if let Some(existing) = self.names.get(ref_str) {
            return existing.clone();
        }
        let base: String = ref_str
            .rsplit('/')
            .next()
            .unwrap_or("schema")
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let base = if base.is_empty() {
            "schema".to_string()
        } else {
            base
        };
        let mut name = base.clone();
        let mut n = 2;
        while self.bodies.contains_key(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        self.names.insert(ref_str.to_string(), name.clone());
        self.bodies.insert(name.clone(), None);
        name
    }

    /// Reserve a unique name for a definition the schema declared itself, which
    /// has no document pointer to key it by.
    fn reserve(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.bodies.contains_key(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        self.bodies.insert(name.clone(), None);
        name
    }
}

/// Rewrite the draft-4 exclusive bounds OpenAPI 3.0 uses.
///
/// 3.0 carries `exclusiveMinimum` and `exclusiveMaximum` as booleans that
/// qualify `minimum` and `maximum`. JSON Schema 2020-12, which the validator
/// compiles against, carries the bound itself as the value. An unconverted
/// boolean makes the schema fail to build, and a schema that fails to build is
/// simply not validated, so the constraint disappears without a word.
///
/// Only schema-valued positions are visited. `enum`, `const`, `default` and the
/// example keywords hold instance data, which may happen to look like a schema,
/// and rewriting it would change the value a request is compared against.
fn convert_draft4_exclusive_bounds(value: &mut Value) {
    let Value::Object(obj) = value else {
        return;
    };

    for (exclusive, bound) in [
        ("exclusiveMinimum", "minimum"),
        ("exclusiveMaximum", "maximum"),
    ] {
        match obj.get(exclusive).and_then(Value::as_bool) {
            // `true` moves the bound onto the exclusive keyword.
            Some(true) => {
                if let Some(limit) = obj.get(bound).cloned() {
                    obj.insert(exclusive.to_string(), limit);
                    obj.remove(bound);
                } else {
                    // Nothing to be exclusive about.
                    obj.remove(exclusive);
                }
            }
            // `false` is the default, and the bound stays inclusive.
            Some(false) => {
                obj.remove(exclusive);
            }
            None => {}
        }
    }

    // A map of schemas keyed by name.
    for keyword in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(Value::Object(map)) = obj.get_mut(keyword) {
            for schema in map.values_mut() {
                convert_draft4_exclusive_bounds(schema);
            }
        }
    }

    // A list of schemas.
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(Value::Array(list)) = obj.get_mut(keyword) {
            for schema in list {
                convert_draft4_exclusive_bounds(schema);
            }
        }
    }

    // One schema, except `items`, which 3.0 also allows as a list.
    for keyword in [
        "items",
        "additionalItems",
        "additionalProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
        "unevaluatedItems",
        "unevaluatedProperties",
    ] {
        match obj.get_mut(keyword) {
            Some(Value::Array(list)) => {
                for schema in list {
                    convert_draft4_exclusive_bounds(schema);
                }
            }
            Some(schema @ Value::Object(_)) => convert_draft4_exclusive_bounds(schema),
            _ => {}
        }
    }
}

/// `true` when the document is OpenAPI 3.0, whose schemas use draft-4 spellings.
fn is_openapi_30(root: &Value) -> bool {
    root.get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with("3.0"))
}

/// Resolve a schema's `$ref` pointers into a self-contained schema.
///
/// Every referenced definition is carried in `$defs` and pointed at, so the
/// schema resolves against itself once it is separated from the document.
fn resolve_schema(value: &Value, root: &Value, defs: &mut SchemaDefs) -> Result<Value, ParseError> {
    let mut resolved = resolve_schema_refs(value, root, defs, &BTreeMap::new())?;
    if is_openapi_30(root) {
        convert_draft4_exclusive_bounds(&mut resolved);
    }
    Ok(resolved)
}

/// The document's definition pool, as the `$defs` object every schema in it
/// points into.
///
/// One pool per document rather than one per schema. A definition reachable
/// from many operations is then held once instead of once per operation, which
/// is what a document like Stripe's does with its `error` schema: 594
/// references, one per operation, each reaching most of the schema graph.
fn finish_schema_defs(defs: SchemaDefs, root: &Value) -> Option<Value> {
    let openapi_30 = is_openapi_30(root);
    let mut map = serde_json::Map::new();
    for (name, body) in defs.bodies {
        if let Some(mut body) = body {
            if openapi_30 {
                convert_draft4_exclusive_bounds(&mut body);
            }
            map.insert(name, body);
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

/// Recursively rewrite `$ref` pointers to point inside the schema.
///
/// Each referenced definition is resolved once into `$defs` and referred to
/// from every use site. Inlining instead would copy a definition per use, which
/// on a document that reuses its schemas heavily expands combinatorially, and
/// cannot terminate at all when a definition refers back to itself.
fn resolve_schema_refs(
    value: &Value,
    root: &Value,
    defs: &mut SchemaDefs,
    scope: &BTreeMap<String, String>,
) -> Result<Value, ParseError> {
    match value {
        Value::Object(obj) => {
            if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()) {
                // A pointer into definitions the source schema declared. Those
                // are hoisted and renamed, so it points at the new name.
                // Only the first token names the definition. Anything after it
                // addresses a place inside that definition and is carried over
                // unchanged, so a pointer such as `#/$defs/Money/properties/amount`
                // follows the definition to its new name instead of resolving
                // against whichever definition kept the old one.
                if let Some(suffix) = ref_str.strip_prefix("#/$defs/") {
                    let (first, rest) = match suffix.split_once('/') {
                        Some((first, rest)) => (first, Some(rest)),
                        None => (suffix, None),
                    };
                    return Ok(match scope.get(&pointer_unescape(first)) {
                        Some(hoisted) => {
                            let mut pointer = format!("#/$defs/{}", pointer_escape(hoisted));
                            if let Some(rest) = rest {
                                pointer.push('/');
                                pointer.push_str(rest);
                            }
                            pointer_ref(&pointer)
                        }
                        None => value.clone(),
                    });
                }
                // An already-known pointer needs no second resolution, which is
                // also what stops a cycle: the body is in flight above us.
                if let Some(name) = defs.names.get(ref_str).cloned() {
                    return Ok(local_ref(&name));
                }
                let name = defs.name_for(ref_str);
                let target = resolve_ref(root, ref_str)
                    .ok_or_else(|| ParseError::UnresolvedRef(ref_str.to_string()))?;
                let resolved = resolve_schema_refs(target, root, defs, scope)?;
                defs.bodies.insert(name.clone(), Some(resolved));
                Ok(local_ref(&name))
            } else {
                // Definitions the schema declares itself are lifted to the one
                // set the schema ends up carrying, under a name reserved there,
                // so a pointer at them still resolves once the schema is
                // detached from its document.
                let mut scope = scope.clone();
                if let Some(Value::Object(local_defs)) = obj.get("$defs") {
                    for name in local_defs.keys() {
                        let hoisted = defs.reserve(name);
                        scope.insert(name.clone(), hoisted);
                    }
                    for (name, body) in local_defs {
                        let hoisted = scope.get(name).cloned().unwrap_or_else(|| name.clone());
                        let resolved = resolve_schema_refs(body, root, defs, &scope)?;
                        defs.bodies.insert(hoisted, Some(resolved));
                    }
                }

                let mut new_obj = serde_json::Map::with_capacity(obj.len());
                for (key, val) in obj {
                    // Its definitions now live in the schema's own set.
                    if key == "$defs" {
                        continue;
                    }
                    new_obj.insert(key.clone(), resolve_schema_refs(val, root, defs, &scope)?);
                }
                Ok(Value::Object(new_obj))
            }
        }
        Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr
                .iter()
                .map(|v| resolve_schema_refs(v, root, defs, scope))
                .collect();
            Ok(Value::Array(items?))
        }
        other => Ok(other.clone()),
    }
}

/// A reference to a definition carried by the schema itself.
fn local_ref(name: &str) -> Value {
    pointer_ref(&format!("#/$defs/{}", pointer_escape(name)))
}

/// A `$ref` node holding `pointer`.
fn pointer_ref(pointer: &str) -> Value {
    let mut obj = serde_json::Map::with_capacity(1);
    obj.insert("$ref".to_string(), Value::String(pointer.to_string()));
    Value::Object(obj)
}

/// Escape a name for use as one JSON Pointer token (RFC 6901): `~` then `/`.
fn pointer_escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Decode one JSON Pointer token (RFC 6901): `~1` then `~0`.
fn pointer_unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Resolve a component entry that may be a `$ref` into the object it names.
///
/// A reference chain is followed to its end; `visited` detects a cycle. Returns
/// `None` for an entry that is not an object.
fn resolve_component_ref<'a>(
    item: &'a Value,
    root: &'a Value,
    visited: &mut HashSet<String>,
) -> Result<Option<&'a Value>, ParseError> {
    let mut current = item;
    loop {
        let Some(obj) = current.as_object() else {
            return Ok(None);
        };
        let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()) else {
            return Ok(Some(current));
        };
        if !visited.insert(ref_str.to_string()) {
            return Err(ParseError::SchemaError(format!(
                "circular $ref detected: {}",
                ref_str
            )));
        }
        current = resolve_ref(root, ref_str)
            .ok_or_else(|| ParseError::UnresolvedRef(ref_str.to_string()))?;
    }
}

/// Merge a path item's parameters with an operation's own.
///
/// A parameter is unique by name and location, and the operation's definition
/// replaces the path item's rather than joining it. Keeping both would validate
/// a value twice, so a stale inherited schema could reject what the operation
/// accepts. Header names are compared case-insensitively, as HTTP treats them.
fn merge_parameters(path_params: &[Parameter], op_params: Vec<Parameter>) -> Vec<Parameter> {
    let overridden = |p: &Parameter| {
        op_params.iter().any(|o| {
            o.location == p.location
                && if o.location == "header" {
                    o.name.eq_ignore_ascii_case(&p.name)
                } else {
                    o.name == p.name
                }
        })
    };
    let mut merged: Vec<Parameter> = path_params
        .iter()
        .filter(|p| !overridden(p))
        .cloned()
        .collect();
    merged.extend(op_params);
    merged
}

/// Parse `components.securitySchemes`, resolving `$ref` entries.
///
/// A scheme with an unknown `type`, or missing the fields its type requires, is
/// an error: the scheme decides which credential header an operation accepts.
fn parse_security_schemes(root: &Value) -> Result<BTreeMap<String, SecurityScheme>, ParseError> {
    let Some(schemes) = root
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(|s| s.as_object())
    else {
        return Ok(BTreeMap::new());
    };

    let mut out = BTreeMap::new();
    for (name, value) in schemes {
        let mut visited = HashSet::new();
        let Some(resolved) = resolve_component_ref(value, root, &mut visited)? else {
            continue;
        };
        out.insert(name.clone(), parse_security_scheme(name, resolved)?);
    }
    Ok(out)
}

/// Parse one security scheme object.
///
/// Shared by `components.securitySchemes` and the inline schemes an AsyncAPI
/// server may declare, so both understand the same vocabulary.
fn parse_security_scheme(name: &str, resolved: &Value) -> Result<SecurityScheme, ParseError> {
    let Some(obj) = resolved.as_object() else {
        return Err(ParseError::SchemaError(format!(
            "security scheme '{}' must be an object",
            name
        )));
    };
    let Some(kind) = obj.get("type").and_then(|v| v.as_str()) else {
        return Err(ParseError::SchemaError(format!(
            "security scheme '{}' has no 'type'",
            name
        )));
    };

    let scheme = match kind {
        // OpenAPI's `apiKey` names a request parameter. AsyncAPI reuses the
        // word for a broker credential placed in the connection's user or
        // password field, which has no `name` and reaches no request.
        "apiKey" => {
            let location = obj
                .get("in")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ParseError::SchemaError(format!(
                        "security scheme '{}' of type apiKey has no 'in'",
                        name
                    ))
                })?
                .to_ascii_lowercase();
            match location.as_str() {
                "user" | "password" => SecurityScheme::Transport {
                    kind: kind.to_string(),
                },
                _ => {
                    let key_name = obj.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                        ParseError::SchemaError(format!(
                            "security scheme '{}' of type apiKey has no 'name'",
                            name
                        ))
                    })?;
                    SecurityScheme::ApiKey {
                        name: key_name.to_string(),
                        location,
                    }
                }
            }
        }
        "http" => {
            let http_scheme = obj.get("scheme").and_then(|v| v.as_str()).ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "security scheme '{}' of type http has no 'scheme'",
                    name
                ))
            })?;
            SecurityScheme::Http {
                scheme: http_scheme.to_ascii_lowercase(),
            }
        }
        "oauth2" => SecurityScheme::OAuth2,
        "openIdConnect" => SecurityScheme::OpenIdConnect,
        "mutualTLS" => SecurityScheme::MutualTls,
        // AsyncAPI's spelling of an API key in a header, query parameter
        // or cookie. Same shape as OpenAPI's `apiKey`, different name.
        "httpApiKey" => {
            let key_name = obj.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "security scheme '{}' of type httpApiKey has no 'name'",
                    name
                ))
            })?;
            let location = obj.get("in").and_then(|v| v.as_str()).ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "security scheme '{}' of type httpApiKey has no 'in'",
                    name
                ))
            })?;
            SecurityScheme::ApiKey {
                name: key_name.to_string(),
                location: location.to_ascii_lowercase(),
            }
        }
        // The rest of AsyncAPI's set. A credential the transport or the
        // broker carries, so none reaches a request header.
        "X509"
        | "symmetricEncryption"
        | "asymmetricEncryption"
        | "scramSha256"
        | "scramSha512"
        | "gssapi"
        | "plain"
        | "userPassword" => SecurityScheme::Transport {
            kind: kind.to_string(),
        },
        other => {
            return Err(ParseError::SchemaError(format!(
                "security scheme '{}' has unknown type '{}'",
                name, other
            )));
        }
    };
    Ok(scheme)
}

/// Parse a `security` list. `None` when the key is absent, so an operation can
/// be told apart from one declaring `security: []` to opt out of the root.
fn parse_security(obj: &serde_json::Map<String, Value>) -> Option<Vec<SecurityRequirement>> {
    let arr = obj.get("security")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let Some(map) = entry.as_object() else {
            continue;
        };
        let mut requirement = SecurityRequirement::new();
        for (scheme, scopes) in map {
            let scopes = scopes
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            requirement.insert(scheme.clone(), scopes);
        }
        out.push(requirement);
    }
    Some(out)
}

/// HTTP methods we recognize in OpenAPI paths.
/// Includes `query` from OpenAPI 3.2 (RFC 9110 extension).
const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace", "query",
];

/// Parse an OpenAPI or AsyncAPI spec from a YAML/JSON string.
pub fn parse_spec(input: &str) -> Result<ApiSpec, ParseError> {
    // Parse YAML (also handles JSON since JSON is valid YAML)
    let root: Value =
        serde_yaml::from_str(input).map_err(|e| ParseError::ParseError(e.to_string()))?;

    let root_obj = root
        .as_object()
        .ok_or_else(|| ParseError::ParseError("spec root must be an object".into()))?;

    // Detect format
    let (format, version) = detect_format(root_obj)?;

    // Extract info
    let info = root_obj
        .get("info")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ParseError::SchemaError("missing 'info' object".into()))?;

    let title = info
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ParseError::SchemaError("missing 'info.title'".into()))?
        .to_string();

    let api_version = info
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();

    // Extract root-level x-barbacane-* extensions
    let extensions = extract_extensions(root_obj);

    // Extract global middlewares
    let global_middlewares = extract_middlewares(root_obj);

    // Security schemes and the root-level requirement they are referenced by
    let mut security_schemes = parse_security_schemes(&root)?;
    let security = parse_security(root_obj);

    // One definition pool for the whole document. Every schema points into it,
    // so a definition many operations reach is held once.
    let mut defs = SchemaDefs::default();

    // Parse operations based on format
    let operations = match format {
        SpecFormat::OpenApi => parse_openapi_paths(root_obj, &root, &mut defs)?,
        SpecFormat::AsyncApi => {
            // AsyncAPI puts the security requirement on the server, and an
            // inline scheme there is registered as it is read.
            let server_security = parse_server_security(root_obj, &root, &mut security_schemes)?;
            let channel_servers = parse_channel_servers(root_obj, &server_security)?;
            parse_asyncapi_channels(
                root_obj,
                &root,
                &server_security,
                &channel_servers,
                &mut defs,
            )?
        }
    };

    let schema_defs = finish_schema_defs(defs, &root);

    Ok(ApiSpec {
        filename: None,
        schema_defs,
        format,
        version,
        title,
        api_version,
        operations,
        global_middlewares,
        extensions,
        security_schemes,
        security,
    })
}

/// Parse a spec from a file path.
pub fn parse_spec_file(path: &std::path::Path) -> Result<ApiSpec, ParseError> {
    let content = std::fs::read_to_string(path)?;
    let mut spec = parse_spec(&content)?;
    spec.filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());
    Ok(spec)
}

/// Detect whether this is OpenAPI or AsyncAPI and extract the version.
fn detect_format(
    root: &serde_json::Map<String, Value>,
) -> Result<(SpecFormat, String), ParseError> {
    if let Some(version) = root.get("openapi").and_then(|v| v.as_str()) {
        if !version.starts_with("3.") {
            return Err(ParseError::SchemaError(format!(
                "unsupported OpenAPI version: {} (only 3.x supported)",
                version
            )));
        }
        Ok((SpecFormat::OpenApi, version.to_string()))
    } else if let Some(version) = root.get("asyncapi").and_then(|v| v.as_str()) {
        if !version.starts_with("3.") {
            return Err(ParseError::SchemaError(format!(
                "unsupported AsyncAPI version: {} (only 3.x supported)",
                version
            )));
        }
        Ok((SpecFormat::AsyncApi, version.to_string()))
    } else {
        Err(ParseError::UnknownFormat)
    }
}

/// Extract all x-barbacane-* keys from an object.
fn extract_extensions(obj: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    obj.iter()
        .filter(|(k, _)| k.starts_with("x-barbacane-"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Extract x-barbacane-middlewares from an object.
fn extract_middlewares(obj: &serde_json::Map<String, Value>) -> Vec<MiddlewareConfig> {
    obj.get("x-barbacane-middlewares")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Extract x-barbacane-dispatch from an operation object.
fn extract_dispatch(obj: &serde_json::Map<String, Value>) -> Option<DispatchConfig> {
    obj.get("x-barbacane-dispatch")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Parse OpenAPI 3.x paths into operations.
fn parse_openapi_paths(
    root: &serde_json::Map<String, Value>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<Vec<Operation>, ParseError> {
    let mut operations = Vec::new();

    let paths = match root.get("paths").and_then(|v| v.as_object()) {
        Some(p) => p,
        None => return Ok(operations), // No paths is valid (empty API)
    };

    for (path, path_item) in paths {
        // A path item may be a reference, which is how a spec reuses one across
        // paths. Reading it without resolving finds no methods, so every
        // operation it holds would be dropped without a word.
        let mut visited = HashSet::new();
        let path_item =
            resolve_component_ref(path_item, spec_root, &mut visited)?.ok_or_else(|| {
                ParseError::SchemaError(format!("path item for '{}' must be an object", path))
            })?;
        let path_obj = path_item.as_object().ok_or_else(|| {
            ParseError::SchemaError(format!("path item for '{}' must be an object", path))
        })?;

        // Path-level parameters (inherited by all operations)
        let path_params = parse_parameters(path_obj, spec_root, defs)?;

        for method in HTTP_METHODS {
            if let Some(op_value) = path_obj.get(*method) {
                let op_obj = op_value.as_object().ok_or_else(|| {
                    ParseError::SchemaError(format!(
                        "operation {} {} must be an object",
                        method.to_uppercase(),
                        path
                    ))
                })?;

                let params =
                    merge_parameters(&path_params, parse_parameters(op_obj, spec_root, defs)?);

                let operation_id = op_obj
                    .get("operationId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let summary = op_obj
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let description = op_obj
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let request_body = parse_request_body(op_obj, spec_root, defs)?;
                let responses = parse_responses(op_obj, spec_root, defs)?;

                let dispatch = extract_dispatch(op_obj);

                let middlewares = if op_obj.contains_key("x-barbacane-middlewares") {
                    Some(extract_middlewares(op_obj))
                } else {
                    None
                };

                // Extract deprecated flag (standard OpenAPI field)
                let deprecated = op_obj
                    .get("deprecated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Extract sunset date from x-sunset extension (RFC 8594)
                let sunset = op_obj
                    .get("x-sunset")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let extensions = extract_extensions(op_obj);

                operations.push(Operation {
                    path: path.clone(),
                    method: method.to_uppercase(),
                    operation_id,
                    summary,
                    description,
                    parameters: params,
                    request_body,
                    dispatch,
                    middlewares,
                    deprecated,
                    sunset,
                    extensions,
                    messages: Vec::new(), // OpenAPI doesn't use AsyncAPI messages
                    bindings: BTreeMap::new(), // OpenAPI doesn't use protocol bindings
                    responses,
                    security: parse_security(op_obj),
                });
            }
        }

        // OpenAPI 3.2: parse additionalOperations (custom HTTP methods)
        if let Some(additional) = path_obj
            .get("additionalOperations")
            .and_then(|v| v.as_object())
        {
            for (method_name, op_value) in additional {
                let op_obj = op_value.as_object().ok_or_else(|| {
                    ParseError::SchemaError(format!(
                        "additionalOperations.{} on {} must be an object",
                        method_name, path
                    ))
                })?;

                let params =
                    merge_parameters(&path_params, parse_parameters(op_obj, spec_root, defs)?);

                let operation_id = op_obj
                    .get("operationId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let summary = op_obj
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let description = op_obj
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let request_body = parse_request_body(op_obj, spec_root, defs)?;
                let responses = parse_responses(op_obj, spec_root, defs)?;
                let dispatch = extract_dispatch(op_obj);

                let middlewares = if op_obj.contains_key("x-barbacane-middlewares") {
                    Some(extract_middlewares(op_obj))
                } else {
                    None
                };

                let deprecated = op_obj
                    .get("deprecated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let sunset = op_obj
                    .get("x-sunset")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let extensions = extract_extensions(op_obj);

                operations.push(Operation {
                    path: path.clone(),
                    method: method_name.to_uppercase(),
                    operation_id,
                    summary,
                    description,
                    parameters: params,
                    request_body,
                    dispatch,
                    middlewares,
                    deprecated,
                    sunset,
                    extensions,
                    messages: Vec::new(),
                    bindings: BTreeMap::new(),
                    responses,
                    security: parse_security(op_obj),
                });
            }
        }
    }

    Ok(operations)
}

/// Parse parameters from a path item or operation object.
///
/// OpenAPI 3.2: `in: querystring` parameters use `content` instead of `schema`.
/// The schema is extracted from `content.<media-type>.schema`.
fn parse_parameters(
    obj: &serde_json::Map<String, Value>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<Vec<Parameter>, ParseError> {
    let Some(arr) = obj.get("parameters").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };

    let mut params = Vec::with_capacity(arr.len());
    for item in arr {
        // An entry may be a `$ref` to `#/components/parameters/...`, which carries
        // `in` and `name` on the target rather than on the entry itself.
        let mut visited = HashSet::new();
        let Some(resolved) = resolve_component_ref(item, spec_root, &mut visited)? else {
            continue;
        };
        let Some(param_obj) = resolved.as_object() else {
            continue;
        };
        let Some(location) = param_obj.get("in").and_then(|v| v.as_str()) else {
            continue;
        };
        let location = location.to_string();

        // OpenAPI 3.2: querystring params use content instead of schema
        let raw_schema = if location == "querystring" {
            extract_content_schema(param_obj)
        } else {
            param_obj.get("schema").cloned()
        };

        let schema = raw_schema
            .map(|s| resolve_schema(&s, spec_root, defs))
            .transpose()?;

        let Some(name) = param_obj.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        params.push(Parameter {
            name: name.to_string(),
            location,
            required: param_obj
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            schema,
        });
    }
    Ok(params)
}

/// Extract schema from a parameter's `content` map (first media type entry).
///
/// Used for `in: querystring` parameters where the schema lives under
/// `content.<media-type>.schema` instead of the top-level `schema` field.
fn extract_content_schema(param_obj: &serde_json::Map<String, Value>) -> Option<Value> {
    let content = param_obj.get("content")?.as_object()?;
    // Use the first (and typically only) media type entry
    let (_media_type, media_obj) = content.iter().next()?;
    media_obj.as_object()?.get("schema").cloned()
}

/// Parse request body from an operation object.
fn parse_request_body(
    obj: &serde_json::Map<String, Value>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<Option<RequestBody>, ParseError> {
    let Some(body) = obj.get("requestBody").and_then(|v| v.as_object()) else {
        return Ok(None);
    };

    let required = body
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let Some(content_obj) = body.get("content").and_then(|v| v.as_object()) else {
        return Ok(None);
    };

    let mut content = BTreeMap::new();
    for (media_type, media_obj) in content_obj {
        let raw_schema = media_obj.as_object().and_then(|o| o.get("schema").cloned());
        let schema = raw_schema
            .map(|s| resolve_schema(&s, spec_root, defs))
            .transpose()?;
        content.insert(media_type.clone(), ContentSchema { schema });
    }

    Ok(Some(RequestBody { required, content }))
}

/// Parse response definitions from an operation object.
fn parse_responses(
    obj: &serde_json::Map<String, Value>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<BTreeMap<String, ResponseContent>, ParseError> {
    let Some(responses) = obj.get("responses").and_then(|v| v.as_object()) else {
        return Ok(BTreeMap::new());
    };

    let mut result = BTreeMap::new();
    for (status_code, resp_value) in responses {
        // Resolve a `$ref` on the response object itself, without descending:
        // each content schema is resolved below, and resolving twice would meet
        // the `#/$defs/` pointers the first pass produced.
        let mut visited = HashSet::new();
        let Some(resolved) = resolve_component_ref(resp_value, spec_root, &mut visited)? else {
            continue;
        };
        let Some(resp_obj) = resolved.as_object() else {
            continue;
        };

        let Some(content_obj) = resp_obj.get("content").and_then(|v| v.as_object()) else {
            continue;
        };

        let mut content = BTreeMap::new();
        for (media_type, media_obj) in content_obj {
            let raw_schema = media_obj.as_object().and_then(|o| o.get("schema").cloned());
            let schema = raw_schema
                .map(|s| resolve_schema(&s, spec_root, defs))
                .transpose()?;
            content.insert(media_type.clone(), ContentSchema { schema });
        }

        if !content.is_empty() {
            result.insert(status_code.clone(), ResponseContent { content });
        }
    }
    Ok(result)
}

/// Parse AsyncAPI 3.x channels and operations.
///
/// AsyncAPI 3.x structure:
/// - `channels`: Map of channel names to channel definitions (address, messages)
/// - `operations`: Map of operation IDs to operation definitions (action, channel ref)
fn parse_asyncapi_channels(
    root: &serde_json::Map<String, Value>,
    spec_root: &Value,
    server_security: &BTreeMap<String, Vec<String>>,
    channel_servers: &BTreeMap<String, Vec<String>>,
    defs: &mut SchemaDefs,
) -> Result<Vec<Operation>, ParseError> {
    let mut operations = Vec::new();

    // Parse channels first to build a lookup map
    let channels = root.get("channels").and_then(|v| v.as_object());
    let ops = root.get("operations").and_then(|v| v.as_object());

    // If no operations defined, return empty
    let ops = match ops {
        Some(o) => o,
        None => return Ok(operations),
    };

    // Build channel lookup: channel_name -> (address, messages, parameters, bindings)
    let channel_lookup = build_channel_lookup(channels, spec_root, defs)?;

    for (op_id, op_value) in ops {
        let op_obj = op_value.as_object().ok_or_else(|| {
            ParseError::SchemaError(format!("operation '{}' must be an object", op_id))
        })?;

        // Extract action (send/receive)
        let action = op_obj
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ParseError::SchemaError(format!("operation '{}' missing 'action' field", op_id))
            })?;

        // Normalize action to uppercase for consistency with HTTP methods
        let method = match action {
            "send" => "SEND",
            "receive" => "RECEIVE",
            other => {
                return Err(ParseError::SchemaError(format!(
                    "operation '{}' has invalid action '{}' (must be 'send' or 'receive')",
                    op_id, other
                )))
            }
        }
        .to_string();

        // Resolve channel reference
        let (address, channel_messages, channel_params, channel_bindings) =
            resolve_channel_ref(op_obj, &channel_lookup, spec_root, defs)?;

        // AsyncAPI declares the credential on the server the channel is reached
        // through, so the operation inherits it from there.
        let reached = operation_servers(op_obj, channel_servers, server_security)?;
        let security = server_security_requirement(reached.as_deref(), server_security);

        // Parse operation-level messages (may override or filter channel messages)
        let messages = parse_operation_messages(op_obj, &channel_messages, spec_root, defs)?;

        // For SEND operations, create a request body from the first message payload
        let request_body = if method == "SEND" && !messages.is_empty() {
            messages.first().and_then(|msg| {
                msg.payload.as_ref().map(|schema| {
                    let content_type = msg
                        .content_type
                        .clone()
                        .unwrap_or_else(|| "application/json".to_string());
                    let mut content = BTreeMap::new();
                    content.insert(
                        content_type,
                        ContentSchema {
                            schema: Some(schema.clone()),
                        },
                    );
                    RequestBody {
                        required: true,
                        content,
                    }
                })
            })
        } else {
            None
        };

        // Merge channel and operation-level bindings
        let mut bindings = channel_bindings;
        if let Some(op_bindings) = op_obj.get("bindings").and_then(|v| v.as_object()) {
            for (protocol, config) in op_bindings {
                bindings.insert(protocol.clone(), config.clone());
            }
        }

        // Extract dispatch config
        let dispatch = extract_dispatch(op_obj);

        // Extract middlewares
        let middlewares = if op_obj.contains_key("x-barbacane-middlewares") {
            Some(extract_middlewares(op_obj))
        } else {
            None
        };

        // Extract deprecated and sunset
        let deprecated = op_obj
            .get("deprecated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let sunset = op_obj
            .get("x-sunset")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let extensions = extract_extensions(op_obj);

        operations.push(Operation {
            path: address,
            method,
            operation_id: Some(op_id.clone()),
            summary: None,
            description: None,
            parameters: channel_params,
            request_body,
            dispatch,
            middlewares,
            deprecated,
            sunset,
            extensions,
            messages,
            bindings,
            responses: BTreeMap::new(),
            security,
        });
    }

    Ok(operations)
}

/// Channel info: (address, messages, parameters, bindings).
type ChannelInfo = (
    String,
    Vec<Message>,
    Vec<Parameter>,
    BTreeMap<String, Value>,
);

/// The security scheme names each AsyncAPI server requires.
///
/// AsyncAPI puts the security requirement on the server rather than the
/// operation: `servers.<name>.security` is a list of Security Scheme Objects,
/// each satisfying the connection on its own. An entry is normally a `$ref` into
/// `components.securitySchemes`; an inline object is registered under a
/// synthesized name so it reaches the header allowlist like any other.
fn parse_server_security(
    root: &serde_json::Map<String, Value>,
    spec_root: &Value,
    schemes: &mut BTreeMap<String, SecurityScheme>,
) -> Result<BTreeMap<String, Vec<String>>, ParseError> {
    let mut by_server = BTreeMap::new();

    let Some(servers) = root.get("servers").and_then(|v| v.as_object()) else {
        return Ok(by_server);
    };

    for (server_name, server) in servers {
        // A server declaring no security is recorded with an empty list, not
        // skipped: a channel reachable through it can be reached without a
        // credential, and that is an alternative the operation must keep.
        let Some(entries) = server.get("security").and_then(|v| v.as_array()) else {
            by_server.insert(server_name.clone(), Vec::new());
            continue;
        };

        let mut names = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match entry.get("$ref").and_then(|v| v.as_str()) {
                Some(reference) => {
                    let name = reference
                        .strip_prefix("#/components/securitySchemes/")
                        .ok_or_else(|| {
                            ParseError::SchemaError(format!(
                                "server '{server_name}' security $ref '{reference}' must point \
                                 into #/components/securitySchemes"
                            ))
                        })?;
                    if !schemes.contains_key(name) {
                        return Err(ParseError::SchemaError(format!(
                            "server '{server_name}' requires security scheme '{name}', which is \
                             not defined under components.securitySchemes"
                        )));
                    }
                    names.push(name.to_string());
                }
                // An inline scheme has no name of its own, so it gets one. The
                // allowlist is keyed on names, and an unnamed scheme would
                // otherwise contribute nothing and silently drop its header.
                None => {
                    let mut visited = HashSet::new();
                    let Some(resolved) = resolve_component_ref(entry, spec_root, &mut visited)?
                    else {
                        continue;
                    };
                    let name = format!("{server_name}#{index}");
                    let scheme = parse_security_scheme(&name, resolved)?;
                    schemes.insert(name.clone(), scheme);
                    names.push(name);
                }
            }
        }

        by_server.insert(server_name.clone(), names);
    }

    Ok(by_server)
}

/// The servers a channel is available on, by channel name.
///
/// A channel may narrow itself with `servers: [$ref]`. One that does not is
/// available on every server, which this records as an absent entry.
///
/// Every reference must resolve. One that does not narrows the channel to a
/// server that is not there, leaving the operation requiring nothing and
/// dropping the credential the document declares, so it is refused instead.
fn parse_channel_servers(
    root: &serde_json::Map<String, Value>,
    servers: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, Vec<String>>, ParseError> {
    let mut by_channel = BTreeMap::new();

    let Some(channels) = root.get("channels").and_then(|v| v.as_object()) else {
        return Ok(by_channel);
    };

    for (channel_name, channel) in channels {
        let Some(entries) = channel.get("servers").and_then(|v| v.as_array()) else {
            continue;
        };

        by_channel.insert(
            channel_name.clone(),
            channel_server_names(channel_name, entries, servers)?,
        );
    }

    Ok(by_channel)
}

/// Resolve a channel's `servers` list to server names.
///
/// Every reference must resolve. One that does not narrows the channel to a
/// server that is not there, leaving the operation requiring nothing and
/// dropping the credential the document declares, so it is refused instead.
fn channel_server_names(
    channel_name: &str,
    entries: &[Value],
    servers: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, ParseError> {
    let mut names = Vec::new();
    for entry in entries {
        let reference = entry.get("$ref").and_then(|v| v.as_str()).ok_or_else(|| {
            ParseError::SchemaError(format!(
                "channel '{channel_name}' lists a server that is not a $ref; `servers` entries \
                 must reference #/servers/..."
            ))
        })?;
        let name = reference.strip_prefix("#/servers/").ok_or_else(|| {
            ParseError::SchemaError(format!(
                "channel '{channel_name}' server $ref '{reference}' must point into #/servers"
            ))
        })?;
        if !servers.contains_key(name) {
            return Err(ParseError::SchemaError(format!(
                "channel '{channel_name}' is declared on server '{name}', which is not defined \
                 under `servers`"
            )));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

/// The servers an operation's channel is reached through.
///
/// `None` means every server, which is what a channel naming none is available
/// on. A channel referenced by name is looked up; one defined inline carries its
/// own `servers` list and is read directly, since it appears under no name.
fn operation_servers(
    op: &serde_json::Map<String, Value>,
    channel_servers: &BTreeMap<String, Vec<String>>,
    declared: &BTreeMap<String, Vec<String>>,
) -> Result<Option<Vec<String>>, ParseError> {
    let Some(channel) = op.get("channel") else {
        return Ok(None);
    };

    if let Some(reference) = channel.get("$ref").and_then(|v| v.as_str()) {
        let Some(name) = reference.strip_prefix("#/channels/") else {
            return Ok(None);
        };
        return Ok(channel_servers.get(name).cloned());
    }

    // Inline channel: its `servers` list narrows it exactly as a named one's
    // does, so ignoring it would inherit credentials from servers the channel
    // cannot be reached through.
    let Some(obj) = channel.as_object() else {
        return Ok(None);
    };
    match obj.get("servers").and_then(|v| v.as_array()) {
        Some(entries) => Ok(Some(channel_server_names("<inline>", entries, declared)?)),
        None => Ok(None),
    }
}

/// The security requirement an operation inherits from the servers it reaches.
///
/// Each scheme is its own alternative, since satisfying one authorizes the
/// connection. `None` when nothing applies, which leaves the operation as
/// unconstrained as it was.
fn server_security_requirement(
    reached_servers: Option<&[String]>,
    server_security: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<SecurityRequirement>> {
    // Nothing declares a credential anywhere, so every operation stays as
    // unconstrained as it was.
    if server_security.values().all(|schemes| schemes.is_empty()) {
        return None;
    }

    // A channel naming servers reaches those; one naming none reaches all.
    let applicable: Vec<&String> = match reached_servers {
        Some(names) => names.iter().collect(),
        None => server_security.keys().collect(),
    };

    let mut schemes: BTreeSet<&String> = BTreeSet::new();
    let mut reachable_without_a_credential = false;
    for server in applicable {
        match server_security.get(server) {
            Some(names) if !names.is_empty() => schemes.extend(names.iter()),
            // Reaching the channel through a server that requires nothing is an
            // alternative of its own, so the operation is not unconditionally
            // authenticated.
            Some(_) => reachable_without_a_credential = true,
            None => {}
        }
    }

    if schemes.is_empty() {
        return None;
    }

    let mut requirements: Vec<SecurityRequirement> = schemes
        .into_iter()
        .map(|name| {
            let mut requirement = SecurityRequirement::new();
            requirement.insert(name.clone(), Vec::new());
            requirement
        })
        .collect();
    if reachable_without_a_credential {
        requirements.push(SecurityRequirement::new());
    }
    Some(requirements)
}

/// Build a lookup map of channel names to their definitions.
fn build_channel_lookup(
    channels: Option<&serde_json::Map<String, Value>>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<BTreeMap<String, ChannelInfo>, ParseError> {
    let mut lookup = BTreeMap::new();

    let channels = match channels {
        Some(c) => c,
        None => return Ok(lookup),
    };

    for (name, channel_value) in channels {
        let channel_obj = match channel_value.as_object() {
            Some(o) => o,
            None => continue,
        };

        // Extract address (defaults to channel name if not specified)
        let address = channel_obj
            .get("address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.clone());

        // Parse messages
        let messages = parse_channel_messages(channel_obj, spec_root, defs)?;

        // Parse parameters
        let parameters = parse_channel_parameters(channel_obj, spec_root, defs)?;

        // Parse bindings
        let bindings = channel_obj
            .get("bindings")
            .and_then(|v| v.as_object())
            .map(|b| {
                b.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        lookup.insert(name.clone(), (address, messages, parameters, bindings));
    }

    Ok(lookup)
}

/// Parse messages from a channel definition.
fn parse_channel_messages(
    channel: &serde_json::Map<String, Value>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<Vec<Message>, ParseError> {
    let messages_obj = match channel.get("messages").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };

    let mut messages = Vec::with_capacity(messages_obj.len());
    for (name, msg_value) in messages_obj {
        let Some(msg_obj) = msg_value.as_object() else {
            continue;
        };

        let payload = msg_obj
            .get("payload")
            .map(|p| resolve_schema(p, spec_root, defs))
            .transpose()?;

        let content_type = msg_obj
            .get("contentType")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let bindings = msg_obj
            .get("bindings")
            .and_then(|v| v.as_object())
            .map(|b| {
                b.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        messages.push(Message {
            name: name.clone(),
            payload,
            content_type,
            bindings,
        });
    }
    Ok(messages)
}

/// Parse parameters from a channel definition (for templated addresses).
fn parse_channel_parameters(
    channel: &serde_json::Map<String, Value>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<Vec<Parameter>, ParseError> {
    let params = match channel.get("parameters").and_then(|v| v.as_object()) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    let mut result = Vec::with_capacity(params.len());
    for (name, param_value) in params {
        let raw_schema = param_value
            .as_object()
            .and_then(|o| o.get("schema").cloned());
        let schema = raw_schema
            .map(|s| resolve_schema(&s, spec_root, defs))
            .transpose()?;

        // In AsyncAPI, channel parameters are always required
        result.push(Parameter {
            name: name.clone(),
            location: "path".to_string(),
            required: true,
            schema,
        });
    }
    Ok(result)
}

/// Resolve a channel reference from an operation.
fn resolve_channel_ref(
    op: &serde_json::Map<String, Value>,
    lookup: &BTreeMap<String, ChannelInfo>,
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<ChannelInfo, ParseError> {
    let channel = op
        .get("channel")
        .ok_or_else(|| ParseError::SchemaError("operation missing 'channel' field".into()))?;

    // Channel can be a $ref or inline
    if let Some(channel_obj) = channel.as_object() {
        if let Some(ref_str) = channel_obj.get("$ref").and_then(|v| v.as_str()) {
            // Parse $ref like "#/channels/userSignedUp"
            let channel_name = ref_str.strip_prefix("#/channels/").ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "invalid channel $ref '{}' (expected #/channels/...)",
                    ref_str
                ))
            })?;

            lookup.get(channel_name).cloned().ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "channel '{}' referenced but not defined",
                    channel_name
                ))
            })
        } else {
            // Inline channel definition
            let address = channel_obj
                .get("address")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();

            let messages = parse_channel_messages(channel_obj, spec_root, defs)?;
            let parameters = parse_channel_parameters(channel_obj, spec_root, defs)?;
            let bindings = channel_obj
                .get("bindings")
                .and_then(|v| v.as_object())
                .map(|b| {
                    b.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();

            Ok((address, messages, parameters, bindings))
        }
    } else {
        Err(ParseError::SchemaError(
            "operation 'channel' must be an object (either $ref or inline)".into(),
        ))
    }
}

/// Parse messages from an operation (may reference channel messages via $ref).
fn parse_operation_messages(
    op: &serde_json::Map<String, Value>,
    channel_messages: &[Message],
    spec_root: &Value,
    defs: &mut SchemaDefs,
) -> Result<Vec<Message>, ParseError> {
    // If operation has explicit messages array, use those
    let Some(msgs) = op.get("messages").and_then(|v| v.as_array()) else {
        // Use all channel messages (already resolved)
        return Ok(channel_messages.to_vec());
    };

    let mut result = Vec::with_capacity(msgs.len());
    for msg in msgs {
        let Some(obj) = msg.as_object() else {
            continue;
        };

        if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()) {
            // Reference to channel message
            // Format: "#/channels/channelName/messages/messageName"
            let parts: Vec<&str> = ref_str.split('/').collect();
            if parts.len() >= 5 && parts[3] == "messages" {
                let msg_name = parts[4];
                if let Some(m) = channel_messages.iter().find(|m| m.name == msg_name) {
                    result.push(m.clone());
                }
            }
            continue;
        }

        // Inline message definition
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let payload = obj
            .get("payload")
            .map(|p| resolve_schema(p, spec_root, defs))
            .transpose()?;
        let content_type = obj
            .get("contentType")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let bindings = obj
            .get("bindings")
            .and_then(|v| v.as_object())
            .map(|b| {
                b.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        result.push(Message {
            name,
            payload,
            content_type,
            bindings,
        });
    }
    Ok(result)
}

/// Follow a schema's local `$ref` to the definition the schema carries.
///
/// `root` is the schema that owns `$defs`; `node` is the value being inspected,
/// which may be `root` itself or something nested inside it.
#[cfg(test)]
fn deref_local<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    let Some(reference) = node.get("$ref").and_then(|v| v.as_str()) else {
        return node;
    };
    let name = reference
        .strip_prefix("#/$defs/")
        .unwrap_or_else(|| panic!("reference is not local: {reference}"));
    root.get("$defs")
        .and_then(|d| d.get(name))
        .unwrap_or_else(|| panic!("schema does not carry $defs/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_openapi() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      operationId: getHealth
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.format, SpecFormat::OpenApi);
        assert_eq!(spec.version, "3.1.0");
        assert_eq!(spec.title, "Test API");
        assert_eq!(spec.operations.len(), 1);

        let op = &spec.operations[0];
        assert_eq!(op.path, "/health");
        assert_eq!(op.method, "GET");
        assert_eq!(op.operation_id, Some("getHealth".to_string()));

        let dispatch = op.dispatch.as_ref().unwrap();
        assert_eq!(dispatch.name, "mock");
    }

    #[test]
    fn parse_path_with_parameters() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users/{id}:
    get:
      operationId: getUser
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: integer
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        assert_eq!(op.parameters.len(), 1);

        let param = &op.parameters[0];
        assert_eq!(param.name, "id");
        assert_eq!(param.location, "path");
        assert!(param.required);
    }

    #[test]
    fn parse_global_middlewares() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 100
      window: 60
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.global_middlewares.len(), 1);
        assert_eq!(spec.global_middlewares[0].name, "rate-limit");
    }

    #[test]
    fn parse_operation_middlewares_override() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: global-auth
paths:
  /public:
    get:
      x-barbacane-middlewares: []
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        // Operation has explicit middlewares (empty array = disable all)
        assert!(op.middlewares.is_some());
        assert_eq!(op.middlewares.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn reject_openapi_2() {
        let yaml = r#"
swagger: "2.0"
info:
  title: Old API
  version: "1.0.0"
paths: {}
"#;
        let result = parse_spec(yaml);
        assert!(matches!(result, Err(ParseError::UnknownFormat)));
    }

    #[test]
    fn parse_multiple_methods() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
    post:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        let methods: Vec<&str> = spec
            .operations
            .iter()
            .map(|op| op.method.as_str())
            .collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
    }

    #[test]
    fn extract_barbacane_extensions() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_second: 100
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-middlewares:
        - name: cache
          config:
            ttl: 60
"#;
        let spec = parse_spec(yaml).unwrap();
        assert!(spec.extensions.contains_key("x-barbacane-middlewares"));

        let op = &spec.operations[0];
        assert!(op.extensions.contains_key("x-barbacane-middlewares"));
    }

    #[test]
    fn parse_request_body() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    post:
      operationId: createUser
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required:
                - name
              properties:
                name:
                  type: string
                email:
                  type: string
                  format: email
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        let body = op.request_body.as_ref().expect("should have request body");
        assert!(body.required);
        assert!(body.content.contains_key("application/json"));

        let json_content = &body.content["application/json"];
        let schema = json_content.schema.as_ref().expect("should have schema");
        assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
    }

    #[test]
    fn parse_deprecated_operation() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /old-endpoint:
    get:
      deprecated: true
      x-sunset: "Sat, 31 Dec 2025 23:59:59 GMT"
      x-barbacane-dispatch:
        name: mock
  /new-endpoint:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        // Check deprecated operation
        let old_op = spec
            .operations
            .iter()
            .find(|op| op.path == "/old-endpoint")
            .unwrap();
        assert!(old_op.deprecated);
        assert_eq!(
            old_op.sunset,
            Some("Sat, 31 Dec 2025 23:59:59 GMT".to_string())
        );

        // Check non-deprecated operation
        let new_op = spec
            .operations
            .iter()
            .find(|op| op.path == "/new-endpoint")
            .unwrap();
        assert!(!new_op.deprecated);
        assert!(new_op.sunset.is_none());
    }

    // ==================== AsyncAPI 3.x Tests ====================

    #[test]
    fn parse_minimal_asyncapi() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: User Events API
  version: "1.0.0"
channels:
  userSignedUp:
    address: user/signedup
    messages:
      UserSignedUpMessage:
        payload:
          type: object
          properties:
            userId:
              type: string
operations:
  processUserSignup:
    action: receive
    channel:
      $ref: '#/channels/userSignedUp'
    x-barbacane-dispatch:
      name: kafka
      config:
        topic: user-events
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.format, SpecFormat::AsyncApi);
        assert_eq!(spec.version, "3.0.0");
        assert_eq!(spec.title, "User Events API");
        assert_eq!(spec.operations.len(), 1);

        let op = &spec.operations[0];
        assert_eq!(op.path, "user/signedup");
        assert_eq!(op.method, "RECEIVE");
        assert_eq!(op.operation_id, Some("processUserSignup".to_string()));

        // Check dispatch config
        let dispatch = op.dispatch.as_ref().unwrap();
        assert_eq!(dispatch.name, "kafka");

        // Check messages
        assert_eq!(op.messages.len(), 1);
        assert_eq!(op.messages[0].name, "UserSignedUpMessage");
        assert!(op.messages[0].payload.is_some());
    }

    #[test]
    fn parse_asyncapi_send_operation() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Notification Service
  version: "1.0.0"
channels:
  notifications:
    address: notifications/{userId}
    parameters:
      userId:
        schema:
          type: string
    messages:
      NotificationMessage:
        contentType: application/json
        payload:
          type: object
          required:
            - title
            - body
          properties:
            title:
              type: string
            body:
              type: string
operations:
  sendNotification:
    action: send
    channel:
      $ref: '#/channels/notifications'
    x-barbacane-dispatch:
      name: nats
      config:
        subject: notifications
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        assert_eq!(op.method, "SEND");
        assert_eq!(op.path, "notifications/{userId}");
        assert_eq!(op.operation_id, Some("sendNotification".to_string()));

        // Check channel parameters
        assert_eq!(op.parameters.len(), 1);
        assert_eq!(op.parameters[0].name, "userId");
        assert_eq!(op.parameters[0].location, "path");
        assert!(op.parameters[0].required);

        // SEND operations should have request_body from message payload
        assert!(op.request_body.is_some());
        let body = op.request_body.as_ref().unwrap();
        assert!(body.required);
        assert!(body.content.contains_key("application/json"));

        // Check messages
        assert_eq!(op.messages.len(), 1);
        assert_eq!(
            op.messages[0].content_type,
            Some("application/json".to_string())
        );
    }

    #[test]
    fn parse_asyncapi_with_bindings() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Order Events
  version: "1.0.0"
channels:
  orderCreated:
    address: orders.created
    bindings:
      kafka:
        topic: order-events
        partitions: 10
        replicas: 3
    messages:
      OrderCreatedMessage:
        bindings:
          kafka:
            key:
              type: string
        payload:
          type: object
operations:
  handleOrderCreated:
    action: receive
    channel:
      $ref: '#/channels/orderCreated'
    bindings:
      kafka:
        groupId: order-processor
    x-barbacane-dispatch:
      name: kafka
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        // Check operation-level bindings (merged from channel and operation)
        assert!(op.bindings.contains_key("kafka"));
        let kafka_binding = op.bindings.get("kafka").unwrap();
        // Operation binding should override channel binding
        assert!(kafka_binding.get("groupId").is_some());

        // Check message bindings
        assert!(op.messages[0].bindings.contains_key("kafka"));
    }

    #[test]
    fn parse_asyncapi_inline_channel() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Inline Channel Test
  version: "1.0.0"
operations:
  inlineOp:
    action: receive
    channel:
      address: inline/topic
      messages:
        InlineMessage:
          payload:
            type: string
    x-barbacane-dispatch:
      name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        assert_eq!(op.path, "inline/topic");
        assert_eq!(op.messages.len(), 1);
        assert_eq!(op.messages[0].name, "InlineMessage");
    }

    #[test]
    fn parse_asyncapi_multiple_operations() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Multi-Op API
  version: "1.0.0"
channels:
  events:
    address: events
    messages:
      Event:
        payload:
          type: object
operations:
  publishEvent:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: kafka
  consumeEvent:
    action: receive
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: kafka
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        let send_op = spec
            .operations
            .iter()
            .find(|op| op.method == "SEND")
            .unwrap();
        let recv_op = spec
            .operations
            .iter()
            .find(|op| op.method == "RECEIVE")
            .unwrap();

        assert_eq!(send_op.operation_id, Some("publishEvent".to_string()));
        assert_eq!(recv_op.operation_id, Some("consumeEvent".to_string()));
    }

    #[test]
    fn parse_asyncapi_global_middlewares() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Middleware Test
  version: "1.0.0"
x-barbacane-middlewares:
  - name: auth
    config:
      type: jwt
channels:
  events:
    address: events
    messages:
      Event:
        payload:
          type: object
operations:
  handleEvent:
    action: receive
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: kafka
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.global_middlewares.len(), 1);
        assert_eq!(spec.global_middlewares[0].name, "auth");
    }

    #[test]
    fn parse_asyncapi_3_1() {
        let yaml = r#"
asyncapi: "3.1.0"
info:
  title: User Events API
  version: "1.0.0"
channels:
  userSignedUp:
    address: user/signedup
    messages:
      UserSignedUpMessage:
        payload:
          type: object
          properties:
            userId:
              type: string
operations:
  processUserSignup:
    action: send
    channel:
      $ref: '#/channels/userSignedUp'
    x-barbacane-dispatch:
      name: kafka
      config:
        topic: user-events
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.format, SpecFormat::AsyncApi);
        assert_eq!(spec.version, "3.1.0");
        assert_eq!(spec.operations.len(), 1);
    }

    #[test]
    fn reject_asyncapi_2() {
        let yaml = r#"
asyncapi: "2.6.0"
info:
  title: Old AsyncAPI
  version: "1.0.0"
channels: {}
"#;
        let result = parse_spec(yaml);
        assert!(matches!(result, Err(ParseError::SchemaError(_))));
    }

    // ==================== OpenAPI 3.2 Tests ====================

    #[test]
    fn parse_query_method() {
        let yaml = r#"
openapi: "3.2.0"
info:
  title: Query Method API
  version: "1.0.0"
paths:
  /search:
    query:
      operationId: searchItems
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              properties:
                filter:
                  type: string
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.version, "3.2.0");
        assert_eq!(spec.operations.len(), 1);

        let op = &spec.operations[0];
        assert_eq!(op.path, "/search");
        assert_eq!(op.method, "QUERY");
        assert_eq!(op.operation_id, Some("searchItems".to_string()));
        assert!(op.request_body.is_some());
    }

    #[test]
    fn parse_additional_operations() {
        let yaml = r#"
openapi: "3.2.0"
info:
  title: Custom Methods API
  version: "1.0.0"
paths:
  /cache/{key}:
    get:
      operationId: getCache
      x-barbacane-dispatch:
        name: mock
    additionalOperations:
      purge:
        operationId: purgeCache
        parameters:
          - name: key
            in: path
            required: true
            schema:
              type: string
        x-barbacane-dispatch:
          name: mock
          config:
            status: 204
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        let get_op = spec
            .operations
            .iter()
            .find(|op| op.method == "GET")
            .unwrap();
        assert_eq!(get_op.operation_id, Some("getCache".to_string()));

        let purge_op = spec
            .operations
            .iter()
            .find(|op| op.method == "PURGE")
            .unwrap();
        assert_eq!(purge_op.operation_id, Some("purgeCache".to_string()));
        assert_eq!(purge_op.parameters.len(), 1);
        assert_eq!(purge_op.parameters[0].name, "key");
    }

    #[test]
    fn parse_additional_operations_inherits_path_params() {
        let yaml = r#"
openapi: "3.2.0"
info:
  title: Path Params Inheritance
  version: "1.0.0"
paths:
  /items/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema:
          type: string
    additionalOperations:
      link:
        operationId: linkItem
        x-barbacane-dispatch:
          name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 1);

        let op = &spec.operations[0];
        assert_eq!(op.method, "LINK");
        // Path-level parameters should be inherited
        assert_eq!(op.parameters.len(), 1);
        assert_eq!(op.parameters[0].name, "id");
    }

    #[test]
    fn parse_querystring_parameter() {
        let yaml = r#"
openapi: "3.2.0"
info:
  title: Querystring API
  version: "1.0.0"
paths:
  /search:
    get:
      operationId: search
      parameters:
        - name: q
          in: querystring
          required: true
          content:
            application/x-www-form-urlencoded:
              schema:
                type: string
                minLength: 1
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        assert_eq!(op.parameters.len(), 1);

        let param = &op.parameters[0];
        assert_eq!(param.name, "q");
        assert_eq!(param.location, "querystring");
        assert!(param.required);
        // Schema should be extracted from content, not top-level schema
        assert!(param.schema.is_some());
        assert_eq!(
            param.schema.as_ref().unwrap().get("type").unwrap(),
            "string"
        );
    }

    // ── $ref resolution tests ────────────────────────────────────────────

    #[test]
    fn resolve_ref_in_parameter_schema() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    UserId:
      type: integer
      format: int64
paths:
  /users/{id}:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema:
            $ref: "#/components/schemas/UserId"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let param = &spec.operations[0].parameters[0];
        let schema = param.schema.as_ref().unwrap();
        // The reference resolves against the document's definition pool, which
        // is where the target is held.
        let resolved = spec.self_contained(schema);
        let target = deref_local(&resolved, schema);
        assert_eq!(target.get("type").unwrap(), "integer");
        assert_eq!(target.get("format").unwrap(), "int64");
    }

    #[test]
    fn resolve_ref_to_components_parameters() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  parameters:
    TenantHeader:
      name: X-Tenant-Id
      in: header
      required: true
      schema:
        type: string
    TraceCookie:
      name: trace
      in: cookie
      schema:
        type: string
paths:
  /orders:
    parameters:
      - $ref: "#/components/parameters/TraceCookie"
    get:
      parameters:
        - $ref: "#/components/parameters/TenantHeader"
        - name: limit
          in: query
          schema:
            type: integer
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let params = &spec.operations[0].parameters;
        assert_eq!(params.len(), 3, "path-item and operation refs both resolve");

        let cookie = params.iter().find(|p| p.name == "trace").expect("cookie");
        assert_eq!(cookie.location, "cookie");

        let tenant = params
            .iter()
            .find(|p| p.name == "X-Tenant-Id")
            .expect("header parameter resolved from components");
        assert_eq!(tenant.location, "header");
        assert!(tenant.required);
        assert_eq!(
            tenant.schema.as_ref().unwrap().get("type").unwrap(),
            "string"
        );
    }

    #[test]
    fn resolve_chained_ref_to_components_parameters() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  parameters:
    Canonical:
      name: X-Tenant-Id
      in: header
      schema:
        type: string
    Alias:
      $ref: "#/components/parameters/Canonical"
paths:
  /orders:
    get:
      parameters:
        - $ref: "#/components/parameters/Alias"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let params = &spec.operations[0].parameters;
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "X-Tenant-Id");
        assert_eq!(params[0].location, "header");
    }

    #[test]
    fn operation_parameter_overrides_the_path_item_one() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /orders/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema:
          type: string
      - name: limit
        in: query
        required: true
        schema:
          type: string
      - name: X-Tenant-Id
        in: header
        schema:
          type: string
          maxLength: 3
    get:
      parameters:
        - name: limit
          in: query
          required: false
          schema:
            type: integer
        - name: x-tenant-id
          in: header
          schema:
            type: string
            maxLength: 64
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let params = &spec.operations[0].parameters;

        // One entry per (name, location): the path item's `limit` is replaced,
        // not kept alongside the operation's.
        let limits: Vec<_> = params.iter().filter(|p| p.name == "limit").collect();
        assert_eq!(limits.len(), 1, "operation parameter must replace, not add");
        assert!(!limits[0].required, "the operation definition wins");
        assert_eq!(
            limits[0].schema.as_ref().unwrap().get("type").unwrap(),
            "integer"
        );

        // Header names are case-insensitive, so this is the same parameter.
        let tenants: Vec<_> = params
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case("x-tenant-id"))
            .collect();
        assert_eq!(tenants.len(), 1, "header override is case-insensitive");
        assert_eq!(
            tenants[0]
                .schema
                .as_ref()
                .unwrap()
                .get("maxLength")
                .unwrap(),
            64
        );

        // A path-item parameter the operation does not redefine is inherited.
        let ids: Vec<_> = params.iter().filter(|p| p.name == "id").collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].location, "path");

        assert_eq!(params.len(), 3);
    }

    #[test]
    fn unresolvable_parameter_ref_is_an_error() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /orders:
    get:
      parameters:
        - $ref: "#/components/parameters/Missing"
      x-barbacane-dispatch:
        name: mock
"##;
        let err = parse_spec(yaml).expect_err("a dangling parameter ref must not be ignored");
        assert!(
            matches!(err, ParseError::UnresolvedRef(ref r) if r.contains("Missing")),
            "expected UnresolvedRef, got: {err:?}"
        );
    }

    #[test]
    fn circular_parameter_ref_is_an_error() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  parameters:
    Loop:
      $ref: "#/components/parameters/Loop"
paths:
  /orders:
    get:
      parameters:
        - $ref: "#/components/parameters/Loop"
      x-barbacane-dispatch:
        name: mock
"##;
        let err = parse_spec(yaml).expect_err("a circular parameter ref must not hang");
        assert!(
            matches!(err, ParseError::SchemaError(ref m) if m.contains("circular")),
            "expected a circular-ref error, got: {err:?}"
        );
    }

    #[test]
    fn parses_every_security_scheme_type() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  securitySchemes:
    KeyHeader:
      type: apiKey
      name: X-API-Key
      in: header
    KeyCookie:
      type: apiKey
      name: session
      in: cookie
    Basic:
      type: http
      scheme: Basic
    Bearer:
      type: http
      scheme: bearer
      bearerFormat: JWT
    Oidc:
      type: openIdConnect
      openIdConnectUrl: https://idp.example.com/.well-known/openid-configuration
    Flows:
      type: oauth2
      flows: {}
    Mtls:
      type: mutualTLS
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let s = &spec.security_schemes;
        assert_eq!(s.len(), 7);
        assert_eq!(
            s["KeyHeader"],
            SecurityScheme::ApiKey {
                name: "X-API-Key".to_string(),
                location: "header".to_string(),
            }
        );
        assert_eq!(
            s["KeyCookie"],
            SecurityScheme::ApiKey {
                name: "session".to_string(),
                location: "cookie".to_string(),
            }
        );
        // `scheme` is matched case-insensitively, so it is stored lowercased.
        assert_eq!(
            s["Basic"],
            SecurityScheme::Http {
                scheme: "basic".to_string()
            }
        );
        assert_eq!(
            s["Bearer"],
            SecurityScheme::Http {
                scheme: "bearer".to_string()
            }
        );
        assert_eq!(s["Oidc"], SecurityScheme::OpenIdConnect);
        assert_eq!(s["Flows"], SecurityScheme::OAuth2);
        assert_eq!(s["Mtls"], SecurityScheme::MutualTls);
    }

    /// AsyncAPI has its own security scheme vocabulary. A document using it
    /// must parse whole: an unknown type fails the entire spec, not the scheme.
    #[test]
    fn parses_every_asyncapi_security_scheme_type() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
components:
  securitySchemes:
    Certs:
      type: X509
    SaslScram:
      type: scramSha256
    SaslScram512:
      type: scramSha512
    Symmetric:
      type: symmetricEncryption
    Asymmetric:
      type: asymmetricEncryption
    Kerberos:
      type: gssapi
    SaslPlain:
      type: plain
    UserPassword:
      type: userPassword
    BrokerUser:
      type: apiKey
      in: user
    BrokerPassword:
      type: apiKey
      in: password
    KeyHeader:
      type: httpApiKey
      name: X-API-Key
      in: header
    Bearer:
      type: http
      scheme: bearer
channels:
  events:
    address: /events
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let s = &spec.security_schemes;
        assert_eq!(s.len(), 12);

        for (name, kind) in [
            ("Certs", "X509"),
            ("SaslScram", "scramSha256"),
            ("SaslScram512", "scramSha512"),
            ("Symmetric", "symmetricEncryption"),
            ("Asymmetric", "asymmetricEncryption"),
            ("Kerberos", "gssapi"),
            ("SaslPlain", "plain"),
            ("UserPassword", "userPassword"),
            // AsyncAPI's `apiKey` places the credential in the connection's
            // user or password field and carries no `name`.
            ("BrokerUser", "apiKey"),
            ("BrokerPassword", "apiKey"),
        ] {
            assert_eq!(
                s[name],
                SecurityScheme::Transport {
                    kind: kind.to_string()
                },
                "{name} is a transport credential"
            );
        }

        // `httpApiKey` is AsyncAPI's spelling of a request parameter.
        assert_eq!(
            s["KeyHeader"],
            SecurityScheme::ApiKey {
                name: "X-API-Key".to_string(),
                location: "header".to_string(),
            }
        );
        assert_eq!(
            s["Bearer"],
            SecurityScheme::Http {
                scheme: "bearer".to_string()
            }
        );
    }

    /// AsyncAPI declares the credential on the server, so an operation inherits
    /// it from the servers its channel is reached through. Without this the
    /// operation reads as anonymous and the credential header is dropped at
    /// ingress, though the document declares it.
    #[test]
    fn asyncapi_operations_inherit_their_server_security() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  gateway:
    host: broker.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/apiKeyHeader'
channels:
  events:
    address: /events
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    apiKeyHeader:
      type: httpApiKey
      name: X-API-Key
      in: header
"##;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        let requirement = op.security.as_ref().expect("the server requires a key");
        assert_eq!(requirement.len(), 1);
        assert!(
            requirement[0].contains_key("apiKeyHeader"),
            "{requirement:?}"
        );
    }

    /// A channel may narrow itself to particular servers, and then only those
    /// servers' credentials apply to operations on it.
    #[test]
    fn a_channel_inherits_only_from_the_servers_it_names() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  public:
    host: public.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/PublicKey'
  internal:
    host: internal.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/InternalKey'
channels:
  restricted:
    address: /restricted
    servers:
      - $ref: '#/servers/internal'
  open:
    address: /open
operations:
  onRestricted:
    action: send
    channel:
      $ref: '#/channels/restricted'
    x-barbacane-dispatch:
      name: mock
  onOpen:
    action: send
    channel:
      $ref: '#/channels/open'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    PublicKey:
      type: httpApiKey
      name: X-Public-Key
      in: header
    InternalKey:
      type: httpApiKey
      name: X-Internal-Key
      in: header
"##;
        let spec = parse_spec(yaml).unwrap();

        let restricted = spec
            .operations
            .iter()
            .find(|o| o.path == "/restricted")
            .expect("restricted");
        let names: Vec<&String> = restricted
            .security
            .as_ref()
            .expect("required")
            .iter()
            .flat_map(|r| r.keys())
            .collect();
        assert_eq!(names, vec!["InternalKey"], "only the server it names");

        // A channel naming no servers is available on every one, so every
        // server's credential applies as an alternative.
        let open = spec
            .operations
            .iter()
            .find(|o| o.path == "/open")
            .expect("open");
        let mut names: Vec<&String> = open
            .security
            .as_ref()
            .expect("required")
            .iter()
            .flat_map(|r| r.keys())
            .collect();
        names.sort();
        assert_eq!(names, vec!["InternalKey", "PublicKey"]);
    }

    /// Each scheme is its own alternative: satisfying one authorizes the
    /// connection, which is AsyncAPI's rule for a server's security list.
    #[test]
    fn server_schemes_are_alternatives() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  gateway:
    host: broker.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/KeyA'
      - $ref: '#/components/securitySchemes/KeyB'
channels:
  events:
    address: /events
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    KeyA:
      type: httpApiKey
      name: X-Key-A
      in: header
    KeyB:
      type: httpApiKey
      name: X-Key-B
      in: header
"##;
        let spec = parse_spec(yaml).unwrap();
        let requirement = spec.operations[0].security.as_ref().expect("required");
        assert_eq!(requirement.len(), 2, "two alternatives, not one AND");
        assert!(requirement.iter().all(|r| r.len() == 1));
    }

    /// A server may declare a scheme inline rather than referencing one. It has
    /// no name of its own, so it gets one: the allowlist is keyed on names and
    /// an unnamed scheme would silently contribute nothing.
    #[test]
    fn an_inline_server_scheme_is_registered() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  gateway:
    host: broker.example.com
    protocol: ws
    security:
      - type: httpApiKey
        name: X-Inline-Key
        in: header
channels:
  events:
    address: /events
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let requirement = spec.operations[0].security.as_ref().expect("required");
        let name = requirement[0].keys().next().expect("a name");
        assert_eq!(
            spec.security_schemes.get(name),
            Some(&SecurityScheme::ApiKey {
                name: "X-Inline-Key".to_string(),
                location: "header".to_string(),
            }),
            "the inline scheme is registered under its synthesized name"
        );
    }

    /// A server with no security leaves its operations as they were, rather
    /// than declaring them anonymous.
    #[test]
    fn a_server_without_security_requires_nothing() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  gateway:
    host: broker.example.com
    protocol: ws
channels:
  events:
    address: /events
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        assert!(spec.operations[0].security.is_none());
    }

    /// A requirement naming a scheme the document does not define resolves to
    /// nothing, so it is refused rather than silently admitting no header.
    #[test]
    fn a_server_referencing_an_undefined_scheme_is_an_error() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  gateway:
    host: broker.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/Nowhere'
channels:
  events:
    address: /events
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
"##;
        let err = parse_spec(yaml).expect_err("the scheme is not defined");
        assert!(
            matches!(err, ParseError::SchemaError(ref m) if m.contains("Nowhere")),
            "{err:?}"
        );
    }

    /// A channel reachable through a secured server and an unsecured one can be
    /// reached without a credential, so anonymous stays an alternative. Dropping
    /// it would describe the operation as authenticated on every route to it.
    #[test]
    fn a_channel_on_a_secured_and_an_unsecured_server_keeps_the_anonymous_route() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  secured:
    host: secure.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/Key'
  open:
    host: open.example.com
    protocol: ws
channels:
  events:
    address: /events
  securedOnly:
    address: /secured
    servers:
      - $ref: '#/servers/secured'
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
  publishSecured:
    action: send
    channel:
      $ref: '#/channels/securedOnly'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    Key:
      type: httpApiKey
      name: X-Key
      in: header
"##;
        let spec = parse_spec(yaml).unwrap();

        let open = spec
            .operations
            .iter()
            .find(|o| o.path == "/events")
            .expect("events");
        let requirement = open.security.as_ref().expect("the secured server applies");
        assert_eq!(requirement.len(), 2, "the key, or nothing: {requirement:?}");
        assert!(
            requirement.iter().any(|r| r.is_empty()),
            "an unsecured server is an anonymous alternative: {requirement:?}"
        );
        assert!(requirement.iter().any(|r| r.contains_key("Key")));

        // A channel narrowed to the secured server has no anonymous route.
        let secured = spec
            .operations
            .iter()
            .find(|o| o.path == "/secured")
            .expect("secured");
        let requirement = secured.security.as_ref().expect("required");
        assert_eq!(requirement.len(), 1, "{requirement:?}");
        assert!(requirement[0].contains_key("Key"));
    }

    /// A channel narrowed to a server that is not defined would leave the
    /// operation requiring nothing, dropping the credential the document
    /// declares. A typo must not do that quietly.
    #[test]
    fn a_channel_on_an_undefined_server_is_an_error() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Broker API
  version: "1.0.0"
servers:
  internal:
    host: internal.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/Key'
channels:
  events:
    address: /events
    servers:
      - $ref: '#/servers/interal'
operations:
  publish:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    Key:
      type: httpApiKey
      name: X-Key
      in: header
"##;
        let err = parse_spec(yaml).expect_err("the server is not defined");
        assert!(
            matches!(err, ParseError::SchemaError(ref m) if m.contains("interal")),
            "{err:?}"
        );
    }

    /// A channel defined inline on the operation carries its own `servers` list.
    /// Ignoring it inherits credentials from servers the channel cannot be
    /// reached through, admitting a header the document does not offer there.
    #[test]
    fn an_inline_channel_honours_its_own_servers_list() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Inline
  version: "1.0.0"
servers:
  gateway:
    host: a.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/GatewayKey'
  internal:
    host: b.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/InternalKey'
operations:
  publish:
    action: send
    channel:
      address: /events
      servers:
        - $ref: '#/servers/gateway'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    GatewayKey:
      type: httpApiKey
      name: X-Gateway-Key
      in: header
    InternalKey:
      type: httpApiKey
      name: X-Internal-Key
      in: header
"##;
        let spec = parse_spec(yaml).unwrap();
        let names: Vec<&String> = spec.operations[0]
            .security
            .as_ref()
            .expect("the gateway server requires a key")
            .iter()
            .flat_map(|r| r.keys())
            .collect();
        assert_eq!(names, vec!["GatewayKey"], "only the server it names");
    }

    /// The same validation applies inline: a reference to a server that is not
    /// defined is refused rather than silently widening the channel.
    #[test]
    fn an_inline_channel_on_an_undefined_server_is_an_error() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Inline
  version: "1.0.0"
servers:
  gateway:
    host: a.example.com
    protocol: ws
    security:
      - $ref: '#/components/securitySchemes/GatewayKey'
operations:
  publish:
    action: send
    channel:
      address: /events
      servers:
        - $ref: '#/servers/getway'
    x-barbacane-dispatch:
      name: mock
components:
  securitySchemes:
    GatewayKey:
      type: httpApiKey
      name: X-Gateway-Key
      in: header
"##;
        let err = parse_spec(yaml).expect_err("the server is not defined");
        assert!(
            matches!(err, ParseError::SchemaError(ref m) if m.contains("getway")),
            "{err:?}"
        );
    }

    #[test]
    fn security_scheme_ref_resolves() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  securitySchemes:
    Alias:
      $ref: "#/components/securitySchemes/Canonical"
    Canonical:
      type: apiKey
      name: X-API-Key
      in: header
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(
            spec.security_schemes["Alias"],
            spec.security_schemes["Canonical"]
        );
    }

    #[test]
    fn malformed_security_schemes_are_errors() {
        let head = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  securitySchemes:
"##;
        let tail = r##"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"##;
        // apiKey without `name`, apiKey without `in`, http without `scheme`,
        // and a type OpenAPI does not define.
        for (scheme, needle) in [
            (
                "    Bad:\n      type: apiKey\n      in: header\n",
                "no 'name'",
            ),
            (
                "    Bad:\n      type: apiKey\n      name: X-Key\n",
                "no 'in'",
            ),
            ("    Bad:\n      type: http\n", "no 'scheme'"),
            ("    Bad:\n      type: magic\n", "unknown type"),
            ("    Bad:\n      name: X-Key\n", "no 'type'"),
        ] {
            let err = parse_spec(&format!("{head}{scheme}{tail}"))
                .expect_err("malformed security scheme must not be accepted");
            assert!(
                err.to_string().contains(needle),
                "expected {needle:?} in: {err}"
            );
        }
    }

    #[test]
    fn security_requirements_distinguish_absent_from_empty() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
security:
  - Bearer: []
components:
  securitySchemes:
    Bearer:
      type: http
      scheme: bearer
paths:
  /inherits:
    get:
      x-barbacane-dispatch:
        name: mock
  /anonymous:
    get:
      security: []
      x-barbacane-dispatch:
        name: mock
  /scoped:
    get:
      security:
        - Bearer: [read, write]
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let root = spec.security.as_ref().expect("root security");
        assert_eq!(root.len(), 1);
        assert_eq!(root[0]["Bearer"], Vec::<String>::new());

        let op = |path: &str| {
            spec.operations
                .iter()
                .find(|o| o.path == path)
                .unwrap_or_else(|| panic!("{path} missing"))
                .security
                .clone()
        };

        // Absent stays None so the root requirement applies.
        assert!(op("/inherits").is_none());
        // `security: []` is present and empty: the operation is anonymous.
        assert_eq!(op("/anonymous"), Some(vec![]));
        // Scopes are kept.
        let scoped = op("/scoped").expect("operation security");
        assert_eq!(scoped[0]["Bearer"], vec!["read", "write"]);
    }

    #[test]
    fn resolve_ref_in_request_body() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    CreateUser:
      type: object
      required: [name]
      properties:
        name:
          type: string
paths:
  /users:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateUser"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let body = spec.operations[0].request_body.as_ref().unwrap();
        let schema = body.content["application/json"].schema.as_ref().unwrap();
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let target = deref_local(schema, schema);
        assert_eq!(target.get("type").unwrap(), "object");
        assert!(target.get("properties").is_some());
    }

    #[test]
    fn resolve_nested_ref() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Address:
      type: object
      properties:
        street:
          type: string
    User:
      type: object
      properties:
        address:
          $ref: "#/components/schemas/Address"
paths:
  /users:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/User"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let body = spec.operations[0].request_body.as_ref().unwrap();
        let schema = body.content["application/json"].schema.as_ref().unwrap();
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let user = deref_local(schema, schema);
        // The nested reference inside User.properties.address resolves through
        // the same definitions, however deep it sits.
        let address = deref_local(
            schema,
            user.get("properties").unwrap().get("address").unwrap(),
        );
        assert_eq!(address.get("type").unwrap(), "object");
    }

    #[test]
    fn unresolved_ref_returns_error() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    get:
      parameters:
        - name: id
          in: query
          schema:
            $ref: "#/components/schemas/DoesNotExist"
      x-barbacane-dispatch:
        name: mock
"##;
        let err = parse_spec(yaml).unwrap_err();
        assert!(
            matches!(err, ParseError::UnresolvedRef(ref s) if s.contains("DoesNotExist")),
            "expected UnresolvedRef, got: {:?}",
            err
        );
    }

    /// A schema that refers to itself is valid JSON Schema. The cycle is kept as
    /// a definition the schema points at, since inlining it does not terminate.
    #[test]
    fn circular_ref_becomes_a_local_definition() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Node:
      type: object
      properties:
        child:
          $ref: "#/components/schemas/Node"
paths:
  /nodes:
    get:
      parameters:
        - name: root
          in: query
          schema:
            $ref: "#/components/schemas/Node"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("a self-referential schema is valid");
        let schema = spec.operations[0].parameters[0]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let text = serde_json::to_string(&schema).expect("serialize");
        assert!(
            !text.contains("#/components/"),
            "no reference may escape the schema: {text}"
        );
        assert!(text.contains("#/$defs/Node"), "cycle kept as a ref: {text}");
        assert!(schema.get("$defs").is_some(), "definitions travel with it");
    }

    #[test]
    fn asyncapi_message_payload_ref() {
        let yaml = r##"
asyncapi: "3.0.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    UserEvent:
      type: object
      properties:
        userId:
          type: string
channels:
  userSignedUp:
    address: user/signedup
    messages:
      userSignedUp:
        payload:
          $ref: "#/components/schemas/UserEvent"
operations:
  onUserSignedUp:
    action: receive
    channel:
      $ref: "#/channels/userSignedUp"
"##;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        let msg = &op.messages[0];
        let payload = msg.payload.as_ref().unwrap();
        let payload = spec.self_contained(payload);
        let payload = &payload;
        let target = deref_local(payload, payload);
        assert_eq!(target.get("type").unwrap(), "object");
    }

    #[test]
    fn parse_summary_and_description() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test
  version: "1.0.0"
paths:
  /orders:
    post:
      operationId: createOrder
      summary: Create a new order
      description: Creates an order with items and shipping address
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"##;
        let spec = parse_spec(yaml).expect("should parse");
        let op = &spec.operations[0];
        assert_eq!(op.summary.as_deref(), Some("Create a new order"));
        assert_eq!(
            op.description.as_deref(),
            Some("Creates an order with items and shipping address")
        );
    }

    #[test]
    fn parse_summary_and_description_absent() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"##;
        let spec = parse_spec(yaml).expect("should parse");
        let op = &spec.operations[0];
        assert!(op.summary.is_none());
        assert!(op.description.is_none());
    }

    #[test]
    fn parse_responses_with_schema() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test
  version: "1.0.0"
paths:
  /orders:
    post:
      operationId: createOrder
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  order_id:
                    type: string
        "404":
          content:
            application/json:
              schema:
                type: object
                properties:
                  error:
                    type: string
"##;
        let spec = parse_spec(yaml).expect("should parse");
        let op = &spec.operations[0];
        assert_eq!(op.responses.len(), 2);
        assert!(op.responses.contains_key("200"));
        assert!(op.responses.contains_key("404"));
        let resp_200 = &op.responses["200"];
        let schema = resp_200.content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        assert!(schema["properties"]["order_id"].is_object());
    }

    #[test]
    fn parse_responses_with_ref() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test
  version: "1.0.0"
components:
  schemas:
    Order:
      type: object
      properties:
        id:
          type: string
paths:
  /orders:
    post:
      operationId: createOrder
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
      responses:
        "200":
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Order'
"##;
        let spec = parse_spec(yaml).expect("should parse");
        let op = &spec.operations[0];
        let schema = op.responses["200"].content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        // $ref should be resolved inline
        let target = deref_local(schema, schema);
        assert!(target["properties"]["id"].is_object());
    }

    #[test]
    fn parse_responses_empty_when_no_content() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 204
      responses:
        "204":
          description: No content
"##;
        let spec = parse_spec(yaml).expect("should parse");
        let op = &spec.operations[0];
        assert!(op.responses.is_empty());
    }
}

/// Conformance checks against constructs that real-world specs rely on.
///
/// These mirror an assessment run over published documents (GitHub, Box, Asana,
/// Stripe, Petstore), keeping the cases that a spec is likely to contain and
/// that a gateway has to get right, in a form that needs no network.
#[cfg(test)]
mod conformance {
    use super::*;

    /// A schema that refers to itself is ordinary JSON Schema: anything with a
    /// tree shape has one. Stripe's published document contains 38 such schemas
    /// among its first 400, so rejecting them rejects the whole spec.
    ///
    #[test]
    fn recursive_schema_is_accepted() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Comment:
      type: object
      properties:
        body:
          type: string
        replies:
          type: array
          items:
            $ref: "#/components/schemas/Comment"
paths:
  /comments:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Comment"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("a self-referential schema is valid JSON Schema");
        assert_eq!(spec.operations.len(), 1);
        assert!(spec.operations[0].request_body.is_some());
    }

    /// Two schemas that refer to each other are the same problem one step out,
    /// and are what Stripe actually trips on (`file` -> `file_link` -> `file`).
    #[test]
    fn mutually_recursive_schemas_are_accepted() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    File:
      type: object
      properties:
        link:
          $ref: "#/components/schemas/FileLink"
    FileLink:
      type: object
      properties:
        file:
          $ref: "#/components/schemas/File"
paths:
  /files:
    get:
      parameters:
        - name: filter
          in: query
          schema:
            $ref: "#/components/schemas/File"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("mutually recursive schemas are valid");
        assert_eq!(spec.operations.len(), 1);
    }

    /// A recursive schema stays resolvable: the cycle is carried as a `$defs`
    /// entry the schema points at, rather than inlined into itself.
    #[test]
    fn recursive_schema_keeps_the_cycle_as_a_local_ref() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Comment:
      type: object
      properties:
        body:
          type: string
        replies:
          type: array
          items:
            $ref: "#/components/schemas/Comment"
paths:
  /comments:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Comment"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("a self-referential schema is valid JSON Schema");
        let body = spec.operations[0].request_body.as_ref().expect("body");
        let schema = body.content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;

        // Nothing may still point outside this schema: the artifact carries the
        // schema alone, with no document to resolve against.
        let text = serde_json::to_string(schema).expect("serialize");
        assert!(
            !text.contains("#/components/"),
            "no reference may escape the schema: {text}"
        );
        // The cycle survives as a local reference.
        assert!(
            text.contains("#/$defs/"),
            "cycle should become a $defs ref: {text}"
        );
        assert!(
            schema.get("$defs").and_then(|d| d.as_object()).is_some(),
            "the definitions it points at must travel with it"
        );
    }

    /// The data plane builds a `jsonschema::Validator` from exactly this value,
    /// so it has to compile and judge nested data correctly.
    #[test]
    fn recursive_schema_compiles_and_validates_nested_data() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Comment:
      type: object
      required: [body]
      properties:
        body:
          type: string
        replies:
          type: array
          items:
            $ref: "#/components/schemas/Comment"
paths:
  /comments:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Comment"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .clone()
            .expect("schema");
        let schema = spec.self_contained(&schema);

        let validator = jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .expect("the schema must compile");

        // Three levels deep, all valid.
        let good = serde_json::json!({
            "body": "top",
            "replies": [
                {"body": "middle", "replies": [{"body": "leaf"}]}
            ]
        });
        assert!(validator.is_valid(&good), "nested comments should validate");

        // A violation nested inside the recursion must still be caught, which
        // is the point of keeping the reference resolvable.
        let bad = serde_json::json!({
            "body": "top",
            "replies": [
                {"body": "middle", "replies": [{"replies": []}]}
            ]
        });
        assert!(
            !validator.is_valid(&bad),
            "a missing required field three levels down must fail"
        );
    }

    /// A schema may carry its own `$defs`, since OpenAPI 3.1 schemas are full
    /// JSON Schema. Those definitions must survive alongside the ones added for
    /// component references, or the pointers into them dangle.
    #[test]
    fn existing_defs_are_kept() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Order:
      type: object
      $defs:
        Money:
          type: object
          properties:
            amount:
              type: integer
      properties:
        total:
          $ref: "#/$defs/Money"
paths:
  /orders:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Order"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let defs = schema
            .get("$defs")
            .and_then(|d| d.as_object())
            .expect("$defs");
        assert!(
            defs.contains_key("Money"),
            "the schema's own definition must survive: {:?}",
            defs.keys().collect::<Vec<_>>()
        );
        // And it must still compile, which it cannot with a dangling pointer.
        jsonschema::options()
            .build(schema)
            .expect("schema with its own $defs must compile");
    }

    /// A pointer may address a place inside a definition, not just the
    /// definition itself. Hoisting renames the definition, so only the first
    /// token may be rewritten and the rest has to survive untouched.
    #[test]
    fn descendant_pointer_follows_the_renamed_definition() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Order:
      type: object
      $defs:
        Money:
          type: string
      properties:
        outer:
          $ref: "#/$defs/Money"
        line:
          $ref: "#/components/schemas/Line"
    Line:
      type: object
      $defs:
        Money:
          type: object
          properties:
            amount:
              type: integer
      properties:
        paid:
          $ref: "#/$defs/Money/properties/amount"
        total:
          $ref: "#/$defs/Money"
paths:
  /orders:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Order"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let defs = schema
            .get("$defs")
            .and_then(|d| d.as_object())
            .expect("$defs");
        let order = deref_local(schema, schema);
        let order_props = order
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("properties");
        let line = deref_local(schema, &order_props["line"]);
        let line_props = line
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("properties");

        // Both schemas declare a definition called Money, so the second is
        // renamed. Every pointer at it, whole or descendant, has to follow it
        // rather than resolve against the one that kept the name.
        let whole = line_props["total"]["$ref"].as_str().expect("total ref");
        let part = line_props["paid"]["$ref"].as_str().expect("paid ref");
        let named = whole.strip_prefix("#/$defs/").expect("local pointer");
        assert_eq!(
            part,
            format!("#/$defs/{named}/properties/amount"),
            "the descendant pointer must name the same definition as the whole one"
        );

        // It addresses the object definition, not the string one that shares
        // its declared name.
        assert_eq!(defs[named]["type"], "object");
        assert!(
            defs[named]["properties"]["amount"].is_object(),
            "the descendant it names must exist"
        );

        // The other schema's Money is a different definition and still a string.
        let outer = order_props["outer"]["$ref"].as_str().expect("outer ref");
        let outer_named = outer.strip_prefix("#/$defs/").expect("local pointer");
        assert_ne!(outer_named, named, "the two are distinct definitions");
        assert_eq!(defs[outer_named]["type"], "string");
    }

    /// A definition name may contain characters a JSON Pointer escapes, so the
    /// token has to be decoded before it is matched and escaped when emitted.
    #[test]
    fn pointer_escaped_definition_name_resolves() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    Order:
      type: object
      $defs:
        "a/b":
          type: integer
      properties:
        value:
          $ref: "#/$defs/a~1b"
paths:
  /orders:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Order"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let order = deref_local(schema, schema);
        let reference = order["properties"]["value"]["$ref"].as_str().expect("ref");
        let name = reference.strip_prefix("#/$defs/").expect("local pointer");
        // Whatever the escaping, the pointer must name a definition that exists
        // and holds the declared body.
        let decoded = name.replace("~1", "/").replace("~0", "~");
        let defs = schema
            .get("$defs")
            .and_then(|d| d.as_object())
            .expect("$defs");
        assert_eq!(
            defs[&decoded]["type"], "integer",
            "the escaped name must resolve to its definition"
        );
    }

    /// A path item may be a reference, which is how a spec reuses one. Reading
    /// it without resolving finds no methods, so every operation it holds would
    /// be dropped, and the route would simply not exist.
    #[test]
    fn operations_behind_a_path_item_ref_are_kept() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  pathItems:
    BookingResource:
      parameters:
        - name: bookingId
          in: path
          required: true
          schema:
            type: string
      get:
        x-barbacane-dispatch:
          name: mock
      delete:
        x-barbacane-dispatch:
          name: mock
paths:
  /bookings/{bookingId}:
    $ref: "#/components/pathItems/BookingResource"
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        assert_eq!(
            spec.operations.len(),
            3,
            "both referenced operations survive"
        );

        let mut methods: Vec<&str> = spec
            .operations
            .iter()
            .filter(|o| o.path == "/bookings/{bookingId}")
            .map(|o| o.method.as_str())
            .collect();
        methods.sort_unstable();
        assert_eq!(methods, vec!["DELETE", "GET"]);

        // The parameters the referenced item declares reach its operations.
        let get = spec
            .operations
            .iter()
            .find(|o| o.path == "/bookings/{bookingId}" && o.method == "GET")
            .expect("GET");
        assert_eq!(get.parameters.len(), 1);
        assert_eq!(get.parameters[0].name, "bookingId");
        assert_eq!(get.parameters[0].location, "path");
    }

    /// OpenAPI 3.0 spells an exclusive bound as a boolean qualifying `minimum`.
    /// Left alone it makes the schema fail to build, and a schema that fails to
    /// build is not validated at all, so the constraint would vanish silently.
    #[test]
    fn draft4_exclusive_bounds_become_the_2020_form() {
        let yaml = r##"
openapi: "3.0.3"
info:
  title: Test API
  version: "1.0.0"
paths:
  /prices:
    post:
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                amount:
                  type: number
                  minimum: 0
                  exclusiveMinimum: true
                discount:
                  type: number
                  maximum: 100
                  exclusiveMaximum: false
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let props = &schema["properties"];

        // `true` moves the bound onto the exclusive keyword.
        assert_eq!(props["amount"]["exclusiveMinimum"], 0);
        assert!(
            props["amount"].get("minimum").is_none(),
            "the inclusive bound it qualified is gone"
        );
        // `false` is the default, so the bound stays inclusive.
        assert_eq!(props["discount"]["maximum"], 100);
        assert!(props["discount"].get("exclusiveMaximum").is_none());

        // And the result is something the validator can build and enforce.
        let validator = jsonschema::options().build(schema).expect("compiles");
        assert!(
            !validator.is_valid(&serde_json::json!({"amount": 0})),
            "0 is excluded"
        );
        assert!(validator.is_valid(&serde_json::json!({"amount": 0.5})));
        assert!(
            validator.is_valid(&serde_json::json!({"discount": 100})),
            "100 is included"
        );
    }

    /// `enum`, `const`, `default` and the example keywords hold instance data,
    /// not schemas. Rewriting a value there would change what a request is
    /// compared against, so the conversion must not reach into them.
    #[test]
    fn draft4_conversion_leaves_instance_data_alone() {
        let yaml = r##"
openapi: "3.0.3"
info:
  title: Test API
  version: "1.0.0"
paths:
  /rules:
    post:
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                rule:
                  type: object
                  default:
                    minimum: 0
                    exclusiveMinimum: true
                  enum:
                    - minimum: 0
                      exclusiveMinimum: true
                nested:
                  type: object
                  properties:
                    depth:
                      type: integer
                      minimum: 1
                      exclusiveMinimum: true
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let rule = &schema["properties"]["rule"];

        // The default is a value a request may carry, not a constraint.
        assert_eq!(
            rule["default"]["exclusiveMinimum"], true,
            "instance data under `default` must be untouched"
        );
        assert_eq!(rule["default"]["minimum"], 0);

        // The same for an enum entry: it is the value compared against.
        assert_eq!(rule["enum"][0]["exclusiveMinimum"], true);
        assert_eq!(rule["enum"][0]["minimum"], 0);

        // A real schema nested below is still converted.
        let depth = &schema["properties"]["nested"]["properties"]["depth"];
        assert_eq!(depth["exclusiveMinimum"], 1);
        assert!(depth.get("minimum").is_none());
    }

    /// A 3.1 document already carries the numeric form, which must be left as it
    /// is: a number there is the bound, not a flag.
    #[test]
    fn numeric_exclusive_bounds_are_untouched() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /prices:
    post:
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                amount:
                  type: number
                  exclusiveMinimum: 5
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).expect("parse");
        let schema = spec.operations[0].request_body.as_ref().unwrap().content["application/json"]
            .schema
            .as_ref()
            .expect("schema");
        let schema = spec.self_contained(schema);
        let schema = &schema;
        assert_eq!(schema["properties"]["amount"]["exclusiveMinimum"], 5);
    }

    /// A schema is self-contained: every reference points at a definition it
    /// carries, so nothing has to be resolved against the document it came from.
    #[test]
    fn schema_is_self_contained() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  schemas:
    UserId:
      type: integer
      format: int64
paths:
  /users/{id}:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema:
            $ref: "#/components/schemas/UserId"
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        let schema = spec.operations[0].parameters[0].schema.as_ref().unwrap();
        let schema = spec.self_contained(schema);
        let schema = &schema;
        let text = serde_json::to_string(schema).expect("serialize");
        assert!(
            !text.contains("#/components/"),
            "no reference may escape the schema: {text}"
        );
        let target = deref_local(schema, schema);
        assert_eq!(target.get("type").unwrap(), "integer");
        assert_eq!(target.get("format").unwrap(), "int64");
    }

    /// Every operation is extracted once, with its parameters attributed to the
    /// right location. Drift here means routes go missing or a value stops
    /// being validated, which is what the corpus run checks at scale.
    #[test]
    fn extraction_is_exact_across_methods_and_levels() {
        let yaml = r##"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /orders/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema:
          type: string
      - name: X-Trace
        in: header
        schema:
          type: string
    get:
      parameters:
        - name: expand
          in: query
          schema:
            type: string
      x-barbacane-dispatch:
        name: mock
    delete:
      x-barbacane-dispatch:
        name: mock
  /orders:
    post:
      parameters:
        - name: session
          in: cookie
          schema:
            type: string
      x-barbacane-dispatch:
        name: mock
"##;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 3, "one entry per path and method");

        let by = |path: &str, method: &str| {
            spec.operations
                .iter()
                .find(|o| o.path == path && o.method == method)
                .unwrap_or_else(|| panic!("{method} {path} missing"))
        };

        // Path-item parameters reach every operation under that path.
        let get = by("/orders/{id}", "GET");
        let loc = |o: &Operation, l: &str| o.parameters.iter().filter(|p| p.location == l).count();
        assert_eq!(loc(get, "path"), 1);
        assert_eq!(loc(get, "header"), 1);
        assert_eq!(loc(get, "query"), 1);

        // Including one that declares none of its own.
        let del = by("/orders/{id}", "DELETE");
        assert_eq!(del.parameters.len(), 2);

        // A cookie parameter is kept with its location intact, which the
        // header allowlist depends on.
        let post = by("/orders", "POST");
        assert_eq!(loc(post, "cookie"), 1);
    }
}
