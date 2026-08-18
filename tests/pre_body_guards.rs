//! Production pipeline ordering and cancellation guarantees.

use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use axum::body::{Body, Bytes, HttpBody};
use http::{HeaderValue, Method, Request, StatusCode, header::WWW_AUTHENTICATE};
use http_body::Frame;
use soaprs_axum::{
    EmptyRouteIo, EndpointBinding, EndpointGuard, EndpointGuardRejection, EndpointGuardResult,
    EndpointHook, ResponseView, RouteRequest, RouteRequestHead, RouteResponse, SoapRouter,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{EndpointCatalog, EndpointMetadata, RoutePath};
use tower::ServiceExt;

struct PendingBody;

impl HttpBody for PendingBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Pending
    }
}

struct NeverCalled {
    calls: Arc<AtomicUsize>,
}

impl UseCase for NeverCalled {
    type Input = ();
    type Output = ();

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct RejectBeforeBody;

impl EndpointGuard for RejectBeforeBody {
    fn check<'a>(
        &'a self,
        _request: &'a mut RouteRequestHead,
    ) -> BoxFuture<'a, EndpointGuardResult> {
        Box::pin(async {
            let mut rejection = EndpointGuardRejection::new(SoapError::unauthorized());
            rejection.effects_mut().headers.insert(
                WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"pre-body\""),
            );
            Err(rejection)
        })
    }
}

fn application(
    timeout: Option<Duration>,
    guard: bool,
    calls: Arc<AtomicUsize>,
    hook: Option<TimeoutHook>,
) -> SoapResult<axum::Router> {
    let mut endpoint =
        EndpointMetadata::new("pipeline.post", Method::POST, RoutePath::new("/pipeline")?)?;
    if let Some(timeout) = timeout {
        endpoint = endpoint.timeout(timeout)?;
    }
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;
    let binding =
        EndpointBinding::use_case(Arc::new(NeverCalled { calls })).route_io(EmptyRouteIo::new(
            |_request: &RouteRequest| Ok(()),
            |(), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
        ));
    let mut builder = SoapRouter::builder(catalog);
    if guard {
        builder = builder.guard(RejectBeforeBody);
    }
    if let Some(hook) = hook {
        builder = builder.hook(hook);
    }
    builder.bind("pipeline.post", binding)?.build()
}

fn pending_request() -> Request<Body> {
    Request::post("/pipeline")
        .body(Body::new(PendingBody))
        .unwrap_or_else(|error| panic!("valid pending request: {error}"))
}

#[tokio::test]
async fn guard_rejection_does_not_poll_or_buffer_the_request_body() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = application(None, true, Arc::clone(&calls), None)
        .unwrap_or_else(|error| panic!("build guarded app: {error}"));

    let response = tokio::time::timeout(Duration::from_millis(100), app.oneshot(pending_request()))
        .await
        .unwrap_or_else(|_| panic!("guard attempted to read the pending request body"))
        .unwrap_or_else(|error| panic!("guard response: {error}"));

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(WWW_AUTHENTICATE),
        Some(&HeaderValue::from_static("Bearer realm=\"pre-body\""))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[derive(Clone)]
struct TimeoutHook {
    calls: Arc<AtomicUsize>,
}

impl EndpointHook for TimeoutHook {
    fn on_timeout(
        &self,
        _endpoint: &EndpointMetadata,
        _error: &SoapError,
        response: ResponseView<'_>,
    ) {
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn endpoint_timeout_includes_request_body_buffering() {
    let calls = Arc::new(AtomicUsize::new(0));
    let timeout_calls = Arc::new(AtomicUsize::new(0));
    let app = application(
        Some(Duration::from_millis(20)),
        false,
        Arc::clone(&calls),
        Some(TimeoutHook {
            calls: Arc::clone(&timeout_calls),
        }),
    )
    .unwrap_or_else(|error| panic!("build timed app: {error}"));

    let response = tokio::time::timeout(Duration::from_millis(200), app.oneshot(pending_request()))
        .await
        .unwrap_or_else(|_| panic!("endpoint timeout did not cover request body buffering"))
        .unwrap_or_else(|error| panic!("timeout response: {error}"));

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(timeout_calls.load(Ordering::SeqCst), 1);
}
