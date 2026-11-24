//! How a parameter's value is read from the request: its OpenAPI `style`,
//! `explode` and `content`.
//!
//! The text is split the way the parameter's style writes it, and each piece
//! then takes the type its schema names (see [`crate::coercion`]).

use std::collections::HashMap;

use barbacane_compiler::Parameter;
use serde_json::Value;

use crate::coercion::{self, Coercion};

/// OpenAPI's parameter serialization styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Simple,
    Label,
    Matrix,
    Form,
    SpaceDelimited,
    PipeDelimited,
    DeepObject,
}

/// The value a parameter's schema checks.
#[derive(Debug)]
enum Shape {
    /// One value.
    Single(Coercion),
    /// An array, each item read as `items` are.
    Array(Coercion),
    /// An object, each property read as its schema is.
    Object(HashMap<String, Coercion>),
    /// A JSON document (`content: application/json`).
    Json,
}

/// Why a parameter's text cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The parameter is given more than once where its style takes it once.
    Repeated,
    /// The text is not written the way the parameter's style or media type
    /// writes it.
    Malformed(String),
}

/// Reads one parameter's value from the request.
#[derive(Debug)]
pub struct ParamReader {
    style: Style,
    explode: bool,
    shape: Shape,
    /// Header items are trimmed of the whitespace HTTP lists allow.
    trim: bool,
}

impl ParamReader {
    /// The reader for `param`, whose schema's `#/$defs/...` pointers resolve in
    /// `pool` (the artifact's shared definitions).
    pub fn new(param: &Parameter, pool: Option<&Value>) -> Self {
        let style = match param.style.as_deref() {
            Some("label") => Style::Label,
            Some("matrix") => Style::Matrix,
            Some("form") | Some("cookie") => Style::Form,
            Some("spaceDelimited") => Style::SpaceDelimited,
            Some("pipeDelimited") => Style::PipeDelimited,
            Some("deepObject") => Style::DeepObject,
            Some(_) => Style::Simple,
            None => match param.location.as_str() {
                "query" | "cookie" => Style::Form,
                _ => Style::Simple,
            },
        };
        let explode = param.explode.unwrap_or(style == Style::Form);
        let json = param
            .media_type
            .as_deref()
            .is_some_and(|m| m == "application/json" || m.ends_with("+json"));

        let shape = match (&param.schema, param.location.as_str()) {
            _ if json => Shape::Json,
            (Some(schema), "querystring")
                if param.media_type.as_deref() == Some("application/x-www-form-urlencoded")
                    && coercion::names_object(schema, pool) =>
            {
                Shape::Object(coercion::form_fields(schema, pool))
            }
            (_, "querystring") | (None, _) => Shape::Single(Coercion::Text),
            (Some(schema), _) if param.media_type.is_some() => {
                Shape::Single(Coercion::of(schema, pool))
            }
            (Some(schema), _) => match Coercion::of(schema, pool) {
                Coercion::Array(items) => Shape::Array(*items),
                Coercion::Text if coercion::names_object(schema, pool) => {
                    Shape::Object(coercion::form_fields(schema, pool))
                }
                single => Shape::Single(single),
            },
        };

        Self {
            style,
            explode,
            shape,
            trim: param.location == "header",
        }
    }

