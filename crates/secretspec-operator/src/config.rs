// SPDX-License-Identifier: Apache-2.0

//! Operator configuration: command-line flags with environment fallbacks.

use std::net::SocketAddr;
use std::time::Duration;

use clap::{ArgAction, Parser, ValueEnum};

/// Shortest allowed resync interval.
pub const MIN_RESYNC: Duration = Duration::from_secs(60);
/// Longest allowed resync interval.
pub const MAX_RESYNC: Duration = Duration::from_secs(24 * 60 * 60);

/// Log output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// One JSON object per line.
    Json,
    /// Human-readable text.
    Text,
}

/// Invalid configuration detected after parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// Leader election needs a namespace for the lease.
    #[error("POD_NAMESPACE (or --lease-namespace) is required when leader election is enabled")]
    LeaseNamespaceRequired,
    /// Resync interval outside the allowed range.
    #[error("resync interval must be between 1m and 24h")]
    ResyncOutOfRange,
    /// Concurrency must allow at least one reconcile.
    #[error("concurrency must be at least 1")]
    ConcurrencyZero,
}

/// Operator configuration.
#[derive(Clone, Debug, Parser)]
#[command(version, about = "Kubernetes operator for SecretSpec resources")]
pub struct Config {
    /// Comma-separated namespaces; empty = all namespaces.
    #[arg(
        long,
        env = "WATCH_NAMESPACES",
        value_delimiter = ',',
        default_value = ""
    )]
    pub watch_namespaces: Vec<String>,
    /// Enable leader election.
    #[arg(long, env = "LEADER_ELECTION", default_value_t = true, action = ArgAction::Set)]
    pub leader_election: bool,
    /// Namespace of the leader-election lease (required with leader election).
    #[arg(long, env = "POD_NAMESPACE")]
    pub lease_namespace: Option<String>,
    /// Lease holder identity; falls back to the hostname.
    #[arg(long, env = "POD_NAME")]
    pub pod_name: Option<String>,
    /// Name of the leader-election lease.
    #[arg(long, env = "LEASE_NAME", default_value = "secretspec-operator")]
    pub lease_name: String,
    /// Periodic resync interval (`<n>s|m|h`, 1m..=24h).
    #[arg(long, env = "RESYNC_INTERVAL", default_value = "1h", value_parser = parse_resync_interval)]
    pub resync_interval: Duration,
    /// Address for the health and metrics HTTP server.
    #[arg(long, env = "HTTP_BIND", default_value = "0.0.0.0:8080")]
    pub http_bind: SocketAddr,
    /// Log filter directive, for example `info`.
    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,
    /// Log output format.
    #[arg(long, env = "LOG_FORMAT", default_value = "json", value_enum)]
    pub log_format: LogFormat,
    /// Maximum concurrent reconciles.
    #[arg(long, env = "CONCURRENCY", default_value_t = 4)]
    pub concurrency: u16,
}

impl Config {
    /// Check cross-field constraints that parsing cannot express.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] describing the first violated constraint.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.leader_election
            && self
                .lease_namespace
                .as_deref()
                .is_none_or(|ns| ns.trim().is_empty())
        {
            return Err(ConfigError::LeaseNamespaceRequired);
        }
        if !(MIN_RESYNC..=MAX_RESYNC).contains(&self.resync_interval) {
            return Err(ConfigError::ResyncOutOfRange);
        }
        if self.concurrency == 0 {
            return Err(ConfigError::ConcurrencyZero);
        }
        Ok(())
    }

    /// Namespaces to watch: trimmed, non-empty, deduplicated and sorted.
    /// An empty result means all namespaces.
    #[must_use]
    pub fn watch_namespaces(&self) -> Vec<String> {
        let mut namespaces: Vec<String> = self
            .watch_namespaces
            .iter()
            .map(|ns| ns.trim())
            .filter(|ns| !ns.is_empty())
            .map(str::to_owned)
            .collect();
        namespaces.sort();
        namespaces.dedup();
        namespaces
    }
}

