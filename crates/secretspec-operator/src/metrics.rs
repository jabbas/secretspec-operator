// SPDX-License-Identifier: Apache-2.0

//! Prometheus metrics: reconcile counters and histogram, plus gauges computed
//! at scrape time from the `SecretSpec` reflector stores (so a deleted
//! `SecretSpec` disappears from them automatically).
//!
//! Metric names (design §14):
//! - `secretspec_reconcile_total{result="ok|error"}` (counter)
//! - `secretspec_reconcile_duration_seconds` (histogram)
//! - `secretspec_generations_total{reason="Generated|Regenerated|Rotated"}` (counter)
//! - `secretspec_secretspecs{ready="true|false|unknown"}` (gauge)
//! - `secretspec_next_rotation_timestamp_seconds{namespace,name}` (gauge)

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use kube::ResourceExt;
use kube::runtime::reflector::Store;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::DescriptorEncoder;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::MetricType;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;
use secretspec_api::constants::{
    CONDITION_READY, EVENT_GENERATED, EVENT_REGENERATED, EVENT_ROTATED,
};
use secretspec_api::v1alpha1::SecretSpec;

/// A label set with one label.
type Label = [(&'static str, &'static str); 1];

/// The reflector stores the scrape-time gauges are computed from.
type Stores = Arc<RwLock<Vec<Store<SecretSpec>>>>;

/// The operator's metrics and the registry that exposes them.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    reconciles: Family<Label, Counter>,
    reconcile_duration: Histogram,
    generations: Family<Label, Counter>,
    stores: Stores,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// A fresh registry with all reconciler metrics registered.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = Registry::default();
        let reconciles = Family::<Label, Counter>::default();
        registry.register(
            "secretspec_reconcile",
            "Reconciles of SecretSpecs by result (ok or error)",
            reconciles.clone(),
        );
        // 5 ms .. ~41 s.
        let reconcile_duration = Histogram::new(exponential_buckets(0.005, 2.0, 14));
        registry.register(
            "secretspec_reconcile_duration_seconds",
            "Duration of one SecretSpec reconcile in seconds",
            reconcile_duration.clone(),
        );
        let generations = Family::<Label, Counter>::default();
        registry.register(
            "secretspec_generations",
            "Secret values written by reason (Generated, Regenerated or Rotated)",
            generations.clone(),
        );
        let stores = Stores::default();
        registry.register_collector(Box::new(StoreCollector {
            stores: Arc::clone(&stores),
        }));
        Self {
            registry,
            reconciles,
            reconcile_duration,
            generations,
            stores,
        }
    }

    /// Adds a `SecretSpec` reflector store to the scrape-time gauges.
    pub fn register_store(&self, store: Store<SecretSpec>) {
        self.stores
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(store);
    }

    /// Records one finished reconcile.
    pub fn record_reconcile(&self, ok: bool, duration: Duration) {
        let result = if ok { "ok" } else { "error" };
        self.reconciles.get_or_create(&[("result", result)]).inc();
        self.reconcile_duration.observe(duration.as_secs_f64());
    }

    /// Records a published event; only `Generated`, `Regenerated` and
    /// `Rotated` count as generations, every other reason is ignored.
    pub fn record_event(&self, reason: &str) {
        let reason = match reason {
            EVENT_GENERATED => EVENT_GENERATED,
            EVENT_REGENERATED => EVENT_REGENERATED,
            EVENT_ROTATED => EVENT_ROTATED,
            _ => return,
        };
        self.generations.get_or_create(&[("reason", reason)]).inc();
    }

    /// The metrics in the Prometheus/OpenMetrics text format.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::new();
        // Writing into a `String` cannot fail; on the impossible error the
        // partial output is returned rather than panicking.
        if encode(&mut out, &self.registry).is_err() {
            tracing::warn!("encoding metrics failed");
        }
        out
    }
}

/// Scrape-time gauges over the registered reflector stores.
#[derive(Debug)]
struct StoreCollector {
    stores: Stores,
}

/// The `ready` label value of a `SecretSpec`: its `Ready` condition status.
fn ready_label(spec: &SecretSpec) -> &'static str {
    let status = spec.status.as_ref().and_then(|s| {
        s.conditions
            .iter()
            .find(|c| c.type_ == CONDITION_READY)
            .map(|c| c.status.as_str())
    });
    match status {
        Some("True") => "true",
        Some("False") => "false",
        _ => "unknown",
    }
}