    /// The value of parameter `name` among a query string's or a `Cookie`
    /// header's decoded pairs, or `None` when the request does not give it.
    pub fn from_pairs(
        &self,
        name: &str,
        pairs: &[(String, String)],
    ) -> Option<Result<Value, ReadError>> {
        let given = |props: Vec<(String, String)>, fields| {
            (!props.is_empty()).then(|| Ok(coercion::form_object(props, fields)))
        };
        match &self.shape {
            // `tag=a&tag=b`
            Shape::Array(items) if self.explode && self.style != Style::DeepObject => {
                let values: Vec<Value> = pairs
                    .iter()
                    .filter(|(key, _)| key == name)
                    .map(|(_, value)| items.coerce(value))
                    .collect();
                (!values.is_empty()).then_some(Ok(Value::Array(values)))
            }
            // `filter[color]=red&filter[size]=L`
            Shape::Object(fields) if self.style == Style::DeepObject => given(
                pairs
                    .iter()
                    .filter_map(|(key, value)| {
                        let property = key
                            .strip_prefix(name)?
                            .strip_prefix('[')?
                            .strip_suffix(']')?;
                        Some((property.to_string(), value.clone()))
                    })
                    .collect(),
                fields,
            ),
            // `color=red&size=L`: each property is a pair of its own.
            Shape::Object(fields) if self.explode => given(
                pairs
                    .iter()
                    .filter(|(key, _)| fields.contains_key(key))
                    .cloned()
                    .collect(),
                fields,
            ),
            _ => {
                let mut values = pairs.iter().filter(|(key, _)| key == name);
                let (_, first) = values.next()?;
                if values.next().is_some() {
                    return Some(Err(ReadError::Repeated));
                }
                Some(self.from_text(name, first))
            }
        }
    }

    /// The value of parameter `name` written as `text`: a path segment, a
    /// header, or one query or cookie value.
    pub fn from_text(&self, name: &str, text: &str) -> Result<Value, ReadError> {
        let malformed = |what: &str| ReadError::Malformed(format!("is not in {what} style"));
        match self.style {
            Style::Label => {
                let text = text
                    .strip_prefix('.')
                    .ok_or_else(|| malformed("label (.value)"))?;
                self.parse(text, if self.explode { '.' } else { ',' })
            }
            Style::Matrix => {
                let text = text
                    .strip_prefix(';')
                    .ok_or_else(|| malformed("matrix (;name=value)"))?;
                self.parse_matrix(name, text)
                    .ok_or_else(|| malformed("matrix (;name=value)"))?
            }
            Style::SpaceDelimited => self.parse(text, ' '),
            Style::PipeDelimited => self.parse(text, '|'),
            Style::Simple | Style::Form | Style::DeepObject => self.parse(text, ','),
        }
    }

    /// A matrix-style value after its leading `;`; `None` when it does not
    /// name the parameter.
    fn parse_matrix(&self, name: &str, text: &str) -> Option<Result<Value, ReadError>> {
        match &self.shape {
            // `;id=3;id=4`
            Shape::Array(items) if self.explode => text
                .split(';')
                .map(|part| {
                    part.strip_prefix(name)?
                        .strip_prefix('=')
                        .map(|value| items.coerce(value))
                })
                .collect::<Option<Vec<_>>>()
                .map(|values| Ok(Value::Array(values))),
            // `;role=admin;name=Alex`
            Shape::Object(fields) if self.explode => text
                .split(';')
                .map(|part| {
                    part.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect::<Option<Vec<_>>>()
                .map(|pairs| Ok(coercion::form_object(pairs, fields))),
            // `;id=3,4`, or `;id` for an empty value.
            _ => match text.strip_prefix(name)? {
                "" => Some(self.parse("", ',')),
                rest => rest.strip_prefix('=').map(|value| self.parse(value, ',')),
            },
        }
    }

    /// `text` read as the shape, its items split on `separator`.
    fn parse(&self, text: &str, separator: char) -> Result<Value, ReadError> {
        match &self.shape {
            Shape::Json => serde_json::from_str(text)
                .map_err(|e| ReadError::Malformed(format!("is not a JSON document: {e}"))),
            Shape::Single(coercion) => Ok(coercion.coerce(text)),
            Shape::Array(items) => Ok(Value::Array(
                self.items(text, separator)
                    .map(|item| items.coerce(item))
                    .collect(),
            )),
            Shape::Object(fields) => {
                let items: Vec<&str> = self.items(text, separator).collect();
                let pairs: Vec<(String, String)> = if self.explode {
                    // `role=admin,name=Alex`
                    items
                        .iter()
                        .map(|item| {
                            item.split_once('=')
                                .map(|(k, v)| (k.to_string(), v.to_string()))
                        })
                        .collect::<Option<_>>()
                        .ok_or_else(|| {
                            ReadError::Malformed("is not a list of name=value pairs".into())
                        })?
                } else {
                    // `role,admin,name,Alex`
                    if !items.len().is_multiple_of(2) {
                        return Err(ReadError::Malformed(
                            "is not a list of name, value pairs".into(),
                        ));
                    }
                    items
                        .chunks(2)
                        .map(|pair| (pair[0].to_string(), pair[1].to_string()))
                        .collect()
                };
                Ok(coercion::form_object(pairs, fields))
            }
        }
    }

    /// The items of a delimited value; an empty value has none.
    fn items<'t>(&self, text: &'t str, separator: char) -> impl Iterator<Item = &'t str> {
        let trim = self.trim;
        (!text.is_empty())
            .then(|| text.split(separator))
            .into_iter()
            .flatten()
            .map(move |item| if trim { item.trim() } else { item })
    }

