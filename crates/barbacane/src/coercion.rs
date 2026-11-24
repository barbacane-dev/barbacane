//! The JSON values a request's text becomes before its schema checks it.
//!
//! Parameters, cookies and form fields arrive as text. Each is given the type
//! its schema names (`"5"` is `5` under `type: integer`), so a schema checks
//! the value the client meant. Text that does not read as any type the schema
//! allows stays a string, and the schema then reports it.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

/// How deep `$ref` chains and nested schemas are followed.
const MAX_DEPTH: u8 = 16;

/// The definitions a schema's `#/$defs/...` pointers resolve against: its own,
/// then the artifact's shared pool.
#[derive(Clone, Copy)]
struct Defs<'a> {
    own: Option<&'a Map<String, Value>>,
    pool: Option<&'a Map<String, Value>>,
}

impl<'a> Defs<'a> {
    fn of(schema: &'a Value, pool: Option<&'a Value>) -> Self {
        Self {
            own: schema.get("$defs").and_then(Value::as_object),
            pool: pool.and_then(Value::as_object),
        }
    }

    /// The schema a `$ref` chain ends at; a pointer that is not a definition
    /// of either map is left as it is.
    fn resolve(&self, schema: &'a Value) -> &'a Value {
        let mut current = schema;
        for _ in 0..MAX_DEPTH {
            let Some(name) = current
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|p| p.strip_prefix("#/$defs/"))
            else {
                return current;
            };
            let name = name.replace("~1", "/").replace("~0", "~");
            match self
                .own
                .and_then(|d| d.get(&name))
                .or_else(|| self.pool.and_then(|d| d.get(&name)))
            {
                Some(next) => current = next,
                None => return current,
            }
        }
        current
    }

    /// The `type` names a schema allows, looking through `anyOf`, `oneOf` and
    /// `allOf` when it has no `type` of its own.
    fn types(&self, schema: &'a Value, depth: u8, out: &mut Vec<&'a str>) {
        if depth >= MAX_DEPTH {
            return;
        }
        let schema = self.resolve(schema);
        match schema.get("type") {
            Some(Value::String(t)) => out.push(t),
            Some(Value::Array(ts)) => out.extend(ts.iter().filter_map(Value::as_str)),
            _ => {
                for key in ["anyOf", "oneOf", "allOf"] {
                    for member in schema
                        .get(key)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        self.types(member, depth + 1, out);
                    }
                }
            }
        }
    }

    /// The `properties` of an object schema, its `allOf` members' included.
    fn properties(&self, schema: &'a Value, depth: u8, out: &mut Vec<(&'a str, &'a Value)>) {
        if depth >= MAX_DEPTH {
            return;
        }
        let schema = self.resolve(schema);
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            out.extend(props.iter().map(|(name, s)| (name.as_str(), s)));
        }
        for member in schema
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.properties(member, depth + 1, out);
        }
    }
}

/// A JSON type a text value can be read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    Integer,
    Number,
    Boolean,
    Null,
}

/// How a text value becomes the JSON value its schema checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coercion {
    /// Kept as a string: the schema allows strings, or names no type.
    Text,
    /// The first of these types the text reads as, or a string.
    Scalar(Vec<Scalar>),
    /// An array of the values, each read as the items are.
    Array(Box<Coercion>),
}

impl Coercion {
    /// The coercion for values checked by `schema`, whose definitions resolve
    /// in `pool` (the artifact's shared `$defs`) or the schema's own `$defs`.
    pub fn of(schema: &Value, pool: Option<&Value>) -> Self {
        Self::with(schema, Defs::of(schema, pool), 0)
    }

    fn with(schema: &Value, defs: Defs<'_>, depth: u8) -> Self {
        let mut types = Vec::new();
        defs.types(schema, 0, &mut types);
        if types.is_empty() || types.contains(&"string") {
            return Coercion::Text;
        }
        if types.contains(&"array") {
            let items = defs.resolve(schema).get("items");
            return Coercion::Array(Box::new(match items {
                Some(items) if depth < MAX_DEPTH => Self::with(items, defs, depth + 1),
                _ => Coercion::Text,
            }));
        }
        let scalars: Vec<Scalar> = [
            ("integer", Scalar::Integer),
            ("number", Scalar::Number),
            ("boolean", Scalar::Boolean),
            ("null", Scalar::Null),
        ]
        .into_iter()
        .filter(|(name, _)| types.contains(name))
        .map(|(_, scalar)| scalar)
        .collect();
        if scalars.is_empty() {
            Coercion::Text
        } else {
            Coercion::Scalar(scalars)
        }
    }

