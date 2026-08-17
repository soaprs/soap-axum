//! Typed RouteIO projections and HTTP protocol rejection coverage.

use std::{
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use http::{HeaderName, HeaderValue, Method, StatusCode, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use soaprs_axum::{EndpointBinding, JsonResponse, RouteRequest, SoapRouter, TypedJsonRouteIo};
use soaprs_core::{BoxFuture, SoapResult, UseCase};
use soaprs_http::{BodyLimitPolicy, EndpointCatalog, EndpointMetadata, RoutePath};
use tower::ServiceExt;

#[derive(Debug, Deserialize)]
struct CreateWidgetPath {
    widget_id: String,
}

#[derive(Debug, Deserialize)]
struct CreateWidgetQuery {
    limit: u16,
}

#[derive(Debug, Deserialize)]
struct CreateWidgetBody {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreateWidgetInput {
    widget_id: String,
    limit: u16,
    tenant_id: u64,
    name: String,
}

#[derive(Debug)]
struct CreatedWidget {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct CreatedWidgetBody {
    id: String,
    display_name: String,
}

struct CreateWidget {
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<Option<CreateWidgetInput>>>,
}

impl UseCase for CreateWidget {
    type Input = CreateWidgetInput;
    type Output = CreatedWidget;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut seen) = self.seen.lock() {
                *seen = Some(input.clone());
            }
            Ok(CreatedWidget {
                id: input.widget_id,
                name: input.name,
            })
        })
    }
}

fn application(
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<Option<CreateWidgetInput>>>,
) -> SoapResult<axum::Router> {
    let endpoint = EndpointMetadata::new(
        "widgets.create",
        Method::POST,
        RoutePath::new("/widgets/{widget_id}")?,
    )?
    .body_limit(BodyLimitPolicy::new(NonZeroU64::new(128).ok_or_else(
        || soaprs_core::SoapError::infrastructure("body limit fixture must be non-zero"),
    )?));
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;

    let route_io = TypedJsonRouteIo::new(
        |request: &RouteRequest, body: CreateWidgetBody| {
            let path: CreateWidgetPath = request.decode_path()?;
            let query: CreateWidgetQuery = request.decode_query()?;
            let tenant_id = request.required_header::<u64>("x-tenant-id")?;
            Ok(CreateWidgetInput {
                widget_id: path.widget_id,
                limit: query.limit,
                tenant_id,
                name: body.name.trim().to_owned(),
            })
        },
        |output: CreatedWidget, _endpoint: &EndpointMetadata| {
            Ok(JsonResponse::new(CreatedWidgetBody {
                id: output.id,
                display_name: output.name.to_uppercase(),
            })
            .status(StatusCode::CREATED)
            .header(
                HeaderName::from_static("x-route-io"),
                HeaderValue::from_static("typed"),
            ))
        },
    );
    let binding =
        EndpointBinding::use_case(Arc::new(CreateWidget { calls, seen })).route_io(route_io);
    SoapRouter::builder(catalog)
        .bind("widgets.create", binding)?
        .build()
}

fn json_request(uri: &str, body: impl Into<Body>) -> Request<Body> {
    match Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(CONTENT_TYPE, "application/problem+json; charset=utf-8")
        .header("accept", "application/json")
        .header("x-tenant-id", "42")
        .body(body.into())
    {
        Ok(request) => request,
        Err(error) => panic!("valid request fixture: {error}"),
    }
}

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let body = match to_bytes(response.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read response body: {error}"),
    };
    match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => panic!("decode response JSON: {error}"),
    }
}

async fn assert_rejection(
    app: &axum::Router,
    request: Request<Body>,
    status: StatusCode,
    code: &str,
) {
    let response = match app.clone().oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("Axum response: {error}"),
    };
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/json"))
    );
    let json = response_json(response).await;
    assert_eq!(json["code"], code);
}

#[tokio::test]
async fn typed_route_io_keeps_http_mapping_outside_the_use_case() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(None));
    let app = match application(Arc::clone(&calls), Arc::clone(&seen)) {
        Ok(app) => app,
        Err(error) => panic!("build typed RouteIO fixture: {error}"),
    };

    let response = match app
        .clone()
        .oneshot(json_request(
            "/widgets/widget%207?limit=3",
            r#"{"name":" Ada "}"#,
        ))
        .await
    {
        Ok(response) => response,
        Err(error) => panic!("Axum response: {error}"),
    };
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers().get("x-route-io"),
        Some(&HeaderValue::from_static("typed"))
    );
    let json = response_json(response).await;
    assert_eq!(json["id"], "widget 7");
    assert_eq!(json["display_name"], "ADA");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let seen = match seen.lock() {
        Ok(seen) => seen.clone(),
        Err(error) => panic!("use-case input lock: {error}"),
    };
    assert_eq!(
        seen,
        Some(CreateWidgetInput {
            widget_id: "widget 7".to_owned(),
            limit: 3,
            tenant_id: 42,
            name: "Ada".to_owned(),
        })
    );

    assert_rejection(
        &app,
        json_request("/widgets/w-1?limit=nope", r#"{"name":"Ada"}"#),
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;
    assert_rejection(
        &app,
        json_request("/widgets/w-1?limit=1", r#"{"name":"#),
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;
    assert_rejection(
        &app,
        json_request("/widgets/w-1?limit=1", r#"{"name":123}"#),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_error",
    )
    .await;

    let wrong_content_type = match Request::builder()
        .method(Method::POST)
        .uri("/widgets/w-1?limit=1")
        .header(CONTENT_TYPE, "text/plain")
        .header("accept", "application/json")
        .header("x-tenant-id", "42")
        .body(Body::from(r#"{"name":"Ada"}"#))
    {
        Ok(request) => request,
        Err(error) => panic!("valid content-type fixture: {error}"),
    };
    assert_rejection(
        &app,
        wrong_content_type,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;

    let unacceptable = match Request::builder()
        .method(Method::POST)
        .uri("/widgets/w-1?limit=1")
        .header(CONTENT_TYPE, "application/json")
        .header("accept", "text/plain")
        .header("x-tenant-id", "42")
        .body(Body::from(r#"{"name":"Ada"}"#))
    {
        Ok(request) => request,
        Err(error) => panic!("valid Accept fixture: {error}"),
    };
    assert_rejection(
        &app,
        unacceptable,
        StatusCode::NOT_ACCEPTABLE,
        "not_acceptable",
    )
    .await;

    assert_rejection(
        &app,
        json_request(
            "/widgets/w-1?limit=1",
            format!(r#"{{"name":"{}"}}"#, "x".repeat(200)),
        ),
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
