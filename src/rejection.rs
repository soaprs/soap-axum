//! HTTP-protocol rejections produced at the adapter boundary.

use std::{error::Error, fmt};

use http::StatusCode;
use soaprs_core::SoapError;

/// Safe HTTP rejection that is distinct from application and domain errors.
///
/// Route I/O and normalization attach this value as the diagnostic source of a
/// `SoapError`. The Axum adapter recognizes it before invoking the configured
/// application `HttpErrorMapper`, preserving protocol statuses such as 400,
/// 406, 413, and 415 without adding HTTP-specific variants to `SoapErrorKind`.
pub struct HttpRejection {
    status: StatusCode,
    code: &'static str,
    message: String,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl HttpRejection {
    /// Creates a malformed-request rejection.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// Creates a response-content-negotiation rejection.
    pub fn not_acceptable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_ACCEPTABLE, "not_acceptable", message)
    }

    /// Creates an encoded-body-size rejection.
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", message)
    }

    /// Creates a request-content-type rejection.
    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            message,
        )
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches the technical parser or body-stream failure for diagnostics.
    #[must_use]
    pub fn with_source<E>(mut self, source: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        self.source = Some(Box::new(source));
        self
    }

    /// Returns the HTTP status emitted by the adapter.
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the stable response error code.
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Returns the safe client-facing message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Converts the rejection into the error type propagated through endpoint
    /// middleware and lifecycle hooks.
    pub fn into_error(self) -> SoapError {
        SoapError::validation(self.message.clone()).with_source(self)
    }

    pub(crate) fn find(error: &SoapError) -> Option<&Self> {
        let mut source = Error::source(error);
        while let Some(current) = source {
            if let Some(rejection) = current.downcast_ref::<Self>() {
                return Some(rejection);
            }
            source = current.source();
        }
        None
    }
}

impl fmt::Debug for HttpRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRejection")
            .field("status", &self.status)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl fmt::Display for HttpRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HttpRejection {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
