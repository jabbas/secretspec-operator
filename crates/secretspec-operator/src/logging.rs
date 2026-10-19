// SPDX-License-Identifier: Apache-2.0

//! Tracing subscriber setup.

use tracing_subscriber::EnvFilter;

use crate::config::LogFormat;

/// Install the global tracing subscriber writing to stderr.
///
/// `level` is an `EnvFilter` directive string (for example `info` or
/// `secretspec_operator=debug,warn`).
///
/// # Errors
///
/// Returns an error if `level` is not a valid filter or a global subscriber
/// is already installed.
pub fn init_logging(level: &str, format: LogFormat) -> Result<(), anyhow::Error> {
    let (filter, ignored) = filter(level)?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let result = match format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Text => builder.try_init(),
    };
    result.map_err(|e| anyhow::anyhow!("failed to install log subscriber: {e}"))?;
    for directive in ignored {
        tracing::warn!(
            directive,
            "ignoring log directive for kube_client: it may log Secret contents"
        );
    }
    Ok(())
}

/// The kube client crate's tracing target.
const KUBE_CLIENT: &str = "kube_client";
/// The built-in directive that silences the whole kube client crate.
const KUBE_CLIENT_OFF: &str = "kube_client=off";

/// The tracing filter for `level`, with the kube client crate's own logging
/// switched off whatever `level` says: its HTTP client logs response bodies
/// (which for a Secret contain its data) when decoding fails and API error
/// messages (which can echo submitted data) at debug level. Our own logs
/// report kube errors as code and reason only.
///
/// User directives that could re-enable it are dropped and returned: any
/// directive whose target starts with `kube_client`, and any span-filter
/// directive (`target[span]=level`) whose target is empty or a prefix of
/// `kube_client` — span directives take precedence over the static `off`.
///
/// This is the filter [`init_logging`] installs; it is public so tests can
/// capture logs exactly as production filters them.
///
/// # Errors
///
/// Returns an error if `level` is not a valid filter.
pub fn filter(level: &str) -> Result<(EnvFilter, Vec<String>), anyhow::Error> {
    // Validate the whole string first so a malformed directive is reported
    // even if it would have been dropped.
    EnvFilter::try_new(level).map_err(|e| anyhow::anyhow!("invalid log level {level:?}: {e}"))?;
    let (ignored, kept): (Vec<&str>, Vec<&str>) = split_directives(level)
        .into_iter()
        .partition(|directive| reaches_kube_client(directive));
    let filter = EnvFilter::try_new(kept.join(","))
        .map_err(|e| anyhow::anyhow!("invalid log level {level:?}: {e}"))?;
    let quiet = KUBE_CLIENT_OFF
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid built-in log directive: {e}"))?;
    Ok((
        filter.add_directive(quiet),
        ignored.into_iter().map(str::to_owned).collect(),
    ))
}

/// Splits a filter string at the commas between directives (not those inside
/// `[span{field=a,other=b}]`), trimming and skipping empty parts.
fn split_directives(level: &str) -> Vec<&str> {
    let mut directives = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    for (index, ch) in level.char_indices() {
        match ch {
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                directives.push(&level[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    directives.push(&level[start..]);
    directives
        .into_iter()
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .collect()
}

/// Whether `directive` could enable events of the `kube_client` crate.
fn reaches_kube_client(directive: &str) -> bool {
    let target = directive_target(directive);
    target.starts_with(KUBE_CLIENT) || (directive.contains('[') && KUBE_CLIENT.starts_with(target))
}

/// The target of a directive; empty for a bare level (`info`) or a
/// target-less span filter (`[span]=debug`).
fn directive_target(directive: &str) -> &str {
    if let Some(end) = directive.find(['[', '=']) {
        return &directive[..end];
    }
    let is_level = ["off", "error", "warn", "info", "debug", "trace"]
        .iter()
        .any(|level| directive.eq_ignore_ascii_case(level))
        || directive.chars().all(|ch| ch.is_ascii_digit());
    if is_level { "" } else { directive }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn invalid_level_is_rejected() {
        let err = init_logging("info,=bad[", LogFormat::Text).unwrap_err();
        assert!(err.to_string().contains("invalid log level"));
    }

    #[test]
    fn kube_client_directives_are_dropped() {
        let cases: [(&str, &[&str]); 9] = [
            ("info", &[]),
            ("kube=debug,info", &[]),
            (
                "kube_client::client=trace,debug",
                &["kube_client::client=trace"],
            ),
            ("kube_client", &["kube_client"]),
            (
                "kube_client::client[HTTP]=trace,secretspec_operator=debug",
                &["kube_client::client[HTTP]=trace"],
            ),
            ("[HTTP]=trace,info", &["[HTTP]=trace"]),
            ("kube[HTTP]=trace", &["kube[HTTP]=trace"]),
            (
                "info,kube_client::client::builder[s{a=1}]=debug",
                &["kube_client::client::builder[s{a=1}]=debug"],
            ),
            ("secretspec_operator[reconcile]=debug", &[]),
        ];
        for (level, expected) in cases {
            let (_, ignored) = filter(level).unwrap();
            assert_eq!(ignored, expected, "{level}");
        }
    }

    #[test]
    fn split_directives_respects_brackets() {
        assert_eq!(
            split_directives(" info, a[s{x=1,y=2}]=debug ,,b=warn"),
            ["info", "a[s{x=1,y=2}]=debug", "b=warn"]
        );
    }

    #[test]
    fn kept_directives_survive() {
        let (filter, _) = filter("secretspec_operator=debug,kube_client=trace,warn").unwrap();
        let text = filter.to_string();
        assert!(text.contains("secretspec_operator=debug"), "{text}");
        assert!(text.contains("warn"), "{text}");
        assert!(!text.contains("kube_client=trace"), "{text}");
    }

    #[test]
    fn kube_client_logging_is_always_off() {
        for level in [
            "info",
            "trace",
            "kube_client::client=trace,debug",
            "kube_client::client[HTTP]=trace",
        ] {
            let (filter, _) = filter(level).unwrap();
            assert!(
                filter.to_string().contains("kube_client=off"),
                "{level}: {filter}"
            );
            assert!(
                !filter.to_string().contains("kube_client::client=trace"),
                "{level}: {filter}"
            );
        }
    }

    /// Collects formatted log output in memory.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// What `level` lets through, from events emitted by every relevant
    /// target at every level.
    fn emitted(level: &str) -> String {
        let (filter, _) = filter(level).unwrap();
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "kube_client::client", "LEAK-client-trace");
            tracing::debug!(target: "kube_client::client", "LEAK-client-debug");
            tracing::error!(target: "kube_client::client", "LEAK-client-error");
            tracing::trace!(target: "kube_client::client::builder", "LEAK-builder");
            tracing::debug!(target: "kube_client::config", "LEAK-config");
            tracing::warn!(target: "kube_client", "LEAK-root");
            tracing::trace!(target: "secretspec_operator", "own-trace");
            tracing::info!(target: "kube_runtime", "runtime-info");
        });
        let bytes = capture
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn trace_level_keeps_the_whole_kube_client_crate_silent() {
        for level in [
            "trace",
            "kube_client=trace,trace",
            "kube_client::client=trace,trace",
            "[HTTP]=trace,trace",
            "kube=trace,trace",
        ] {
            let logs = emitted(level);
            assert!(!logs.contains("LEAK"), "{level}: {logs}");
            assert!(logs.contains("own-trace"), "{level}: {logs}");
            assert!(logs.contains("runtime-info"), "{level}: {logs}");
        }
    }
}
