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

/// The built-in directive that silences the kube HTTP client.
const KUBE_CLIENT_OFF: &str = "kube_client::client=off";

/// The tracing filter for `level`, with the kube HTTP client's own logging
/// switched off whatever `level` says: it logs response bodies (which for a
/// Secret contain its data) when decoding fails and API error messages (which
/// can echo submitted data) at debug level. Our own logs report kube errors
/// as code and reason only.
///
/// User directives that could re-enable it are dropped and returned: any
/// directive whose target starts with `kube_client`, and any span-filter
/// directive (`target[span]=level`) whose target is empty or a prefix of
/// `kube_client` — span directives take precedence over the static `off`.
fn filter(level: &str) -> Result<(EnvFilter, Vec<String>), anyhow::Error> {
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
    target.starts_with("kube_client")
        || (directive.contains('[') && KUBE_CLIENT_OFF.starts_with(target))
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
                filter.to_string().contains("kube_client::client=off"),
                "{level}: {filter}"
            );
            assert!(
                !filter.to_string().contains("kube_client::client=trace"),
                "{level}: {filter}"
            );
        }
    }
}
