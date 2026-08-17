//! Request/input and output/response mapping.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use soaprs_core::SoapResult;
use soaprs_http::EndpointMetadata;

use crate::{JsonResponse, RouteRequest, RouteResponse};

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

    /// Maps output with access to normalized request metadata.
    ///
    /// The default preserves the original endpoint-only API. Encoders that
    /// perform content negotiation can override this method without exposing
    /// the request to the application use case.
    fn map_response_for(&self, output: O, request: &RouteRequest) -> SoapResult<RouteResponse> {
        self.map_response(output, request.endpoint())
    }
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
        let wire_input = request.decode_json()?;
        (self.map_input)(request, wire_input)
    }

    fn map_response(&self, output: O, endpoint: &EndpointMetadata) -> SoapResult<RouteResponse> {
        (self.map_output)(output, endpoint)
    }
}

/// JSON route mapper with typed request and response transport DTOs.
///
/// The input mapper may combine decoded body data with typed path, query, and
/// header projections from [`RouteRequest`]. The output mapper returns a
/// [`JsonResponse`] DTO; this adapter owns JSON serialization and `Accept`
/// negotiation.
pub struct TypedJsonRouteIo<I, O, WireInput, WireOutput, MapInput, MapOutput> {
    map_input: MapInput,
    map_output: MapOutput,
    input_marker: PhantomData<fn(WireInput) -> I>,
    output_marker: PhantomData<fn(WireOutput) -> O>,
}

impl<I, O, WireInput, WireOutput, MapInput, MapOutput>
    TypedJsonRouteIo<I, O, WireInput, WireOutput, MapInput, MapOutput>
{
    /// Creates a typed JSON request/input and output/response mapper.
    pub fn new(map_input: MapInput, map_output: MapOutput) -> Self {
        Self {
            map_input,
            map_output,
            input_marker: PhantomData,
            output_marker: PhantomData,
        }
    }
}

impl<I, O, WireInput, WireOutput, MapInput, MapOutput> RouteIo<I, O>
    for TypedJsonRouteIo<I, O, WireInput, WireOutput, MapInput, MapOutput>
where
    I: Send,
    O: Send,
    WireInput: DeserializeOwned,
    WireOutput: serde::Serialize,
    MapInput: Fn(&RouteRequest, WireInput) -> SoapResult<I> + Send + Sync,
    MapOutput: Fn(O, &EndpointMetadata) -> SoapResult<JsonResponse<WireOutput>> + Send + Sync,
{
    fn map_request(&self, request: &RouteRequest) -> SoapResult<I> {
        let wire_input = request.decode_json()?;
        request.require_json_acceptable()?;
        (self.map_input)(request, wire_input)
    }

    fn map_response(&self, output: O, endpoint: &EndpointMetadata) -> SoapResult<RouteResponse> {
        (self.map_output)(output, endpoint)?.into_route_response()
    }

    fn map_response_for(&self, output: O, request: &RouteRequest) -> SoapResult<RouteResponse> {
        request.require_json_acceptable()?;
        self.map_response(output, request.endpoint())
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