    /// The JSON value of one text value.
    pub fn coerce(&self, raw: &str) -> Value {
        match self {
            Coercion::Text => Value::String(raw.to_string()),
            Coercion::Scalar(scalars) => scalars
                .iter()
                .find_map(|scalar| read_scalar(*scalar, raw))
                .unwrap_or_else(|| Value::String(raw.to_string())),
            Coercion::Array(items) => Value::Array(vec![items.coerce(raw)]),
        }
    }

    /// The JSON value of a field given `raws.len()` times. A field whose schema
    /// is not an array and that is given more than once is an array of its
    /// strings, which the schema then refuses.
    pub fn coerce_all(&self, raws: &[String]) -> Value {
        match (self, raws) {
            (Coercion::Array(items), _) => {
                Value::Array(raws.iter().map(|raw| items.coerce(raw)).collect())
            }
            (_, [one]) => self.coerce(one),
            _ => Value::Array(raws.iter().cloned().map(Value::String).collect()),
        }
    }
}

fn read_scalar(scalar: Scalar, raw: &str) -> Option<Value> {
    match scalar {
        Scalar::Integer => raw
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| raw.parse::<u64>().map(Value::from))
            .ok(),
        Scalar::Number => raw.parse::<i64>().map(Value::from).ok().or_else(|| {
            raw.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
        }),
        Scalar::Boolean => match raw {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        Scalar::Null => (raw == "null").then_some(Value::Null),
    }
}

/// Whether `schema` describes an object: it names `object` among its types,
/// and neither `string` nor `array`.
pub fn names_object(schema: &Value, pool: Option<&Value>) -> bool {
    let defs = Defs::of(schema, pool);
    let mut types = Vec::new();
    defs.types(schema, 0, &mut types);
    types.contains(&"object") && !types.contains(&"string") && !types.contains(&"array")
}

/// The coercion of each property of an object schema, for form fields.
pub fn form_fields(schema: &Value, pool: Option<&Value>) -> HashMap<String, Coercion> {
    let defs = Defs::of(schema, pool);
    let mut properties = Vec::new();
    defs.properties(schema, 0, &mut properties);
    properties
        .into_iter()
        .map(|(name, s)| (name.to_string(), Coercion::with(s, defs, 0)))
        .collect()
}

/// The object a form's fields make, each field coerced by its property's
/// schema. A field the schema does not name stays text.
pub fn form_object(pairs: Vec<(String, String)>, fields: &HashMap<String, Coercion>) -> Value {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in pairs {
        grouped.entry(name).or_default().push(value);
    }
    Value::Object(
        grouped
            .into_iter()
            .map(|(name, values)| {
                let value = fields
                    .get(&name)
                    .unwrap_or(&Coercion::Text)
                    .coerce_all(&values);
                (name, value)
            })
            .collect(),
    )
}

/// The fields of an `application/x-www-form-urlencoded` body.
pub fn urlencoded_pairs(body: &[u8]) -> Vec<(String, String)> {
    form_urlencoded::parse(body).into_owned().collect()
}

/// The fields of a `multipart/form-data` body, each part's content read as
/// UTF-8 (bytes that are not are replaced). `content_type` carries the
/// boundary.
pub fn multipart_pairs(content_type: &str, body: &[u8]) -> Result<Vec<(String, String)>, String> {
    use futures_util::FutureExt;

    let boundary = multer::parse_boundary(content_type).map_err(|e| e.to_string())?;
    let data = bytes::Bytes::copy_from_slice(body);
    let stream = futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(data) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    async move {
        let mut fields = Vec::new();
        while let Some(field) = multipart.next_field().await.map_err(|e| e.to_string())? {
            let name = field
                .name()
                .map(str::to_string)
                .ok_or_else(|| "a part has no field name".to_string())?;
            let content = field.bytes().await.map_err(|e| e.to_string())?;
            fields.push((name, String::from_utf8_lossy(&content).into_owned()));
        }
        Ok(fields)
    }
    // The body is in memory, so parsing never waits.
    .now_or_never()
    .unwrap_or_else(|| Err("the multipart body is incomplete".to_string()))
}