impl Collector for StoreCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let specs: Vec<Arc<SecretSpec>> = self
            .stores
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .flat_map(Store::state)
            .collect();

        let mut counts = [("true", 0_i64), ("false", 0), ("unknown", 0)];
        for spec in &specs {
            let label = ready_label(spec);
            if let Some(entry) = counts.iter_mut().find(|(l, _)| *l == label) {
                entry.1 += 1;
            }
        }
        let mut family = encoder.encode_descriptor(
            "secretspec_secretspecs",
            "SecretSpecs by status of their Ready condition (true, false or unknown)",
            None,
            MetricType::Gauge,
        )?;
        for (label, count) in counts {
            family
                .encode_family(&[("ready", label)])?
                .encode_gauge(&count)?;
        }

        let mut rotations: Vec<(String, String, i64)> = specs
            .iter()
            .filter_map(|spec| {
                let next = spec.status.as_ref()?.next_rotation_time.as_ref()?;
                Some((
                    spec.namespace().unwrap_or_default(),
                    spec.name_any(),
                    next.0.as_second(),
                ))
            })
            .collect();
        rotations.sort();
        let mut family = encoder.encode_descriptor(
            "secretspec_next_rotation_timestamp_seconds",
            "Unix time of the next scheduled rotation of a SecretSpec",
            None,
            MetricType::Gauge,
        )?;
        for (namespace, name, at) in &rotations {
            family
                .encode_family(&[("namespace", namespace.as_str()), ("name", name.as_str())])?
                .encode_gauge(at)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use secretspec_api::constants::{EVENT_ADOPTED, EVENT_APPLY_FAILED};

    use super::*;

    #[test]
    fn records_reconciles_and_generations() {
        let metrics = Metrics::new();
        metrics.record_reconcile(true, Duration::from_millis(20));
        metrics.record_reconcile(true, Duration::from_millis(20));
        metrics.record_reconcile(false, Duration::from_millis(20));
        metrics.record_event(EVENT_GENERATED);
        metrics.record_event(EVENT_ROTATED);
        metrics.record_event(EVENT_ADOPTED);
        metrics.record_event(EVENT_APPLY_FAILED);
        let text = metrics.encode();
        assert!(
            text.contains(r#"secretspec_reconcile_total{result="ok"} 2"#),
            "{text}"
        );
        assert!(
            text.contains(r#"secretspec_reconcile_total{result="error"} 1"#),
            "{text}"
        );
        assert!(
            text.contains("secretspec_reconcile_duration_seconds_count 3"),
            "{text}"
        );
        assert!(
            text.contains(r#"secretspec_generations_total{reason="Generated"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"secretspec_generations_total{reason="Rotated"} 1"#),
            "{text}"
        );
        assert!(!text.contains("Adopted"), "{text}");
        assert!(!text.contains("ApplyFailed"), "{text}");
    }

    fn spec(name: &str, ready: Option<&str>, next_rotation: Option<&str>) -> SecretSpec {
        let mut status = serde_json::json!({});
        if let Some(ready) = ready {
            status["conditions"] = serde_json::json!([{
                "type": "Ready", "status": ready, "reason": "Generated", "message": "",
                "lastTransitionTime": "2026-10-19T00:00:00Z", "observedGeneration": 1
            }]);
        }
        if let Some(next) = next_rotation {
            status["nextRotationTime"] = serde_json::json!(next);
        }
        serde_json::from_value(serde_json::json!({
            "apiVersion": "secretspec.jabbas.eu/v1alpha1",
            "kind": "SecretSpec",
            "metadata": { "name": name, "namespace": "apps" },
            "spec": { "fields": [{ "name": "password" }] },
            "status": status,
        }))
        .unwrap()
    }

    #[test]
    fn store_gauges_follow_the_store() {
        use kube::runtime::{reflector, watcher};

        let metrics = Metrics::new();
        let text = metrics.encode();
        assert!(
            text.contains(r#"secretspec_secretspecs{ready="true"} 0"#),
            "{text}"
        );

        let (reader, mut writer) = reflector::store::<SecretSpec>();
        metrics.register_store(reader);
        let rotating = spec("b", Some("True"), Some("2026-11-01T00:00:00Z"));
        for obj in [
            spec("a", Some("True"), None),
            rotating.clone(),
            spec("c", Some("False"), None),
            spec("d", None, None),
        ] {
            writer.apply_watcher_event(&watcher::Event::Apply(obj));
        }
        let text = metrics.encode();
        assert!(
            text.contains(r#"secretspec_secretspecs{ready="true"} 2"#),
            "{text}"
        );
        assert!(
            text.contains(r#"secretspec_secretspecs{ready="false"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"secretspec_secretspecs{ready="unknown"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(
                r#"secretspec_next_rotation_timestamp_seconds{namespace="apps",name="b"} 1793491200"#
            ),
            "{text}"
        );
        assert!(!text.contains(r#"name="a""#), "{text}");

        writer.apply_watcher_event(&watcher::Event::Delete(rotating));
        let text = metrics.encode();
        assert!(
            text.contains(r#"secretspec_secretspecs{ready="true"} 1"#),
            "{text}"
        );
        assert!(!text.contains(r#"name="b""#), "{text}");
    }
}
