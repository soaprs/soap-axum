//! Framework-independent response produced by route I/O.

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE};
use serde::Serialize;
use soaprs_core::{SoapError, SoapResult};
use soaprs_http::HttpResponseEffects;

/// Serialized successful response before conversion into an Axum response.
#[derive(Debug, Clone)]
pub struct RouteResponse {
    status: Option<StatusCode>,
    headers: HeaderMap,
    body: Bytes,
    effects: HttpResponseEffects,
}

impl RouteResponse {
    /// Creates a response with an empty body.
    pub fn empty() -> Self {
        Self {
            status: None,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            effects: HttpResponseEffects::new(),
        }
    }

    /// Serializes a JSON response body.
    pub fn json<T>(value: &T) -> SoapResult<Self>
    where
        T: Serialize + ?Sized,
    {
        let body = serde_json::to_vec(value).map_err(|error| {
            SoapError::infrastructure("failed to serialize HTTP response").with_source(error)
        })?;
        let mut response = Self::bytes(Bytes::from(body));
        response
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(response)
    }

    /// Creates a response from already encoded bytes.
    pub fn bytes(body: Bytes) -> Self {
        Self {
            status: None,
            headers: HeaderMap::new(),
            body,
            effects: HttpResponseEffects::new(),
        }
    }

    /// Overrides the endpoint's declared success status.
    #[must_use]
    pub const fn status(mut self, status: StatusCode) -> Self {
        self.status = Some(status);
        self
    }

    /// Inserts or replaces a response header.
    #[must_use]
    pub fn header(mut self, name: http::HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Attaches transport-neutral response effects.
    pub fn effects(mut self, effects: HttpResponseEffects) -> SoapResult<Self> {
        effects.validate()?;
        self.effects = effects;
        Ok(self)
    }

    /// Returns the optional response status override.
    pub const fn status_override(&self) -> Option<StatusCode> {
        self.status
    }

    /// Returns response headers emitted by the output mapper.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Returns the encoded body.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// Returns transport-neutral effects attached by the output mapper.
    pub fn response_effects(&self) -> &HttpResponseEffects {
        &self.effects
    }

    pub(crate) fn into_parts(self) -> (Option<StatusCode>, HeaderMap, Bytes, HttpResponseEffects) {
        (self.status, self.headers, self.body, self.effects)
    }
}
