//! Contract coverage for the optional `soaprs-validation` bridge.

#![cfg(feature = "validation")]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{body::Body, http::Request};
use http::{Method, StatusCode};
use soaprs_axum::{
    EmptyRouteIo, EndpointBinding, RouteRequest, RouteResponse, SoapRouter, ValidationMiddleware,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{
    ContractId, EndpointCatalog, EndpointMetadata, RequestContract, RequestContractLocation,
    RoutePath,
};
use soaprs_validation::{HttpRequestContractValidator, HttpValidationInput, HttpValidationService};
use tower::ServiceExt;

struct BodyValidator {
    calls: Arc<AtomicUsize>,
}

impl HttpRequestContractValidator for BodyValidator {
    fn validate<'a>(
        &'a self,
        contract: &'a RequestContract,
        input: HttpValidationInput<'a>,
    ) -> BoxFuture<'a, SoapResult<()>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if contract.location == RequestContractLocation::Body && input.body() == b"valid" {
                Ok(())
            } else {
                Err(SoapError::validation("body contract rejected request"))
            }
        })
    }
}

struct Operation {
    calls: Arc<AtomicUsize>,
}

impl UseCase for Operation {
    type Input = ();
    type Output = ();

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn validates_before_route_io_and_short_circuits_rejected_requests() {
    let validation_calls = Arc::new(AtomicUsize::new(0));
    let operation_calls = Arc::new(AtomicUsize::new(0));
    let endpoint = match EndpointMetadata::new(
        "validation.check",
        Method::POST,
        RoutePath::new("/validation")
            .unwrap_or_else(|error| panic!("valid validation path: {error}")),
    ) {
        Ok(endpoint) => endpoint.request_contract(RequestContract::new(
            ContractId::new("validation.body")
                .unwrap_or_else(|error| panic!("valid contract id: {error}")),
            RequestContractLocation::Body,
        )),
        Err(error) => panic!("valid validation endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register validation endpoint: {error}");
    }
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |(), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
    );
    let binding = EndpointBinding::use_case(Arc::new(Operation {
        calls: Arc::clone(&operation_calls),
    }))
    .route_io(route_io);
    let validation = ValidationMiddleware::new(HttpValidationService::new(BodyValidator {
        calls: Arc::clone(&validation_calls),
    }));
    let app = match SoapRouter::builder(catalog)
        .middleware(validation)
        .bind("validation.check", binding)
        .and_then(|builder| builder.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build validation app: {error}"),
    };

    let invalid = match app
        .clone()
        .oneshot(
            Request::post("/validation")
                .body(Body::from("invalid"))
                .unwrap_or_else(|error| panic!("build invalid request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("invalid response: {error}"),
    };
    let valid = match app
        .oneshot(
            Request::post("/validation")
                .body(Body::from("valid"))
                .unwrap_or_else(|error| panic!("build valid request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("valid response: {error}"),
    };

    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(valid.status(), StatusCode::OK);
    assert_eq!(validation_calls.load(Ordering::SeqCst), 2);
    assert_eq!(operation_calls.load(Ordering::SeqCst), 1);
}
