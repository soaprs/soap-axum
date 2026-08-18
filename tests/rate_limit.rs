//! Contract coverage for the optional `soaprs-rate-limit` bridge.

#![cfg(feature = "rate-limit")]

use std::{
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use http::{HeaderValue, Method, StatusCode, header::RETRY_AFTER};
use soaprs_axum::{
    EmptyRouteIo, EndpointBinding, RateLimitGuard, RouteRequest, RouteResponse, SoapRouter,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{EndpointCatalog, EndpointMetadata, RateLimitPolicy, RateLimitScope, RoutePath};
use soaprs_rate_limit::{RateLimitDecision, RateLimitRequest, RateLimitService, RateLimiter};
use tower::ServiceExt;

struct OnceLimiter {
    calls: Arc<AtomicUsize>,
}

impl RateLimiter for OnceLimiter {
    fn check<'a>(
        &'a self,
        request: RateLimitRequest<'a>,
    ) -> BoxFuture<'a, SoapResult<RateLimitDecision>> {
        Box::pin(async move {
            if !request.key().as_str().contains("scope=global") {
                return Err(SoapError::infrastructure(
                    "built-in resolver did not select the global scope",
                ));
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(RateLimitDecision::allowed(Some(0), None))
            } else {
                RateLimitDecision::rejected(Duration::from_millis(1500))
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
async fn maps_a_rejected_global_quota_to_429_and_ceiled_retry_after() {
    let limiter_calls = Arc::new(AtomicUsize::new(0));
    let operation_calls = Arc::new(AtomicUsize::new(0));
    let policy = match RateLimitPolicy::new(NonZeroU32::MIN, Duration::from_secs(60)) {
        Ok(policy) => policy.scope(RateLimitScope::Global),
        Err(error) => panic!("valid rate-limit policy: {error}"),
    };
    let endpoint = match EndpointMetadata::new(
        "rate-limit.check",
        Method::GET,
        RoutePath::new("/rate-limit")
            .unwrap_or_else(|error| panic!("valid rate-limit path: {error}")),
    ) {
        Ok(endpoint) => endpoint.rate_limit(policy),
        Err(error) => panic!("valid rate-limit endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register rate-limit endpoint: {error}");
    }
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |(), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
    );
    let binding = EndpointBinding::use_case(Arc::new(Operation {
        calls: Arc::clone(&operation_calls),
    }))
    .route_io(route_io);
    let guard = RateLimitGuard::new(RateLimitService::new(OnceLimiter {
        calls: Arc::clone(&limiter_calls),
    }));
    let app = match SoapRouter::builder(catalog)
        .guard(guard)
        .bind("rate-limit.check", binding)
        .and_then(|builder| builder.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build rate-limit app: {error}"),
    };

    let first = match app
        .clone()
        .oneshot(
            Request::get("/rate-limit")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build first request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("first response: {error}"),
    };
    let rejected = match app
        .oneshot(
            Request::get("/rate-limit")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build rejected request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("rejected response: {error}"),
    };

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        rejected.headers().get(RETRY_AFTER),
        Some(&HeaderValue::from_static("2"))
    );
    let body = match to_bytes(rejected.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read rejected body: {error}"),
    };
    assert!(String::from_utf8_lossy(&body).contains("\"code\":\"rate_limited\""));
    assert_eq!(limiter_calls.load(Ordering::SeqCst), 2);
    assert_eq!(operation_calls.load(Ordering::SeqCst), 1);
}
