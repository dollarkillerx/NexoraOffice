//! The errors a provider call can end with, in the few kinds the interface
//! needs to tell apart: the user can fix a key, wait out a rate limit, retry
//! a server error or check their connection. Messages carry the provider's
//! own words where there are some, never a key.

use std::fmt;

use serde_json::Value;

/// The longest provider message kept, in characters. Error bodies can be
/// whole HTML pages when a proxy answers instead of the API.
const MAX_MESSAGE_CHARS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiError {
    /// The key was refused (401, 403).
    Auth { status: u16, message: String },
    /// Too many requests (429). `retry_after` is in seconds, when the
    /// provider said.
    RateLimited {
        status: u16,
        message: String,
        retry_after: Option<u64>,
    },
    /// The provider failed or is overloaded (5xx, Anthropic's 529).
    Server { status: u16, message: String },
    /// The provider refused the request itself (another 4xx): an unknown
    /// model, a request too large for the context window.
    InvalidRequest { status: u16, message: String },
    /// No connection, a TLS failure, a timeout or a connection that closed
    /// in the middle of a reply.
    Network(String),
    /// The caller set the cancel flag.
    Cancelled,
    /// The provider answered with something this crate cannot read.
    BadResponse(String),
    /// The provider is not set up well enough to be called: no model
    /// chosen, an address that is refused, a key saved for another address.
    NotConfigured(String),
    /// The reply went past one of the [`limits`](crate::limits): a line, the
    /// text, a tool input or the number of tool calls.
    TooLarge(String),
}

impl AiError {
    /// A stable key for the interface to pick its translated text by.
    pub fn kind(&self) -> &'static str {
        match self {
            AiError::Auth { .. } => "auth",
            AiError::RateLimited { .. } => "rate_limited",
            AiError::Server { .. } => "server",
            AiError::InvalidRequest { .. } => "invalid_request",
            AiError::Network(_) => "network",
            AiError::Cancelled => "cancelled",
            AiError::BadResponse(_) => "bad_response",
            AiError::NotConfigured(_) => "not_configured",
            AiError::TooLarge(_) => "too_large",
        }
    }

    /// Whether trying again later can help.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AiError::RateLimited { .. } | AiError::Server { .. } | AiError::Network(_)
        )
    }

    /// The error for an HTTP status the provider answered with, given the
    /// response body and its `retry-after` header.
    pub(crate) fn from_status(status: u16, body: &str, retry_after: Option<&str>) -> AiError {
        let message = error_message(body);
        match status {
            401 | 403 => AiError::Auth { status, message },
            429 => AiError::RateLimited {
                status,
                message,
                retry_after: retry_after.and_then(|value| value.trim().parse().ok()),
            },
            500..=599 => AiError::Server { status, message },
            _ => AiError::InvalidRequest { status, message },
        }
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let with = |f: &mut fmt::Formatter<'_>, what: &str, status: u16, message: &str| {
            if message.is_empty() {
                write!(f, "{what} (HTTP {status})")
            } else {
                write!(f, "{what} (HTTP {status}): {message}")
            }
        };
        match self {
            AiError::Auth { status, message } => {
                with(f, "The API key was not accepted", *status, message)
            }
            AiError::RateLimited {
                status, message, ..
            } => with(f, "Too many requests", *status, message),
            AiError::Server { status, message } => {
                with(f, "The provider had a problem", *status, message)
            }
            AiError::InvalidRequest { status, message } => {
                with(f, "The provider refused the request", *status, message)
            }
            AiError::Network(message) => write!(f, "Network error: {message}"),
            AiError::Cancelled => write!(f, "Stopped"),
            AiError::BadResponse(message) => write!(f, "Unexpected reply: {message}"),
            AiError::NotConfigured(message) => write!(f, "Not set up: {message}"),
            AiError::TooLarge(message) => write!(f, "The reply is too large: {message}"),
        }
    }
}

impl std::error::Error for AiError {}

/// The message in an error body. Anthropic and OpenAI both put it at
/// `error.message`; some compatible servers use `message` or `error` as a
/// string. Anything else is shown as the start of the body.
pub(crate) fn error_message(body: &str) -> String {
    let text = match serde_json::from_str::<Value>(body) {
        Ok(value) => value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .or_else(|| value.get("error").filter(|error| error.is_string()))
            .or_else(|| value.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| body.to_string()),
        Err(_) => body.to_string(),
    };
    shorten(text.trim())
}

pub(crate) fn shorten(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return text.to_string();
    }
    let mut short: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_kinds() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(
            AiError::from_status(401, body, None),
            AiError::Auth {
                status: 401,
                message: "invalid x-api-key".into()
            }
        );
        assert_eq!(
            AiError::from_status(429, "{}", Some("12")),
            AiError::RateLimited {
                status: 429,
                message: "{}".into(),
                retry_after: Some(12)
            }
        );
        assert_eq!(AiError::from_status(529, "", None).kind(), "server");
        assert_eq!(
            AiError::from_status(404, r#"{"error":"model not found"}"#, None),
            AiError::InvalidRequest {
                status: 404,
                message: "model not found".into()
            }
        );
    }

    #[test]
    fn long_bodies_are_cut() {
        let body = "x".repeat(2000);
        assert_eq!(error_message(&body).chars().count(), MAX_MESSAGE_CHARS + 1);
    }
}
