use std::fmt;
use std::time::Duration;

use serde_json::Value;

/// An error from [`Client`](super::Client), classified like the errors
/// `sc --json` reports, so callers can match on its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: &'static str,
    message: String,
    retry_after: Option<Duration>,
    graphql_errors: Vec<GraphqlError>,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after: None,
            graphql_errors: Vec::new(),
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

    /// The errors the backend answered with in a GraphQL response's `errors`,
    /// in its order, when they failed the request. Empty for any other error,
    /// such as an HTTP error.
    pub fn graphql_errors(&self) -> &[GraphqlError] {
        &self.graphql_errors
    }
}

/// One error from a GraphQL response's `errors`, as the backend sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlError(Value);

impl GraphqlError {
    /// Its `message`, such as `Transaction with ID … not found`.
    pub fn message(&self) -> Option<&str> {
        self.0["message"].as_str()
    }

    /// Its `extensions.code`, such as `BAD_REQUEST` or `UNAUTHENTICATED`.
    pub fn code(&self) -> Option<&str> {
        self.0["extensions"]["code"].as_str()
    }

    /// Its `validationErrors.errorCode`, such as `TransactionNotFound`, also
    /// when under `extensions` or in a list, as upstream reads it.
    pub fn validation_error_code(&self) -> Option<&str> {
        fn error_code(value: &Value) -> Option<&str> {
            match value {
                Value::Array(items) => items.iter().find_map(|item| item["errorCode"].as_str()),
                value => value["errorCode"].as_str(),
            }
        }
        error_code(&self.0["validationErrors"])
            .or_else(|| error_code(&self.0["extensions"]["validationErrors"]))
    }

    /// The whole error object, for fields the accessors don't cover.
    pub fn json(&self) -> &Value {
        &self.0
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
        let graphql_errors = (err.chain())
            .find_map(|cause| cause.downcast_ref::<GraphqlErrors>())
            .map(|errors| errors.errors.clone())
            .unwrap_or_default();
        Self {
            retry_after,
            graphql_errors,
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

/// A GraphQL response's errors, which `graphql.rs` raises beside its message,
/// so `Error::graphql_errors` needn't read them back out of the text. Its
/// message is upstream's, which upstream classifies by.
#[derive(Debug)]
pub(crate) struct GraphqlErrors {
    message: String,
    errors: Vec<GraphqlError>,
}

/// The hook `graphql.rs` raises a response's `errors` with: upstream's
/// `message`, and the `errors` value, a list of error objects.
pub(crate) fn graphql_errors(message: String, errors: Value) -> GraphqlErrors {
    let errors = match errors {
        Value::Array(items) => items,
        other => vec![other],
    };
    GraphqlErrors {
        message,
        errors: errors.into_iter().map(GraphqlError).collect(),
    }
}

impl fmt::Display for GraphqlErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GraphqlErrors {}

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
    fn carries_the_graphql_errors_a_response_failed_with() {
        // As the backend refuses a transaction it doesn't know, and an empty ID.
        let errors = serde_json::json!([
            {
                "errorType": "DataFetchingException",
                "extensions": {"classification": "DataFetchingException", "code": "BAD_REQUEST"},
                "message": "Transaction with ID tx-1 not found",
                "validationErrors": {"errorCode": "TransactionNotFound"}
            },
            {
                "errorType": "DataFetchingException",
                "extensions": {"classification": "DataFetchingException", "code": "BAD_REQUEST"},
                "validationErrors": {}
            }
        ]);
        let message = "GraphQL returned errors for TransactionDetails (code: BAD_REQUEST)";
        let err = anyhow::Error::new(graphql_errors(message.to_string(), errors.clone()));
        let err = Error::from(err.context("query failed"));
        // Classified and worded as upstream's message alone was.
        let plain = Error::from(anyhow::anyhow!(message).context("query failed"));
        assert_eq!(err.code(), "internal_error");
        assert_eq!(
            (err.code(), err.to_string()),
            (plain.code(), plain.to_string())
        );

        let [found, empty] = err.graphql_errors() else {
            panic!("two errors: {:?}", err.graphql_errors());
        };
        assert_eq!(found.message(), Some("Transaction with ID tx-1 not found"));
        assert_eq!(found.code(), Some("BAD_REQUEST"));
        assert_eq!(found.validation_error_code(), Some("TransactionNotFound"));
        assert_eq!(found.json()["errorType"], "DataFetchingException");
        assert_eq!(found.json(), &errors[0]);
        assert_eq!(
            (empty.message(), empty.code(), empty.validation_error_code()),
            (None, Some("BAD_REQUEST"), None)
        );
        assert!(plain.graphql_errors().is_empty());
    }

    #[test]
    fn reads_validation_error_codes_where_upstream_does() {
        let code = |error| {
            let err = Error::from(anyhow::Error::new(graphql_errors(
                "GraphQL returned errors".to_string(),
                error,
            )));
            (err.graphql_errors().iter())
                .map(|error| error.validation_error_code().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let under_extensions = serde_json::json!(
            [{"extensions": {"validationErrors": {"errorCode": "PortfolioGroupNotFound"}}}]
        );
        assert_eq!(
            code(under_extensions),
            [Some("PortfolioGroupNotFound".into())]
        );
        let listed = serde_json::json!(
            [{"validationErrors": [{"field": "name"}, {"errorCode": "GroupAlreadyExists"}]}]
        );
        assert_eq!(code(listed), [Some("GroupAlreadyExists".into())]);
        // Not a list, against the spec: kept as the one error it is.
        let single = serde_json::json!({"validationErrors": {"errorCode": "TransactionNotFound"}});
        assert_eq!(code(single), [Some("TransactionNotFound".into())]);
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
