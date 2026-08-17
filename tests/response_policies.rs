//! Contract tests for translating portable endpoint response policies.

use std::{sync::Arc, time::Duration};

use axum::{body::Body, http::Request};
use http::{
    HeaderName, HeaderValue, Method, StatusCode,
    header::{
        ACCEPT_LANGUAGE, AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY,
        STRICT_TRANSPORT_SECURITY, VARY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
    },
};
use soaprs_axum::{EmptyRouteIo, EndpointBinding, RouteRequest, RouteResponse, SoapRouter};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{
    EndpointCatalog, EndpointMetadata, FrameOptions, HstsPolicy, ReferrerPolicy,
    ResponseCachePolicy, RoutePath, SecurityHeadersPolicy,
};
use tower::ServiceExt;

struct Operation {
    fails: bool,
}

impl UseCase for Operation {
    type Input = ();
    type Output = ();

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            if self.fails {
                Err(SoapError::domain("operation failed"))
            } else {
                Ok(())
            }
        })
    }
}

fn application(
    endpoint: EndpointMetadata,
    fails: bool,
    output_headers: Vec<(HeaderName, HeaderValue)>,
) -> SoapResult<axum::Router> {
    let endpoint_id = endpoint.id.to_string();
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        move |(), _endpoint: &EndpointMetadata| {
            let response = output_headers
                .iter()
                .cloned()
                .fold(RouteResponse::empty(), |response, (name, value)| {
                    response.header(name, value)
                });
            Ok(response)
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(Operation { fails })).route_io(route_io);
    SoapRouter::builder(catalog)
        .bind(&endpoint_id, binding)?
        .build()
}

fn endpoint(id: &str) -> SoapResult<EndpointMetadata> {
    EndpointMetadata::new(id, Method::GET, RoutePath::new(format!("/{id}"))?)
}

#[tokio::test]
async fn secure_defaults_cover_success_application_errors_and_normalization_errors() {
    let success =
        match endpoint("success").and_then(|endpoint| application(endpoint, false, vec![])) {
            Ok(app) => app,
            Err(error) => panic!("build success app: {error}"),
        };
    let failure = match endpoint("failure").and_then(|endpoint| application(endpoint, true, vec![]))
    {
        Ok(app) => app,
        Err(error) => panic!("build failure app: {error}"),
    };
    let success_response = match success
        .clone()
        .oneshot(
            Request::get("/success")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build success request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("success response: {error}"),
    };
    let normalization_response = match success
        .oneshot(
            Request::get("/success")
                .header("cookie", "malformed")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build malformed request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("normalization response: {error}"),
    };
    let failure_response = match failure
        .oneshot(
            Request::get("/failure")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build failure request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("failure response: {error}"),
    };

    assert_eq!(success_response.status(), StatusCode::OK);
    assert_eq!(failure_response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(normalization_response.status(), StatusCode::BAD_REQUEST);
    for response in [success_response, failure_response, normalization_response] {
        assert_eq!(
            response.headers().get(X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert_eq!(
            response.headers().get(X_FRAME_OPTIONS),
            Some(&HeaderValue::from_static("DENY"))
        );
        assert_eq!(
            response.headers().get(REFERRER_POLICY),
            Some(&HeaderValue::from_static("no-referrer"))
        );
    }
}

#[tokio::test]
async fn security_policy_is_authoritative_and_can_be_delegated_to_the_application() {
    let managed = match endpoint("managed").and_then(|endpoint| {
        application(
            endpoint,
            false,
            vec![(
                X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("application-value"),
            )],
        )
    }) {
        Ok(app) => app,
        Err(error) => panic!("build managed app: {error}"),
    };
    let delegated = match endpoint("delegated").and_then(|endpoint| {
        application(
            endpoint.without_security_headers(),
            false,
            vec![(
                X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("application-value"),
            )],
        )
    }) {
        Ok(app) => app,
        Err(error) => panic!("build delegated app: {error}"),
    };
    let managed_response = match managed
        .oneshot(
            Request::get("/managed")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build managed request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("managed response: {error}"),
    };
    let delegated_response = match delegated
        .oneshot(
            Request::get("/delegated")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build delegated request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("delegated response: {error}"),
    };

    assert_eq!(
        managed_response.headers().get(X_CONTENT_TYPE_OPTIONS),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        delegated_response.headers().get(X_CONTENT_TYPE_OPTIONS),
        Some(&HeaderValue::from_static("application-value"))
    );
    assert!(delegated_response.headers().get(X_FRAME_OPTIONS).is_none());
    assert!(delegated_response.headers().get(REFERRER_POLICY).is_none());
}

#[tokio::test]
async fn translates_custom_security_cache_and_vary_policies() {
    let mut security = match SecurityHeadersPolicy::secure_defaults()
        .content_security_policy("default-src 'none'")
    {
        Ok(policy) => policy,
        Err(error) => panic!("valid CSP: {error}"),
    };
    security.frame_options = Some(FrameOptions::SameOrigin);
    security.referrer_policy = Some(ReferrerPolicy::StrictOriginWhenCrossOrigin);
    let hsts = match HstsPolicy::new(Duration::from_secs(31_536_000)) {
        Ok(policy) => policy.include_subdomains().preload(),
        Err(error) => panic!("valid HSTS: {error}"),
    };
    security = security.hsts(hsts);
    let cache = match ResponseCachePolicy::private(Duration::from_secs(60)) {
        Ok(policy) => policy.vary(vec![ACCEPT_LANGUAGE, AUTHORIZATION]),
        Err(error) => panic!("valid cache policy: {error}"),
    };
    let configured = match endpoint("configured")
        .map(|endpoint| endpoint.security_headers(security))
        .and_then(|endpoint| endpoint.response_cache(cache))
        .and_then(|endpoint| {
            application(
                endpoint,
                false,
                vec![
                    (CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                    (
                        VARY,
                        HeaderValue::from_static("accept-encoding, Authorization"),
                    ),
                ],
            )
        }) {
        Ok(app) => app,
        Err(error) => panic!("build configured app: {error}"),
    };
    let response = match configured
        .oneshot(
            Request::get("/configured")
                .body(Body::empty())
                .unwrap_or_else(|error| panic!("build configured request: {error}")),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("configured response: {error}"),
    };

    assert_eq!(
        response.headers().get(X_FRAME_OPTIONS),
        Some(&HeaderValue::from_static("SAMEORIGIN"))
    );
    assert_eq!(
        response.headers().get(REFERRER_POLICY),
        Some(&HeaderValue::from_static("strict-origin-when-cross-origin"))
    );
    assert_eq!(
        response.headers().get(CONTENT_SECURITY_POLICY),
        Some(&HeaderValue::from_static("default-src 'none'"))
    );
    assert_eq!(
        response.headers().get(STRICT_TRANSPORT_SECURITY),
        Some(&HeaderValue::from_static(
            "max-age=31536000; includeSubDomains; preload"
        ))
    );
    assert_eq!(
        response.headers().get(CACHE_CONTROL),
        Some(&HeaderValue::from_static("private, max-age=60"))
    );
    assert_eq!(
        response.headers().get(VARY),
        Some(&HeaderValue::from_static(
            "accept-encoding, Authorization, accept-language"
        ))
    );
    assert_eq!(
        response.headers().get_all(VARY).iter().count(),
        1,
        "Vary values are normalized and deduplicated"
    );
}
