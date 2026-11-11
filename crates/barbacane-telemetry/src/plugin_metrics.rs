//! Metrics plugins emit through the `telemetry` capability.
//!
//! Each plugin metric is its own family, `barbacane_plugin_<plugin>_<metric>`
//! with the plugin name's `-` written `_`, and the labels the plugin sends are
//! its Prometheus labels. Families appear as plugins first write to them, so a
//! [`Collector`] encodes them at scrape time.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::RwLock;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;

/// A series' labels, sorted by name.
pub(crate) type LabelSet = Vec<(String, String)>;

/// Why a plugin metric write was refused, as the `reason` label of
/// `barbacane_plugin_metrics_dropped` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// The metric name is not `[a-zA-Z_][a-zA-Z0-9_]*`, or the plugin name
    /// is not `[a-z0-9-]+`.
    InvalidName,
    /// The labels are not a JSON object of label names to strings, numbers
    /// or booleans.
    InvalidLabels,
    /// Another plugin's metric maps to the same family name.
    NameConflict,
    /// The metric already exists with the other type.
    TypeConflict,
}

impl Rejection {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Rejection::InvalidName => "invalid_name",
            Rejection::InvalidLabels => "invalid_labels",
            Rejection::NameConflict => "name_conflict",
            Rejection::TypeConflict => "type_conflict",
        }
    }
}

#[derive(Debug)]
enum Kind {
    Counter(Family<LabelSet, Counter>),
    Histogram(Family<LabelSet, Histogram>),
}

#[derive(Debug)]
struct Entry {
    plugin: String,
    metric: String,
    kind: Kind,
}

/// Every plugin metric family, by exported name.
#[derive(Debug, Default)]
pub(crate) struct PluginMetrics {
    families: RwLock<BTreeMap<String, Entry>>,
}

impl PluginMetrics {
    /// The counter for `labels` of `plugin`'s metric `metric`.
    pub(crate) fn counter(
        &self,
        plugin: &str,
        metric: &str,
        labels: &LabelSet,
    ) -> Result<Counter, Rejection> {
        self.series_with(
            plugin,
            metric,
            || Kind::Counter(Family::default()),
            |kind| match kind {
                Kind::Counter(family) => Ok(family.get_or_create(labels).clone()),
                Kind::Histogram(_) => Err(Rejection::TypeConflict),
            },
        )
    }

    /// The histogram for `labels` of `plugin`'s metric `metric`; a new
    /// metric's histograms come from `new_histogram`.
    pub(crate) fn histogram(
        &self,
        plugin: &str,
        metric: &str,
        labels: &LabelSet,
        new_histogram: fn() -> Histogram,
    ) -> Result<Histogram, Rejection> {
        self.series_with(
            plugin,
            metric,
            || Kind::Histogram(Family::new_with_constructor(new_histogram)),
            |kind| match kind {
                Kind::Histogram(family) => Ok(family.get_or_create(labels).clone()),
                Kind::Counter(_) => Err(Rejection::TypeConflict),
            },
        )
    }

    fn series_with<T>(
        &self,
        plugin: &str,
        metric: &str,
        new: impl FnOnce() -> Kind,
        get: impl Fn(&Kind) -> Result<T, Rejection>,
    ) -> Result<T, Rejection> {
        let name = family_name(plugin, metric).ok_or(Rejection::InvalidName)?;
        let owned_by = |entry: &Entry| entry.plugin == plugin && entry.metric == metric;
        if let Some(entry) = self.families.read().get(&name) {
            return if owned_by(entry) {
                get(&entry.kind)
            } else {
                Err(Rejection::NameConflict)
            };
        }
        let mut families = self.families.write();
        let entry = families.entry(name).or_insert_with(|| Entry {
            plugin: plugin.to_string(),
            metric: metric.to_string(),
            kind: new(),
        });
        if owned_by(entry) {
            get(&entry.kind)
        } else {
            Err(Rejection::NameConflict)
        }
    }
}

