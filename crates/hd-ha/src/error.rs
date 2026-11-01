//! Error type of the Home Assistant client.

use hd_core::i18n::{tr, trf};

#[derive(Debug, Clone)]
pub enum HaError {
    /// The server's certificate was rejected. `fingerprint` is the SHA-256 of
    /// the presented certificate (`sha256:<hex>`), if one was seen.
    Tls {
        fingerprint: Option<String>,
        message: String,
    },
    /// Could not reach the server.
    Network(String),
    /// The request took too long.
    Timeout,
    /// Credentials were rejected (invalid, revoked or expired).
    Auth(String),
    /// Unexpected HTTP status.
    Http { status: u16, message: String },
    /// Home Assistant answered a WebSocket command with an error.
    Ha { code: String, message: String },
    /// The server did not behave like Home Assistant.
    Protocol(String),
    /// The WebSocket connection is closed.
    Closed,
    /// The mobile_app registration was deleted in Home Assistant (HTTP 410).
    Gone,
    /// The user cancelled (e.g. the browser login timed out).
    Cancelled,
    /// Keyring or configuration problem.
    Local(String),
}

impl HaError {
    /// Whether retrying later may help (as opposed to needing user action).
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            HaError::Network(_)
                | HaError::Timeout
                | HaError::Closed
                | HaError::Http {
                    status: 500..=599,
                    ..
                }
        )
    }
}

impl std::fmt::Display for HaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HaError::Tls { message, .. } => {
                write!(
                    f,
                    "{}",
                    trf(
                        "The server's certificate is not trusted ({message}).",
                        &[("message", message)]
                    )
                )
            }
            HaError::Network(m) => write!(
                f,
                "{}",
                trf(
                    "Could not connect to the server: {message}",
                    &[("message", m)]
                )
            ),
            HaError::Timeout => f.write_str(tr("The server did not respond in time.")),
            HaError::Auth(m) => write!(
                f,
                "{}",
                trf(
                    "Home Assistant rejected the login: {message}",
                    &[("message", m)]
                )
            ),
            HaError::Http { status, message } => write!(
                f,
                "{}",
                trf(
                    "Unexpected answer from the server (HTTP {status}): {message}",
                    &[("status", &status.to_string()), ("message", message)]
                )
            ),
            HaError::Ha { code, message } => write!(
                f,
                "{}",
                trf(
                    "Home Assistant reported an error: {message} ({code})",
                    &[("message", message), ("code", code)]
                )
            ),
            HaError::Protocol(m) => write!(
                f,
                "{}",
                trf(
                    "This does not look like a Home Assistant server: {message}",
                    &[("message", m)]
                )
            ),
            HaError::Closed => f.write_str(tr("The connection to Home Assistant was closed.")),
            HaError::Gone => f.write_str(tr("This device was removed from Home Assistant.")),
            HaError::Cancelled => f.write_str(tr("The login was cancelled.")),
            HaError::Local(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for HaError {}

impl From<hd_core::secrets::SecretError> for HaError {
    fn from(e: hd_core::secrets::SecretError) -> Self {
        HaError::Local(e.to_string())
    }
}
