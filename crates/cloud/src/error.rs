//! Error type for the cloud client.

/// Everything that can go wrong talking to Kielbox Cloud.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    /// The configuration is unusable (bad URL, no HTTP client).
    #[error("configuration error: {message}")]
    Config {
        /// What is wrong.
        message: String,
    },
    /// Cloudflare Access or the server rejected the credentials (401/403 or a login
    /// redirect).
    #[error("authentication failed: {message}")]
    Auth {
        /// What went wrong and how to fix it.
        message: String,
    },
    /// No HTTP response was produced (DNS, connect, TLS, reset, timeout).
    #[error("network error: {message}")]
    Network {
        /// Description without URLs or query strings.
        message: String,
    },
    /// The server (API or storage) answered with a non-success status.
    #[error("HTTP {status}{}", .code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Http {
        /// HTTP status code.
        status: u16,
        /// Stable error code from the API envelope, when there was one.
        code: Option<String>,
        /// Server supplied or snippet text.
        message: String,
    },
    /// The server answered successfully but the response made no sense.
    #[error("invalid response from server: {why}")]
    InvalidResponse {
        /// Explanation.
        why: String,
    },
    /// The caller cancelled through the cancellation token.
    #[error("upload cancelled")]
    Cancelled,
    /// The source file changed size while it was being uploaded (still being written).
    #[error("the file changed while it was being uploaded")]
    SourceChanged,
    /// Local I/O failure.
    #[error("I/O error while {context}: {source}")]
    Io {
        /// What we were doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl CloudError {
    pub(crate) fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    pub(crate) fn invalid(why: impl Into<String>) -> Self {
        Self::InvalidResponse { why: why.into() }
    }

    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// True for [`CloudError::Cancelled`].
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}
