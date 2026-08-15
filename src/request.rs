//! Normalized Axum request representation.

use std::{collections::BTreeMap, fmt, net::IpAddr, sync::Arc};

use bytes::Bytes;
use http::{Extensions, HeaderMap, Method, Uri};
use soaprs_core::MessageId;
use soaprs_http::{EndpointMetadata, HttpRequestView};

/// Framework-normalized request data exposed to middleware and route I/O.
pub struct NormalizedRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    cookies: BTreeMap<String, String>,
    path_parameters: BTreeMap<String, String>,
    query_parameters: BTreeMap<String, Vec<String>>,
    client_ip: Option<IpAddr>,
    request_id: Option<MessageId>,
    body: Bytes,
}

impl fmt::Debug for NormalizedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizedRequest")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("cookie_names", &self.cookies.keys().collect::<Vec<_>>())
            .field(
                "path_parameter_names",
                &self.path_parameters.keys().collect::<Vec<_>>(),
            )
            .field(
                "query_parameter_names",
                &self.query_parameters.keys().collect::<Vec<_>>(),
            )
            .field("client_ip", &self.client_ip)
            .field("request_id", &self.request_id)
            .field("body_length", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl NormalizedRequest {
    pub(crate) fn new(
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        cookies: BTreeMap<String, String>,
        path_parameters: BTreeMap<String, String>,
        query_parameters: BTreeMap<String, Vec<String>>,
        body: Bytes,
    ) -> Self {
        Self {
            method,
            uri,
            headers,
            cookies,
            path_parameters,
            query_parameters,
            client_ip: None,
            request_id: None,
            body,
        }
    }

    /// Returns the buffered encoded request body.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// Sets a client address after application-specific trusted-proxy processing.
    pub fn set_client_ip(&mut self, client_ip: IpAddr) {
        self.client_ip = Some(client_ip);
    }

    /// Sets a request identity generated or accepted at the application boundary.
    pub fn set_request_id(&mut self, request_id: impl Into<MessageId>) {
        self.request_id = Some(request_id.into());
    }
}

impl HttpRequestView for NormalizedRequest {
    fn method(&self) -> &Method {
        &self.method
    }

    fn uri(&self) -> &Uri {
        &self.uri
    }

    fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(String::as_str)
    }

    fn path_parameter(&self, name: &str) -> Option<&str> {
        self.path_parameters.get(name).map(String::as_str)
    }

    fn query_parameters(&self, name: &str) -> Option<&[String]> {
        self.query_parameters.get(name).map(Vec::as_slice)
    }

    fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    fn request_id(&self) -> Option<&MessageId> {
        self.request_id.as_ref()
    }
}

/// Complete per-request context passed through middleware and into route I/O.
pub struct RouteRequest {
    endpoint: Arc<EndpointMetadata>,
    normalized: NormalizedRequest,
    extensions: Extensions,
}

impl fmt::Debug for RouteRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouteRequest")
            .field("endpoint", &self.endpoint.id)
            .field("request", &self.normalized)
            .finish_non_exhaustive()
    }
}

impl RouteRequest {
    pub(crate) fn new(
        endpoint: Arc<EndpointMetadata>,
        normalized: NormalizedRequest,
        extensions: Extensions,
    ) -> Self {
        Self {
            endpoint,
            normalized,
            extensions,
        }
    }

    /// Returns the portable endpoint declaration matched by Axum.
    pub fn endpoint(&self) -> &EndpointMetadata {
        &self.endpoint
    }

    /// Returns the normalized request.
    pub fn normalized(&self) -> &NormalizedRequest {
        &self.normalized
    }

    /// Returns the normalized request mutably to trusted boundary middleware.
    pub fn normalized_mut(&mut self) -> &mut NormalizedRequest {
        &mut self.normalized
    }

    /// Returns the encoded request body.
    pub fn body(&self) -> &Bytes {
        self.normalized.body()
    }

    /// Returns typed per-request extensions populated by HTTP middleware.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Returns typed per-request extensions mutably.
    pub fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extensions
    }
}

impl HttpRequestView for RouteRequest {
    fn method(&self) -> &Method {
        self.normalized.method()
    }

    fn uri(&self) -> &Uri {
        self.normalized.uri()
    }

    fn headers(&self) -> &HeaderMap {
        self.normalized.headers()
    }

    fn cookie(&self, name: &str) -> Option<&str> {
        self.normalized.cookie(name)
    }

    fn path_parameter(&self, name: &str) -> Option<&str> {
        self.normalized.path_parameter(name)
    }

    fn query_parameters(&self, name: &str) -> Option<&[String]> {
        self.normalized.query_parameters(name)
    }

    fn client_ip(&self) -> Option<IpAddr> {
        self.normalized.client_ip()
    }

    fn request_id(&self) -> Option<&MessageId> {
        self.normalized.request_id()
    }
}
