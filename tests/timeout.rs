//! Contract tests for endpoint deadlines declared in portable metadata.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use http::{Method, StatusCode};
use soaprs_axum::{
    EmptyRouteIo, EndpointBinding, EndpointHook, EndpointOutcome, ResponseView, RouteRequest,
    RouteResponse, SoapRouter,
};
use soaprs_core::{BoxFuture, SoapError, SoapErrorKind, SoapResult, UseCase};
use soaprs_http::{EndpointCatalog, EndpointMetadata, RoutePath};
use tower::ServiceExt;

struct DelayedOperation {
    delay: Duration,
}

impl UseCase for DelayedOperation {
    type Input = ();
    type Output = &'static str;

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            Ok("completed")
        })
    }
}

#[derive(Default)]
struct RecordedLifecycle {
    outcome: Mutex<Vec<Option<SoapErrorKind>>>,
    response: Mutex<Vec<StatusCode>>,
}

struct RecordingHook {
    lifecycle: Arc<RecordedLifecycle>,
}

impl EndpointHook for RecordingHook {
    fn on_timeout(
        &self,
        _endpoint: &EndpointMetadata,
        error: &SoapError,
        response: ResponseView<'_>,
    ) {
        match self.lifecycle.outcome.lock() {
            Ok(mut outcomes) => outcomes.push(Some(error.kind())),
            Err(lock_error) => panic!("outcome lock poisoned: {lock_error}"),
        }
        match self.lifecycle.response.lock() {
            Ok(mut responses) => responses.push(response.status()),
            Err(lock_error) => panic!("response lock poisoned: {lock_error}"),
        }
    }

    fn on_outcome(&self, _request: &RouteRequest, outcome: &EndpointOutcome) {
        let error_kind = outcome.error().map(|error| error.kind());
        match self.lifecycle.outcome.lock() {
            Ok(mut outcomes) => outcomes.push(error_kind),
            Err(error) => panic!("outcome lock poisoned: {error}"),
        }
    }

    fn on_response(&self, _request: &RouteRequest, response: ResponseView<'_>) {
        match self.lifecycle.response.lock() {
            Ok(mut responses) => responses.push(response.status()),
            Err(error) => panic!("response lock poisoned: {error}"),
        }
    }
}

fn application(
    deadline: Duration,
    operation_delay: Duration,
    lifecycle: Arc<RecordedLifecycle>,
) -> SoapResult<axum::Router> {
    let endpoint =
        EndpointMetadata::new("operation.get", Method::GET, RoutePath::new("/operation")?)?
            .timeout(deadline)?;
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |output: &'static str, _endpoint: &EndpointMetadata| RouteResponse::json(&output),
    );
    let binding = EndpointBinding::use_case(Arc::new(DelayedOperation {
        delay: operation_delay,
    }))
    .route_io(route_io)
    .hook(RecordingHook { lifecycle });
    SoapRouter::builder(catalog)
        .bind("operation.get", binding)?
        .build()
}

#[tokio::test]
async fn endpoint_deadline_maps_to_timeout_and_remains_visible_to_hooks() {
    let lifecycle = Arc::new(RecordedLifecycle::default());
    let app = match application(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Arc::clone(&lifecycle),
    ) {
        Ok(app) => app,
        Err(error) => panic!("build timeout app: {error}"),
    };
    let request = match Request::get("/operation").body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("build timeout request: {error}"),
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("timeout response: {error}"),
    };

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = match to_bytes(response.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read timeout response: {error}"),
    };
    assert!(String::from_utf8_lossy(&body).contains("\"code\":\"timeout\""));
    assert_eq!(
        lifecycle
            .outcome
            .lock()
            .map(|outcomes| outcomes.clone())
            .unwrap_or_else(|error| panic!("outcome lock poisoned: {error}")),
        [Some(SoapErrorKind::Timeout)]
    );
    assert_eq!(
        lifecycle
            .response
            .lock()
            .map(|responses| responses.clone())
            .unwrap_or_else(|error| panic!("response lock poisoned: {error}")),
        [StatusCode::GATEWAY_TIMEOUT]
    );
}

#[tokio::test]
async fn operation_completing_before_deadline_returns_its_mapped_response() {
    let lifecycle = Arc::new(RecordedLifecycle::default());
    let app = match application(
        Duration::from_millis(100),
        Duration::from_millis(1),
        Arc::clone(&lifecycle),
    ) {
        Ok(app) => app,
        Err(error) => panic!("build completing app: {error}"),
    };
    let request = match Request::get("/operation").body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("build completing request: {error}"),
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("completing response: {error}"),
    };

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        lifecycle
            .outcome
            .lock()
            .map(|outcomes| outcomes.clone())
            .unwrap_or_else(|error| panic!("outcome lock poisoned: {error}")),
        [None]
    );
    assert_eq!(
        lifecycle
            .response
            .lock()
            .map(|responses| responses.clone())
            .unwrap_or_else(|error| panic!("response lock poisoned: {error}")),
        [StatusCode::OK]
    );
}
