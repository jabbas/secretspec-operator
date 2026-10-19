// SPDX-License-Identifier: Apache-2.0

//! Prometheus metrics for the reconciler.

use std::time::Duration;

use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;
use secretspec_api::constants::{EVENT_GENERATED, EVENT_REGENERATED, EVENT_ROTATED};

/// A label set with one label.
type Label = [(&'static str, &'static str); 1];

/// The operator's metrics and the registry that exposes them.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    reconciles: Family<Label, Counter>,
    reconcile_duration: Histogram,
    generations: Family<Label, Counter>,
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
        Self {
            registry,
            reconciles,
            reconcile_duration,
            generations,
        }
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
}
