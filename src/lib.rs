//! Axum adapter for the transport-neutral HTTP contracts in soaprs.
//!
//! The crate turns an [`EndpointCatalog`](soaprs_http::EndpointCatalog) into an
//! Axum router while keeping application use cases independent from HTTP. A
//! [`RouteIo`] maps normalized HTTP requests into use-case input and successful
//! use-case output back into a [`RouteResponse`].

mod binding;
mod hook;
mod middleware;
mod plugin;
mod request;
mod response;
mod route_io;
mod router;

pub use binding::{EndpointBinding, HandlerBinding, HttpHandler, UseCaseBinding};
pub use hook::{EndpointHook, ResponseView};
pub use middleware::{EndpointMiddleware, EndpointNext, EndpointOutcome};
pub use plugin::{PluginContext, RouterPlugin};
pub use request::{NormalizedRequest, RouteRequest};
pub use response::RouteResponse;
pub use route_io::{EmptyRouteIo, JsonRouteIo, RouteIo};
pub use router::{SoapRouter, SoapRouterBuilder};
