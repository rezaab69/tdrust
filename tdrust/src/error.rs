use std::time::Duration;

/// A `Result` whose error type is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong when talking to TDLib.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// TDLib answered a request with an error object.
    #[error(transparent)]
    Td(#[from] TdError),

    /// The `tdjson` shared library could not be loaded.
    #[error("cannot load TDLib: {0}")]
    Library(String),

    /// The client has been closed, or was closed while the request was pending.
    #[error("TDLib client {0} is closed")]
    ClientClosed(i32),

    /// A request could not be turned into JSON, or was not a JSON object with
    /// a `@type` field.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// A response did not match the type expected for it. This usually means
    /// the TDLib library and the schema tdrust was built from are different
    /// versions; see `BUNDLED_TDLIB_VERSION`.
    #[error("cannot decode the response to `{function}`: {source}")]
    Decode {
        /// The TDLib function whose response failed to decode.
        function: &'static str,
        /// The underlying serde error.
        source: serde_json::Error,
        /// The response as received from TDLib.
        json: Box<serde_json::Value>,
    },

    /// A [`ClientBuilder`](crate::ClientBuilder) was missing required settings.
    #[error("invalid client configuration: {0}")]
    Config(String),

    /// Logging in could not be completed.
    #[error("authorization failed: {0}")]
    Authorization(String),

    /// An I/O error, for example while reading console input during login.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The TDLib error, if this is one.
    pub fn as_td(&self) -> Option<&TdError> {
        match self {
            Error::Td(error) => Some(error),
            _ => None,
        }
    }
}

/// An error returned by TDLib, such as `400 PHONE_CODE_INVALID`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("TDLib error {code}: {message}")]
pub struct TdError {
    /// Error code. Codes follow HTTP conventions (400 bad request, 401
    /// unauthorized, 403 forbidden, 404 not found, 406 must not be shown to
    /// the user, 420/429 flood wait, 500 internal error).
    pub code: i32,
    /// Error message, usually an uppercase identifier like `CHAT_NOT_FOUND`.
    pub message: String,
}

impl TdError {
    /// How long to wait before retrying, for "Too Many Requests: retry after N"
    /// (code 429) and `FLOOD_WAIT_N` (code 420) errors.
    ///
    /// ```
    /// # use std::time::Duration;
    /// let error = tdrust::TdError { code: 429, message: "Too Many Requests: retry after 17".into() };
    /// assert_eq!(error.retry_after(), Some(Duration::from_secs(17)));
    /// ```
    pub fn retry_after(&self) -> Option<Duration> {
        if self.code != 429 && self.code != 420 {
            return None;
        }
        let digits: String =
            self.message.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
        digits.parse().ok().map(Duration::from_secs)
    }

    /// Whether the message must not be shown to the user (code 406).
    pub fn is_silent(&self) -> bool {
        self.code == 406
    }
}

impl From<crate::types::Error> for TdError {
    fn from(error: crate::types::Error) -> Self {
        TdError { code: error.code, message: error.message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_parses_flood_waits() {
        let too_many = TdError { code: 429, message: "Too Many Requests: retry after 5".into() };
        assert_eq!(too_many.retry_after(), Some(Duration::from_secs(5)));
        let flood = TdError { code: 420, message: "FLOOD_WAIT_30".into() };
        assert_eq!(flood.retry_after(), Some(Duration::from_secs(30)));
        let other = TdError { code: 400, message: "CHAT_NOT_FOUND".into() };
        assert_eq!(other.retry_after(), None);
    }

    #[test]
    fn display_is_readable() {
        let error = Error::from(TdError { code: 400, message: "PHONE_CODE_INVALID".into() });
        assert_eq!(error.to_string(), "TDLib error 400: PHONE_CODE_INVALID");
        assert_eq!(error.as_td().map(|e| e.code), Some(400));
    }
}
