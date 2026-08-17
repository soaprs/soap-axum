//! Observational endpoint lifecycle hooks.

use http::{HeaderMap, StatusCode};
use soaprs_core::SoapError;
use soaprs_http::EndpointMetadata;

use crate::{EndpointOutcome, RouteRequest};

/// Read-only view of the final encoded response.
#[derive(Debug, Clone, Copy)]
pub struct ResponseView<'a> {
    status: StatusCode,
    headers: &'a HeaderMap,
}

impl<'a> ResponseView<'a> {
    pub(crate) const fn new(status: StatusCode, headers: &'a HeaderMap) -> Self {
        Self { status, headers }
    }

    /// Returns the final HTTP status.
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the final response headers.
    pub const fn headers(&self) -> &'a HeaderMap {
        self.headers
    }
}

/// Observes endpoint lifecycle without controlling request flow.
pub trait EndpointHook: Send + Sync {
    /// Observes a request rejected while parsing or buffering it, before a
    /// complete [`RouteRequest`] exists.
    fn on_normalization_rejection(
        &self,
        _endpoint: &EndpointMetadata,
        _error: &SoapError,
        _response: ResponseView<'_>,
    ) {
    }

    /// Runs after request normalization and before middleware.
    fn on_request(&self, _request: &RouteRequest) {}

    /// Runs after middleware and target processing, before error mapping.
    fn on_outcome(&self, _request: &RouteRequest, _outcome: &EndpointOutcome) {}

    /// Runs after response mapping and response effects have been applied.
    fn on_response(&self, _request: &RouteRequest, _response: ResponseView<'_>) {}
}
