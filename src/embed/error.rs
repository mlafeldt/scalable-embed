use std::fmt;

/// An error from [`Client`](super::Client), classified like the errors
/// `sc --json` reports, so callers can match on its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: &'static str,
    message: String,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// The code of a login stopped by its `cancel` flag.
    pub const LOGIN_CANCELLED: &str = "login_cancelled";

    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }

    /// The code `sc --json` reports in `error.code`, such as `no_session`,
    /// `rate_limited`, `network_error` or `invalid_input`, or
    /// [`Error::LOGIN_CANCELLED`].
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Whether only a new login fixes it: there is no session, or it can't be
    /// refreshed anymore.
    pub fn needs_login(&self) -> bool {
        matches!(self.code, "no_session" | "refresh_relogin_required")
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
        Self::new(
            crate::machine::classify_error(&err).code,
            crate::machine::user_error_message(&err),
        )
    }
}

impl From<clap::Error> for Error {
    fn from(err: clap::Error) -> Self {
        Self::invalid_input(err.to_string().trim_end())
    }
}