/// The exported name of `plugin`'s metric `metric`, or `None` when either
/// name cannot appear in one.
pub(crate) fn family_name(plugin: &str, metric: &str) -> Option<String> {
    let plugin_ok = !plugin.is_empty()
        && plugin
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let mut chars = metric.chars();
    let metric_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    (plugin_ok && metric_ok)
        .then(|| format!("barbacane_plugin_{}_{metric}", plugin.replace('-', "_")))
}

/// Read a plugin's labels: empty, or a JSON object whose keys are label
/// names (`[a-zA-Z_][a-zA-Z0-9_]*`, not starting with `__`) and whose values
/// are strings, numbers or booleans. Sorted by name.
pub(crate) fn parse_labels(json: &str) -> Option<LabelSet> {
    if json.trim().is_empty() {
        return Some(Vec::new());
    }
    let serde_json::Value::Object(object) = serde_json::from_str(json).ok()? else {
        return None;
    };
    let mut labels = Vec::with_capacity(object.len());
    for (name, value) in object {
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with("__");
        if !valid {
            return None;
        }
        let value = match value {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => return None,
        };
        labels.push((name, value));
    }
    labels.sort();
    Some(labels)
}

/// Encodes the plugin metric families at scrape time.
#[derive(Debug)]
pub(crate) struct PluginMetricsCollector(pub(crate) Arc<PluginMetrics>);

impl Collector for PluginMetricsCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), fmt::Error> {
        for (name, entry) in self.0.families.read().iter() {
            let help = format!("Emitted by the {} plugin", entry.plugin);
            match &entry.kind {
                Kind::Counter(family) => {
                    let metric =
                        encoder.encode_descriptor(name, &help, None, family.metric_type())?;
                    family.encode(metric)?;
                }
                Kind::Histogram(family) => {
                    let metric =
                        encoder.encode_descriptor(name, &help, None, family.metric_type())?;
                    family.encode(metric)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_is_named_after_its_plugin_and_metric() {
        assert_eq!(
            family_name("observability", "slo_violation").as_deref(),
            Some("barbacane_plugin_observability_slo_violation")
        );
        assert_eq!(
            family_name("rate-limit", "hits").as_deref(),
            Some("barbacane_plugin_rate_limit_hits")
        );
        for (plugin, metric) in [
            ("", "m"),
            ("Upper", "m"),
            ("p", ""),
            ("p", "1st"),
            ("p", "with-dash"),
            ("p", "a:b"),
            ("p", "a b"),
        ] {
            assert_eq!(family_name(plugin, metric), None, "{plugin} {metric}");
        }
    }

    #[test]
    fn labels_are_read_and_sorted() {
        assert_eq!(parse_labels(""), Some(vec![]));
        assert_eq!(parse_labels("{}"), Some(vec![]));
        assert_eq!(
            parse_labels(r#"{"status": 200, "route": "/a", "ok": true}"#),
            Some(vec![
                ("ok".into(), "true".into()),
                ("route".into(), "/a".into()),
                ("status".into(), "200".into()),
            ])
        );
        for invalid in [
            "[]",
            "not json",
            r#"{"a": null}"#,
            r#"{"a": [1]}"#,
            r#"{"a": {"b": 1}}"#,
            r#"{"1a": "x"}"#,
            r#"{"a-b": "x"}"#,
            r#"{"__name__": "x"}"#,
        ] {
            assert_eq!(parse_labels(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn a_family_belongs_to_the_plugin_metric_that_created_it() {
        let metrics = PluginMetrics::default();
        let labels = vec![];
        metrics.counter("a", "b_c", &labels).expect("first").inc();
        assert_eq!(
            metrics.counter("a-b", "c", &labels).err(),
            Some(Rejection::NameConflict)
        );
        assert_eq!(
            metrics
                .histogram("a", "b_c", &labels, || Histogram::new([1.0].into_iter()))
                .err(),
            Some(Rejection::TypeConflict)
        );
        assert_eq!(metrics.counter("a", "b_c", &labels).expect("same").get(), 1);
    }
}
