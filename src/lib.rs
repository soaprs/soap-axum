//! Axum adapter for the transport-neutral HTTP contracts in soaprs.
//!
//! The crate turns an [`EndpointCatalog`](soaprs_http::EndpointCatalog) into an
//! Axum router while keeping application use cases independent from HTTP. A
//! [`RouteIo`] maps normalized HTTP requests into use-case input and successful
//! use-case output back into a [`RouteResponse`].

#[cfg(feature = "auth")]
mod auth;
mod binding;
mod hook;
mod middleware;
mod plugin;
mod policy;
#[cfg(feature = "rate-limit")]
mod rate_limit;
mod request;
mod response;
mod route_io;
mod router;
#[cfg(feature = "validation")]
mod validation;

#[cfg(feature = "auth")]
pub use auth::{AuthContext, AuthenticationMiddleware, BuiltInAuthorization, HttpAuthorization};
pub use binding::{EndpointBinding, HandlerBinding, HttpHandler, UseCaseBinding};
pub use hook::{EndpointHook, ResponseView};
pub use middleware::{EndpointMiddleware, EndpointNext, EndpointOutcome};
pub use plugin::{PluginContext, RouterPlugin};
#[cfg(feature = "rate-limit")]
pub use rate_limit::{BuiltInRateLimitKeyResolver, HttpRateLimitKeyResolver, RateLimitMiddleware};
pub use request::{NormalizedRequest, RouteRequest};
pub use response::RouteResponse;
pub use route_io::{EmptyRouteIo, JsonRouteIo, RouteIo};
pub use router::{SoapRouter, SoapRouterBuilder};
#[cfg(feature = "validation")]
pub use validation::ValidationMiddleware;