/// Parse a duration of the form `<n>s`, `<n>m` or `<n>h` with `n >= 1`.
///
/// # Errors
///
/// Returns a message when the unit is unknown, `n` is not a positive integer
/// or the value overflows.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s.len().saturating_sub(1);
    let (digits, unit) = s.split_at_checked(split).unwrap_or((s, ""));
    let multiplier: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => {
            return Err(format!(
                "invalid duration {s:?}: expected <n>s, <n>m or <n>h"
            ));
        }
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "invalid duration {s:?}: expected <n>s, <n>m or <n>h"
        ));
    }
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("invalid duration {s:?}: number too large"))?;
    if n == 0 {
        return Err(format!("invalid duration {s:?}: must be at least 1"));
    }
    n.checked_mul(multiplier)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("invalid duration {s:?}: number too large"))
}

/// [`parse_duration`] restricted to the allowed resync range (1m..=24h).
fn parse_resync_interval(s: &str) -> Result<Duration, String> {
    let d = parse_duration(s)?;
    if (MIN_RESYNC..=MAX_RESYNC).contains(&d) {
        Ok(d)
    } else {
        Err(format!("resync interval {s:?} must be between 1m and 24h"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Config, clap::Error> {
        let mut full = vec!["secretspec-operator"];
        full.extend_from_slice(args);
        Config::try_parse_from(full)
    }

    #[test]
    fn defaults() {
        let c = parse(&[]).unwrap();
        assert_eq!(c.watch_namespaces(), Vec::<String>::new());
        assert!(c.leader_election);
        assert_eq!(c.lease_namespace, None);
        assert_eq!(c.pod_name, None);
        assert_eq!(c.lease_name, "secretspec-operator");
        assert_eq!(c.resync_interval, Duration::from_secs(3600));
        assert_eq!(c.http_bind, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(c.log_level, "info");
        assert_eq!(c.log_format, LogFormat::Json);
        assert_eq!(c.concurrency, 4);
    }

    #[test]
    fn watch_namespaces_are_trimmed_deduplicated_and_sorted() {
        let c = parse(&["--watch-namespaces", " b, a,,a"]).unwrap();
        assert_eq!(c.watch_namespaces(), vec!["a", "b"]);
    }

    #[test]
    fn empty_watch_namespaces_means_all() {
        let c = parse(&["--watch-namespaces", ""]).unwrap();
        assert_eq!(c.watch_namespaces(), Vec::<String>::new());
    }

    #[test]
    fn leader_election_false_parses() {
        let c = parse(&["--leader-election", "false"]).unwrap();
        assert!(!c.leader_election);
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn leader_election_requires_lease_namespace() {
        let c = parse(&[]).unwrap();
        let err = c.validate().unwrap_err();
        assert_eq!(
            err.to_string(),
            "POD_NAMESPACE (or --lease-namespace) is required when leader election is enabled"
        );
        let c = parse(&["--lease-namespace", "ops"]).unwrap();
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_zero_concurrency_and_out_of_range_resync() {
        let mut c = parse(&["--leader-election", "false"]).unwrap();
        c.concurrency = 0;
        assert_eq!(c.validate(), Err(ConfigError::ConcurrencyZero));
        c.concurrency = 1;
        c.resync_interval = Duration::from_secs(59);
        assert_eq!(c.validate(), Err(ConfigError::ResyncOutOfRange));
        c.resync_interval = Duration::from_secs(25 * 3600);
        assert_eq!(c.validate(), Err(ConfigError::ResyncOutOfRange));
    }

    #[test]
    fn parse_duration_accepts_valid_values() {
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("1m"), Ok(Duration::from_secs(60)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("24h"), Ok(Duration::from_secs(86_400)));
    }

    #[test]
    fn parse_duration_rejects_invalid_values() {
        for bad in [
            "0s",
            "1d",
            "abc",
            "",
            "s",
            "-5s",
            "1.5h",
            "99999999999999999999h",
            "é",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn resync_interval_is_range_checked() {
        for ok in ["1m", "60s", "1h", "24h"] {
            assert!(parse(&["--resync-interval", ok]).is_ok(), "{ok}");
        }
        for bad in ["0s", "59s", "25h", "1d", "abc"] {
            assert!(parse(&["--resync-interval", bad]).is_err(), "{bad}");
        }
    }
}