    /// Reads an OpenAPI 3.2 `querystring` parameter: the whole query string,
    /// as its media type writes it.
    pub fn from_query_string(&self, query: &str) -> Result<Value, ReadError> {
        match &self.shape {
            Shape::Json => self.parse(&crate::validator::percent_decode(query), ','),
            Shape::Object(fields) => Ok(coercion::form_object(
                coercion::urlencoded_pairs(query.as_bytes()),
                fields,
            )),
            _ => Ok(Value::String(query.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reader(
        location: &str,
        style: Option<&str>,
        explode: Option<bool>,
        schema: Value,
    ) -> ParamReader {
        ParamReader::new(
            &Parameter {
                name: "p".into(),
                location: location.into(),
                schema: Some(schema),
                style: style.map(str::to_string),
                explode,
                ..Default::default()
            },
            None,
        )
    }

    fn pairs(query: &str) -> Vec<(String, String)> {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn ints() -> Value {
        json!({"type": "array", "items": {"type": "integer"}})
    }

    fn object() -> Value {
        json!({"type": "object", "properties": {"id": {"type": "integer"}, "role": {"type": "string"}}})
    }

    #[test]
    fn query_arrays_are_read_in_each_style() {
        let cases = [
            (None, None, "p=1&p=2", json!([1, 2])),
            (None, None, "p=1", json!([1])),
            (Some("form"), Some(false), "p=1,2", json!([1, 2])),
            (Some("form"), Some(false), "p=", json!([])),
            (Some("spaceDelimited"), Some(false), "p=1 2", json!([1, 2])),
            (Some("pipeDelimited"), Some(false), "p=1|2", json!([1, 2])),
            (Some("pipeDelimited"), None, "p=1|2", json!([1, 2])),
        ];
        for (style, explode, query, expected) in cases {
            let value = reader("query", style, explode, ints()).from_pairs("p", &pairs(query));
            assert_eq!(value, Some(Ok(expected)), "{style:?} {explode:?} {query}");
        }
    }

    #[test]
    fn query_objects_are_read_in_each_style() {
        let cases = [
            (
                None,
                None,
                "id=3&role=admin&other=x",
                json!({"id": 3, "role": "admin"}),
            ),
            (
                Some("form"),
                Some(false),
                "p=id,3,role,admin",
                json!({"id": 3, "role": "admin"}),
            ),
            (
                Some("deepObject"),
                None,
                "p[id]=3&p[role]=admin&q[id]=4",
                json!({"id": 3, "role": "admin"}),
            ),
        ];
        for (style, explode, query, expected) in cases {
            let value = reader("query", style, explode, object()).from_pairs("p", &pairs(query));
            assert_eq!(value, Some(Ok(expected)), "{style:?} {query}");
        }
        assert_eq!(
            reader("query", None, None, object()).from_pairs("p", &pairs("other=x")),
            None
        );
        assert!(matches!(
            reader("query", Some("form"), Some(false), object())
                .from_pairs("p", &pairs("p=id,3,role")),
            Some(Err(ReadError::Malformed(_)))
        ));
    }

    #[test]
    fn a_single_valued_parameter_given_twice_is_refused() {
        let scalar = reader("query", None, None, json!({"type": "integer"}));
        assert_eq!(
            scalar.from_pairs("p", &pairs("p=1&p=2")),
            Some(Err(ReadError::Repeated))
        );
        let delimited = reader("query", Some("form"), Some(false), ints());
        assert_eq!(
            delimited.from_pairs("p", &pairs("p=1&p=2")),
            Some(Err(ReadError::Repeated))
        );
        assert_eq!(scalar.from_pairs("p", &pairs("q=1")), None);
    }

    #[test]
    fn path_values_are_read_in_each_style() {
        let cases = [
            (None, None, ints(), "1,2", json!([1, 2])),
            (Some("label"), None, ints(), ".1,2", json!([1, 2])),
            (Some("label"), Some(true), ints(), ".1.2", json!([1, 2])),
            (Some("matrix"), None, ints(), ";p=1,2", json!([1, 2])),
            (
                Some("matrix"),
                Some(true),
                ints(),
                ";p=1;p=2",
                json!([1, 2]),
            ),
            (
                Some("matrix"),
                None,
                json!({"type": "integer"}),
                ";p=5",
                json!(5),
            ),
            (
                Some("label"),
                None,
                json!({"type": "integer"}),
                ".5",
                json!(5),
            ),
            (
                None,
                None,
                object(),
                "id,3,role,admin",
                json!({"id": 3, "role": "admin"}),
            ),
            (
                None,
                Some(true),
                object(),
                "id=3,role=admin",
                json!({"id": 3, "role": "admin"}),
            ),
            (
                Some("matrix"),
                Some(true),
                object(),
                ";id=3;role=admin",
                json!({"id": 3, "role": "admin"}),
            ),
        ];
        for (style, explode, schema, text, expected) in cases {
            let value = reader("path", style, explode, schema).from_text("p", text);
            assert_eq!(value, Ok(expected), "{style:?} {explode:?} {text}");
        }
        for (style, text) in [("label", "1,2"), ("matrix", "p=1"), ("matrix", ";q=1")] {
            assert!(
                matches!(
                    reader("path", Some(style), None, ints()).from_text("p", text),
                    Err(ReadError::Malformed(_))
                ),
                "{style} {text}"
            );
        }
    }

    #[test]
    fn header_items_are_trimmed() {
        assert_eq!(
            reader("header", None, None, ints()).from_text("p", "1, 2"),
            Ok(json!([1, 2]))
        );
    }

    #[test]
    fn content_parameters_are_read_as_their_media_type() {
        let json_param = |location: &str| {
            ParamReader::new(
                &Parameter {
                    name: "p".into(),
                    location: location.into(),
                    schema: Some(object()),
                    media_type: Some("application/json".into()),
                    ..Default::default()
                },
                None,
            )
        };
        assert_eq!(
            json_param("query").from_pairs("p", &[("p".into(), r#"{"id":3}"#.into())]),
            Some(Ok(json!({"id": 3})))
        );
        assert!(matches!(
            json_param("header").from_text("p", "{"),
            Err(ReadError::Malformed(_))
        ));
        assert_eq!(
            json_param("querystring").from_query_string("%7B%22id%22%3A3%7D"),
            Ok(json!({"id": 3}))
        );

        let form = ParamReader::new(
            &Parameter {
                name: "q".into(),
                location: "querystring".into(),
                schema: Some(object()),
                media_type: Some("application/x-www-form-urlencoded".into()),
                ..Default::default()
            },
            None,
        );
        assert_eq!(
            form.from_query_string("id=3&role=a+b"),
            Ok(json!({"id": 3, "role": "a b"}))
        );
    }
}
