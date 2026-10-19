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
    let filter = filter(level)?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let result = match format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Text => builder.try_init(),
    };
    result.map_err(|e| anyhow::anyhow!("failed to install log subscriber: {e}"))
}

/// The tracing filter for `level`, with the kube HTTP client's own logging
/// switched off whatever `level` says: it logs response bodies (which for a
/// Secret contain its data) when decoding fails and API error messages (which
/// can echo submitted data) at debug level. Our own logs report kube errors
/// as code and reason only.
fn filter(level: &str) -> Result<EnvFilter, anyhow::Error> {
    let filter = EnvFilter::try_new(level)
        .map_err(|e| anyhow::anyhow!("invalid log level {level:?}: {e}"))?;
    let quiet = "kube_client::client=off"
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid built-in log directive: {e}"))?;
    Ok(filter.add_directive(quiet))
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
    fn kube_client_logging_is_always_off() {
        for level in ["info", "trace", "kube_client::client=trace,debug"] {
            let filter = filter(level).unwrap();
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
