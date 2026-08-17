//! Normalized Axum request representation.

use std::{collections::BTreeMap, fmt, net::IpAddr, sync::Arc};

use bytes::Bytes;
use http::{
    Extensions, HeaderMap, Method, Uri,
    header::{ACCEPT, CONTENT_TYPE},
};
use serde::de::DeserializeOwned;
use soaprs_core::MessageId;
use soaprs_http::{EndpointMetadata, HttpRequestView};

use crate::HttpRejection;

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

    /// Deserializes normalized path parameters into a transport DTO.
    pub fn decode_path<T>(&self) -> soaprs_core::SoapResult<T>
    where
        T: DeserializeOwned,
    {
        let encoded =
            serde_urlencoded::to_string(&self.normalized.path_parameters).map_err(|error| {
                HttpRejection::bad_request("failed to normalize path parameters")
                    .with_source(error)
                    .into_error()
            })?;
        serde_urlencoded::from_str(&encoded).map_err(|error| {
            HttpRejection::bad_request("path parameters do not match the expected shape")
                .with_source(error)
                .into_error()
        })
    }

    /// Deserializes normalized query parameters into a transport DTO.
    pub fn decode_query<T>(&self) -> soaprs_core::SoapResult<T>
    where
        T: DeserializeOwned,
    {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        for (name, values) in &self.normalized.query_parameters {
            for value in values {
                serializer.append_pair(name, value);
            }
        }
        serde_urlencoded::from_str(&serializer.finish()).map_err(|error| {
            HttpRejection::bad_request("query parameters do not match the expected shape")
                .with_source(error)
                .into_error()
        })
    }

    /// Parses one optional, non-repeated request header into a typed value.
    pub fn optional_header<T>(&self, name: &str) -> soaprs_core::SoapResult<Option<T>>
    where
        T: std::str::FromStr,
    {
        let mut values = self.normalized.headers.get_all(name).iter();
        let Some(value) = values.next() else {
            return Ok(None);
        };
        if values.next().is_some() {
            return Err(HttpRejection::bad_request(format!(
                "request header `{name}` must not be repeated"
            ))
            .into_error());
        }
        let value = value.to_str().map_err(|error| {
            HttpRejection::bad_request(format!("request header `{name}` is not valid visible text"))
                .with_source(error)
                .into_error()
        })?;
        value.parse::<T>().map(Some).map_err(|_| {
            HttpRejection::bad_request(format!(
                "request header `{name}` does not match the expected type"
            ))
            .into_error()
        })
    }

    /// Parses one required, non-repeated request header into a typed value.
    pub fn required_header<T>(&self, name: &str) -> soaprs_core::SoapResult<T>
    where
        T: std::str::FromStr,
    {
        self.optional_header(name)?.ok_or_else(|| {
            HttpRejection::bad_request(format!("required request header `{name}` is missing"))
                .into_error()
        })
    }

    /// Parses every value of a repeated request header into typed values.
    pub fn header_values<T>(&self, name: &str) -> soaprs_core::SoapResult<Vec<T>>
    where
        T: std::str::FromStr,
    {
        self.normalized
            .headers
            .get_all(name)
            .iter()
            .map(|value| {
                let value = value.to_str().map_err(|error| {
                    HttpRejection::bad_request(format!(
                        "request header `{name}` is not valid visible text"
                    ))
                    .with_source(error)
                    .into_error()
                })?;
                value.parse::<T>().map_err(|_| {
                    HttpRejection::bad_request(format!(
                        "request header `{name}` does not match the expected type"
                    ))
                    .into_error()
                })
            })
            .collect()
    }

    /// Decodes a JSON request body after enforcing a JSON `Content-Type`.
    pub fn decode_json<T>(&self) -> soaprs_core::SoapResult<T>
    where
        T: DeserializeOwned,
    {
        self.require_json_content_type()?;
        serde_json::from_slice(self.body()).map_err(|error| {
            if error.is_syntax() || error.is_eof() {
                HttpRejection::bad_request("request body contains malformed JSON")
                    .with_source(error)
                    .into_error()
            } else {
                soaprs_core::SoapError::validation(
                    "JSON request body does not match the expected shape",
                )
                .with_source(error)
            }
        })
    }

    /// Returns typed per-request extensions populated by HTTP middleware.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Returns typed per-request extensions mutably.
    pub fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extensions
    }

    pub(crate) fn require_json_acceptable(&self) -> soaprs_core::SoapResult<()> {
        let values = self.normalized.headers.get_all(ACCEPT);
        if values.iter().next().is_none() {
            return Ok(());
        }
        for value in values {
            let value = value.to_str().map_err(|error| {
                HttpRejection::bad_request("Accept header is not valid visible text")
                    .with_source(error)
                    .into_error()
            })?;
            if accept_value_allows_json(value)? {
                return Ok(());
            }
        }
        Err(
            HttpRejection::not_acceptable("client does not accept an application/json response")
                .into_error(),
        )
    }

    fn require_json_content_type(&self) -> soaprs_core::SoapResult<()> {
        let mut values = self.normalized.headers.get_all(CONTENT_TYPE).iter();
        let Some(value) = values.next() else {
            return Err(HttpRejection::unsupported_media_type(
                "JSON request requires Content-Type: application/json",
            )
            .into_error());
        };
        if values.next().is_some() {
            return Err(HttpRejection::bad_request(
                "request must contain exactly one Content-Type header",
            )
            .into_error());
        }
        let value = value.to_str().map_err(|error| {
            HttpRejection::unsupported_media_type("request Content-Type is not valid text")
                .with_source(error)
                .into_error()
        })?;
        let essence = value.split(';').next().unwrap_or_default().trim();
        let Some((kind, subtype)) = essence.split_once('/') else {
            return Err(HttpRejection::unsupported_media_type(
                "request Content-Type is not a JSON media type",
            )
            .into_error());
        };
        if !kind.eq_ignore_ascii_case("application")
            || !(subtype.eq_ignore_ascii_case("json")
                || subtype.to_ascii_lowercase().ends_with("+json"))
        {
            return Err(HttpRejection::unsupported_media_type(
                "request Content-Type is not a JSON media type",
            )
            .into_error());
        }
        Ok(())
    }
}

fn accept_value_allows_json(value: &str) -> soaprs_core::SoapResult<bool> {
    for item in value.split(',') {
        let mut segments = item.trim().split(';');
        let range = segments.next().unwrap_or_default().trim();
        let Some((kind, subtype)) = range.split_once('/') else {
            return Err(HttpRejection::bad_request(
                "Accept header contains an invalid media range",
            )
            .into_error());
        };
        let mut allowed = true;
        for parameter in segments {
            let parameter = parameter.trim();
            let Some((name, value)) = parameter.split_once('=') else {
                return Err(HttpRejection::bad_request(
                    "Accept header contains an invalid parameter",
                )
                .into_error());
            };
            if name.trim().eq_ignore_ascii_case("q") {
                allowed = parse_quality(value.trim()).ok_or_else(|| {
                    HttpRejection::bad_request("Accept header contains an invalid quality value")
                        .into_error()
                })?;
            }
        }
        if allowed
            && (kind == "*" || kind.eq_ignore_ascii_case("application"))
            && (subtype == "*" || subtype.eq_ignore_ascii_case("json"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn parse_quality(value: &str) -> Option<bool> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match whole {
        "0" => Some(fraction.bytes().any(|byte| byte != b'0')),
        "1" if fraction.bytes().all(|byte| byte == b'0') => Some(true),
        _ => None,
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
