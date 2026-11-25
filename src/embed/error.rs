use std::fmt;
use std::time::Duration;

/// An error from [`Client`](super::Client), classified like the errors
/// `sc --json` reports, so callers can match on its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: &'static str,
    message: String,
    retry_after: Option<Duration>,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }

    /// The code `sc --json` reports in `error.code`, such as `no_session`,
    /// `rate_limited`, `network_error` or `invalid_input`. A login stopped
    /// by its `cancel` flag has code `login_cancelled`.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Whether only a new login fixes it: there is no session, or it can't be
    /// refreshed anymore.
    pub fn needs_login(&self) -> bool {
        matches!(self.code, "no_session" | "refresh_relogin_required")
    }

    /// How long the backend asked to wait before trying again, when it
    /// rate-limited the request with a `Retry-After` in seconds.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        let retry_after = (err.chain())
            .find_map(|cause| cause.downcast_ref::<RateLimited>())
            .and_then(|limited| limited.retry_after);
        Self {
            retry_after,
            ..Self::new(
                crate::machine::classify_error(&err).code,
                crate::machine::user_error_message(&err),
            )
        }
    }
}

/// A rate limit, which `graphql.rs` raises with the backend's `Retry-After`
/// beside its message, so `Error::retry_after` needn't read it back out of
/// the text. Its message is upstream's, which upstream classifies by.
#[derive(Debug)]
pub(crate) struct RateLimited {
    message: String,
    retry_after: Option<Duration>,
}

/// The hook `graphql.rs` raises a 429 with: upstream's `message`, and the
/// `Retry-After` seconds it read from the response, if a number.
pub(crate) fn rate_limited(message: String, retry_after_seconds: Option<u64>) -> RateLimited {
    RateLimited {
        message,
        retry_after: retry_after_seconds.map(Duration::from_secs),
    }
}

impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RateLimited {}

impl From<clap::Error> for Error {
    fn from(err: clap::Error) -> Self {
        Self::invalid_input(err.to_string().trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_the_wait_a_rate_limit_asked_for() {
        let message =
            "RATE_LIMITED: backend rate limit exceeded during BrokerOverview; retry after 30s";
        let limited = |seconds| {
            let err = anyhow::Error::new(rate_limited(message.to_string(), seconds));
            Error::from(err.context("query failed"))
        };
        let err = limited(Some(30));
        // Classified and worded as upstream's message alone was.
        let plain = Error::from(anyhow::anyhow!(message).context("query failed"));
        assert_eq!(err.code(), "rate_limited");
        assert_eq!(
            (err.code(), err.to_string()),
            (plain.code(), plain.to_string())
        );
        assert_eq!(err.retry_after(), Some(Duration::from_secs(30)));
        assert_eq!(limited(None).retry_after(), None);
        // A wait in another error's text is none.
        let err = Error::from(anyhow::anyhow!("GraphQL HTTP error 400: retry after 30s"));
        assert_eq!(err.retry_after(), None);
    }

    #[test]
    fn preserves_refresh_relogin_required_from_the_error_chain() {
        let source = crate::auth::refresh_relogin_required("refresh token was rejected")
            .into_error()
            .context("query failed");
        let err = Error::from(source);
        assert_eq!(err.code(), "refresh_relogin_required");
        assert!(err.needs_login());
    }
}
