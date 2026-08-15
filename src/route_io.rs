//! Request/input and output/response mapping.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use soaprs_core::{SoapError, SoapResult};
use soaprs_http::EndpointMetadata;

use crate::{RouteRequest, RouteResponse};

/// Maps normalized HTTP data into application input and successful output back
/// into an HTTP response.
pub trait RouteIo<I, O>: Send + Sync {
    /// Maps the normalized request to input accepted by a target.
    fn map_request(&self, request: &RouteRequest) -> SoapResult<I>;

    /// Maps successful target output to a response.
    ///
    /// Failed target results bypass this method and are handled by the
    /// configured `HttpErrorMapper`.
    fn map_response(&self, output: O, endpoint: &EndpointMetadata) -> SoapResult<RouteResponse>;
}

/// JSON request mapper backed by two typed closure functions.
pub struct JsonRouteIo<I, O, WireInput, MapInput, MapOutput> {
    map_input: MapInput,
    map_output: MapOutput,
    marker: PhantomData<fn(WireInput) -> (I, O)>,
}

impl<I, O, WireInput, MapInput, MapOutput> JsonRouteIo<I, O, WireInput, MapInput, MapOutput> {
    /// Creates a JSON route mapper.
    pub fn new(map_input: MapInput, map_output: MapOutput) -> Self {
        Self {
            map_input,
            map_output,
            marker: PhantomData,
        }
    }
}

impl<I, O, WireInput, MapInput, MapOutput> RouteIo<I, O>
    for JsonRouteIo<I, O, WireInput, MapInput, MapOutput>
where
    I: Send,
    O: Send,
    WireInput: DeserializeOwned,
    MapInput: Fn(&RouteRequest, WireInput) -> SoapResult<I> + Send + Sync,
    MapOutput: Fn(O, &EndpointMetadata) -> SoapResult<RouteResponse> + Send + Sync,
{
    fn map_request(&self, request: &RouteRequest) -> SoapResult<I> {
        let wire_input = serde_json::from_slice(request.body()).map_err(|error| {
            SoapError::validation("invalid JSON request body").with_source(error)
        })?;
        (self.map_input)(request, wire_input)
    }

    fn map_response(&self, output: O, endpoint: &EndpointMetadata) -> SoapResult<RouteResponse> {
        (self.map_output)(output, endpoint)
    }
}

/// Body-free request mapper backed by two typed closure functions.
pub struct EmptyRouteIo<I, O, MapInput, MapOutput> {
    map_input: MapInput,
    map_output: MapOutput,
    marker: PhantomData<fn() -> (I, O)>,
}

impl<I, O, MapInput, MapOutput> EmptyRouteIo<I, O, MapInput, MapOutput> {
    /// Creates a body-free route mapper.
    pub fn new(map_input: MapInput, map_output: MapOutput) -> Self {
        Self {
            map_input,
            map_output,
            marker: PhantomData,
        }
    }
}

impl<I, O, MapInput, MapOutput> RouteIo<I, O> for EmptyRouteIo<I, O, MapInput, MapOutput>
where
    I: Send,
    O: Send,
    MapInput: Fn(&RouteRequest) -> SoapResult<I> + Send + Sync,
    MapOutput: Fn(O, &EndpointMetadata) -> SoapResult<RouteResponse> + Send + Sync,
{
    fn map_request(&self, request: &RouteRequest) -> SoapResult<I> {
        (self.map_input)(request)
    }

    fn map_response(&self, output: O, endpoint: &EndpointMetadata) -> SoapResult<RouteResponse> {
        (self.map_output)(output, endpoint)
    }
}
