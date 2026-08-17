//! Fail-closed enforcement coverage and router-level plugin behavior.

use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    http::Request,
    middleware::{Next, from_fn},
    routing::options,
};
use http::{Method, StatusCode};
use soaprs_axum::{
    EmptyRouteIo, EndpointBinding, EndpointMiddleware, EndpointNext, EndpointOutcome,
    PluginContext, RouteRequest, RouteResponse, RouterPlugin, SoapRouter,
};
use soaprs_core::{BoxFuture, SoapResult, UseCase};
use soaprs_http::{
    AuthorizationPolicy, ContractId, CorsPolicy, EndpointCatalog, EndpointMetadata,
    HttpEnforcementCapability, RateLimitPolicy, RequestContract, RequestContractLocation,
    RoutePath,
};
use tower::ServiceExt;

const ENDPOINT_ID: &str = "capabilities.covered";

struct UnitUseCase;

impl UseCase for UnitUseCase {
    type Input = ();
    type Output = ();

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async { Ok(()) })
    }
}

fn binding() -> EndpointBinding {
    EndpointBinding::use_case(Arc::new(UnitUseCase)).route_io(EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |(), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
    ))
}

fn protected_endpoint() -> SoapResult<EndpointMetadata> {
    let contract = ContractId::new("capabilities.body")?;
    let rate_limit = RateLimitPolicy::new(NonZeroU32::MIN, Duration::from_secs(1))?;
    let cors = CorsPolicy::any(vec![Method::POST])?;
    Ok(
        EndpointMetadata::new(ENDPOINT_ID, Method::POST, RoutePath::new("/covered")?)?
            .authorize(AuthorizationPolicy::Authenticated)?
            .request_contract(RequestContract::new(
                contract,
                RequestContractLocation::Body,
            ))
            .rate_limit(rate_limit)
            .cors(cors)
            .require_csrf(),
    )
}

fn catalog(endpoint: EndpointMetadata) -> SoapResult<EndpointCatalog> {
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;
    Ok(catalog)
}

fn documented_endpoint() -> SoapResult<EndpointMetadata> {
    Ok(EndpointMetadata::new(
        "capabilities.documented",
        Method::POST,
        RoutePath::new("/documented")?,
    )?
    .request_contract(RequestContract::new(
        ContractId::new("capabilities.documented.body")?,
        RequestContractLocation::Body,
    )))
}

struct EndpointEnforcement;

impl EndpointMiddleware for EndpointEnforcement {
    fn enforcement_capabilities(&self) -> &'static [HttpEnforcementCapability] {
        &[
            HttpEnforcementCapability::Authentication,
            HttpEnforcementCapability::RequestValidation,
            HttpEnforcementCapability::RateLimit,
            HttpEnforcementCapability::Csrf,
        ]
    }

    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        next.run(request)
    }
}

struct CorsRouterPlugin;

impl RouterPlugin for CorsRouterPlugin {
    fn name(&self) -> &'static str {
        "contract-cors"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        context.router_enforcement_capability(HttpEnforcementCapability::Cors);
        context.transform_router(|router| {
            Ok(router.route("/covered", options(|| async { StatusCode::NO_CONTENT })))
        });
        Ok(())
    }
}

struct OuterTelemetryPlugin {
    statuses: Arc<Mutex<Vec<StatusCode>>>,
}

impl RouterPlugin for OuterTelemetryPlugin {
    fn name(&self) -> &'static str {
        "contract-outer-telemetry"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        let statuses = Arc::clone(&self.statuses);
        context.transform_router(move |router| {
            let statuses = Arc::clone(&statuses);
            Ok(
                router.layer(from_fn(move |request: Request<Body>, next: Next| {
                    let statuses = Arc::clone(&statuses);
                    async move {
                        let response = next.run(request).await;
                        if let Ok(mut statuses) = statuses.lock() {
                            statuses.push(response.status());
                        }
                        response
                    }
                })),
            )
        });
        Ok(())
    }
}

#[test]
fn build_rejects_declared_but_unprovided_enforcement() {
    let endpoint =
        protected_endpoint().unwrap_or_else(|error| panic!("valid protected endpoint: {error}"));
    let catalog = catalog(endpoint).unwrap_or_else(|error| panic!("valid catalog: {error}"));
    let result = SoapRouter::builder(catalog)
        .bind(ENDPOINT_ID, binding())
        .and_then(|builder| builder.build());
    let error = result
        .err()
        .unwrap_or_else(|| panic!("missing capability coverage must fail the build"));
    let message = error.message();
    for capability in [
        "authentication",
        "request validation",
        "rate limiting",
        "CORS",
        "CSRF",
    ] {
        assert!(
            message.contains(capability),
            "missing {capability}: {message}"
        );
    }
}

#[test]
fn build_requires_an_explicit_opt_out_for_externally_enforced_metadata() {
    let endpoint =
        documented_endpoint().unwrap_or_else(|error| panic!("valid documented endpoint: {error}"));
    let catalog = catalog(endpoint).unwrap_or_else(|error| panic!("valid catalog: {error}"));
    let result = SoapRouter::builder(catalog)
        .allow_unenforced(
            "capabilities.documented",
            HttpEnforcementCapability::RequestValidation,
        )
        .and_then(|builder| builder.bind("capabilities.documented", binding()))
        .and_then(|builder| builder.build());
    assert!(result.is_ok(), "explicit external enforcement must build");
}

#[tokio::test]
async fn router_plugin_can_cover_preflight_while_endpoint_middleware_covers_the_pipeline() {
    let endpoint =
        protected_endpoint().unwrap_or_else(|error| panic!("valid protected endpoint: {error}"));
    let catalog = catalog(endpoint).unwrap_or_else(|error| panic!("valid catalog: {error}"));
    let app = SoapRouter::builder(catalog)
        .plugin(CorsRouterPlugin)
        .and_then(|builder| builder.bind(ENDPOINT_ID, binding().middleware(EndpointEnforcement)))
        .and_then(|builder| builder.build())
        .unwrap_or_else(|error| panic!("covered router must build: {error}"));

    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("/covered")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("valid preflight request: {error}"));
    let response = app
        .oneshot(request)
        .await
        .unwrap_or_else(|error| panic!("preflight response: {error}"));
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn outer_router_transform_observes_unmatched_responses() {
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let endpoint = EndpointMetadata::new(
        "capabilities.known",
        Method::GET,
        RoutePath::new("/known").unwrap_or_else(|error| panic!("valid path: {error}")),
    )
    .unwrap_or_else(|error| panic!("valid endpoint: {error}"));
    let catalog = catalog(endpoint).unwrap_or_else(|error| panic!("valid catalog: {error}"));
    let app = SoapRouter::builder(catalog)
        .plugin(OuterTelemetryPlugin {
            statuses: Arc::clone(&statuses),
        })
        .and_then(|builder| builder.bind("capabilities.known", binding()))
        .and_then(|builder| builder.build())
        .unwrap_or_else(|error| panic!("telemetry router must build: {error}"));

    let request = Request::builder()
        .uri("/missing")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("valid missing request: {error}"));
    let response = app
        .oneshot(request)
        .await
        .unwrap_or_else(|error| panic!("missing response: {error}"));
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        statuses
            .lock()
            .unwrap_or_else(|error| panic!("telemetry status lock: {error}"))
            .as_slice(),
        [StatusCode::NOT_FOUND]
    );
}
