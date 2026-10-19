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
    let filter = EnvFilter::try_new(level)
        .map_err(|e| anyhow::anyhow!("invalid log level {level:?}: {e}"))?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let result = match format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Text => builder.try_init(),
    };
    result.map_err(|e| anyhow::anyhow!("failed to install log subscriber: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_level_is_rejected() {
        let err = init_logging("info,=bad[", LogFormat::Text).unwrap_err();
        assert!(err.to_string().contains("invalid log level"));
    }
}