/// The name-value pairs of a `Cookie` header (RFC 6265 §5.4): pairs split on
/// `;`, names and values trimmed, and a value's enclosing double quotes
/// removed. A pair without `=` is skipped.
pub fn cookie_pairs(header: &str) -> Vec<(String, String)> {
    header
        .split(';')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            Some((name.trim().to_string(), value.to_string()))
        })
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_value_takes_the_type_its_schema_names() {
        let cases = [
            (json!({"type": "integer"}), "10", json!(10)),
            (json!({"type": "integer"}), "ten", json!("ten")),
            (json!({"type": "integer"}), "1.5", json!("1.5")),
            (json!({"type": "number"}), "1.5", json!(1.5)),
            (json!({"type": "number"}), "NaN", json!("NaN")),
            (json!({"type": "boolean"}), "true", json!(true)),
            (json!({"type": "boolean"}), "yes", json!("yes")),
            (json!({"type": ["integer", "null"]}), "null", json!(null)),
            (json!({"type": ["integer", "string"]}), "10", json!("10")),
            (
                json!({"type": "array", "items": {"type": "integer"}}),
                "3",
                json!([3]),
            ),
            (
                json!({"anyOf": [{"type": "integer"}, {"type": "boolean"}]}),
                "false",
                json!(false),
            ),
            (json!({"enum": ["a", "b"]}), "a", json!("a")),
        ];
        for (schema, raw, expected) in cases {
            assert_eq!(
                Coercion::of(&schema, None).coerce(raw),
                expected,
                "{schema} {raw}"
            );
        }
    }

    #[test]
    fn references_resolve_in_the_pool_and_the_schema() {
        let pool = json!({"Count": {"$ref": "#/$defs/Int"}, "Int": {"type": "integer"}});
        let schema = json!({"$ref": "#/$defs/Count"});
        assert_eq!(Coercion::of(&schema, Some(&pool)).coerce("4"), json!(4));

        let own = json!({"$ref": "#/$defs/Flag", "$defs": {"Flag": {"type": "boolean"}}});
        assert_eq!(Coercion::of(&own, None).coerce("true"), json!(true));
    }

    #[test]
    fn a_repeated_field_is_an_array_only_where_the_schema_allows_one() {
        let schema = json!({
            "allOf": [{"$ref": "#/$defs/Base"}],
            "properties": {"tags": {"type": "array", "items": {"type": "string"}}}
        });
        let pool = json!({"Base": {"properties": {"age": {"type": "integer"}}}});
        let fields = form_fields(&schema, Some(&pool));
        let pairs = |p: &[(&str, &str)]| {
            p.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            form_object(
                pairs(&[("age", "41"), ("tags", "a"), ("tags", "b"), ("x", "y")]),
                &fields
            ),
            json!({"age": 41, "tags": ["a", "b"], "x": "y"})
        );
        assert_eq!(
            form_object(pairs(&[("age", "41"), ("age", "42")]), &fields),
            json!({"age": ["41", "42"]})
        );
    }

    #[test]
    fn form_bodies_are_read_as_fields() {
        assert_eq!(
            urlencoded_pairs(b"name=Ada+L&note=a%26b"),
            vec![
                ("name".into(), "Ada L".into()),
                ("note".into(), "a&b".into())
            ]
        );
        let body = "--XyZ\r\nContent-Disposition: form-data; name=\"age\"\r\n\r\n41\r\n\
                    --XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\
                    Content-Type: text/plain\r\n\r\nhello\r\n--XyZ--\r\n";
        assert_eq!(
            multipart_pairs("multipart/form-data; boundary=XyZ", body.as_bytes()),
            Ok(vec![
                ("age".into(), "41".into()),
                ("file".into(), "hello".into())
            ])
        );
        assert!(multipart_pairs("multipart/form-data", body.as_bytes()).is_err());
        assert!(multipart_pairs("multipart/form-data; boundary=XyZ", b"--XyZ\r\nbroken").is_err());
    }

    #[test]
    fn cookies_are_read_as_rfc_6265_pairs() {
        assert_eq!(
            cookie_pairs(r#"session=abc; theme="dark" ;flag; =x; n=1=2"#),
            vec![
                ("session".into(), "abc".into()),
                ("theme".into(), "dark".into()),
                ("n".into(), "1=2".into()),
            ]
        );
    }
}
