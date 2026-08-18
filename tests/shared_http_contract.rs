//! Shared framework-neutral HTTP adapter contract executed against Axum.

use std::{
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::body::{Body, to_bytes};
use http::{HeaderName, HeaderValue, Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use soaprs_axum::{
    EndpointBinding, EndpointHook, EndpointMiddleware, EndpointNext, EndpointOutcome, JsonResponse,
    ResponseView, RouteRequest, RouteRequestHead, SoapRouter, TypedJsonRouteIo,
};
use soaprs_contract_tests::{
    HttpAdapterHarness, HttpAdapterObservations, verify_http_adapter_contract,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{
    BodyLimitPolicy, EndpointCatalog, EndpointMetadata, HttpRequestView, HttpResponseEffects,
    ResponseCookie, RoutePath,
};
use tower::ServiceExt;

#[derive(Debug, Deserialize)]
struct WidgetPath {
    widget_id: String,
}

#[derive(Debug, Deserialize)]
struct WidgetQuery {
    limit: u16,
}

#[derive(Debug, Deserialize)]
struct WidgetBody {
    name: String,
}

#[derive(Debug)]
struct CreateWidgetInput {
    widget_id: String,
    display_name: String,
    limit: u16,
    tenant_id: u64,
    session: String,
    trace_values: usize,
}

#[derive(Debug)]
struct CreatedWidget {
    id: String,
    display_name: String,
    limit: u16,
    tenant_id: u64,
    session: String,
    trace_values: usize,
}

#[derive(Debug, Serialize)]
struct CreatedWidgetBody {
    id: String,
    display_name: String,
    limit: u16,
    tenant_id: u64,
    session: String,
    trace_values: usize,
}

struct CreateWidget {
    calls: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<String>>>,
}

impl UseCase for CreateWidget {
    type Input = CreateWidgetInput;
    type Output = CreatedWidget;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            record(&self.events, "use_case");
            if input.display_name == "DUPLICATE" {
                return Err(SoapError::conflict("widget already exists")
                    .with_source(std::io::Error::other("database-secret")));
            }
            Ok(CreatedWidget {
                id: input.widget_id,
                display_name: input.display_name,
                limit: input.limit,
                tenant_id: input.tenant_id,
                session: input.session,
                trace_values: input.trace_values,
            })
        })
    }
}

struct RecordingMiddleware {
    name: &'static str,
    events: Arc<Mutex<Vec<String>>>,
    applies_effect: bool,
}

impl EndpointMiddleware for RecordingMiddleware {
    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            record(&self.events, &format!("{}.before", self.name));
            let mut outcome = next.run(request).await;
            record(&self.events, &format!("{}.after", self.name));
            if self.applies_effect {
                outcome.effects_mut().headers.insert(
                    HeaderName::from_static("x-contract-middleware"),
                    HeaderValue::from_static("applied"),
                );
            }
            outcome
        })
    }
}

struct RecordingHook {
    events: Arc<Mutex<Vec<String>>>,
}

impl EndpointHook for RecordingHook {
    fn on_normalization_rejection(
        &self,
        _endpoint: &EndpointMetadata,
        _error: &SoapError,
        _response: ResponseView<'_>,
    ) {
        record(&self.events, "hook.normalization_rejection");
    }

    fn on_body_rejection(
        &self,
        _request: &RouteRequestHead,
        _error: &SoapError,
        _response: ResponseView<'_>,
    ) {
        record(&self.events, "hook.normalization_rejection");
    }

    fn on_request(&self, _request: &RouteRequest) {
        record(&self.events, "hook.request");
    }

    fn on_outcome(&self, _request: &RouteRequest, _outcome: &EndpointOutcome) {
        record(&self.events, "hook.outcome");
    }

    fn on_response(&self, _request: &RouteRequest, _response: ResponseView<'_>) {
        record(&self.events, "hook.response");
    }
}

struct AxumHarness {
    app: axum::Router,
    calls: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<String>>>,
}

impl HttpAdapterHarness for AxumHarness {
    fn execute(&self, request: Request<Vec<u8>>) -> BoxFuture<'_, SoapResult<Response<Vec<u8>>>> {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let request = Request::from_parts(parts, Body::from(body));
            let response = self.app.clone().oneshot(request).await.map_err(|error| {
                SoapError::infrastructure("Axum contract request failed").with_source(error)
            })?;
            let (parts, body) = response.into_parts();
            let body = to_bytes(body, 4096).await.map_err(|error| {
                SoapError::infrastructure("failed to read Axum contract response")
                    .with_source(error)
            })?;
            Ok(Response::from_parts(parts, body.to_vec()))
        })
    }

    fn observations(&self) -> SoapResult<HttpAdapterObservations> {
        let events = self.events.lock().map_err(|error| {
            SoapError::infrastructure(format!("HTTP contract event lock was poisoned: {error}"))
        })?;
        Ok(HttpAdapterObservations::new(
            self.calls.load(Ordering::SeqCst),
            events.clone(),
        ))
    }
}

fn harness() -> SoapResult<AxumHarness> {
    let calls = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let endpoint = EndpointMetadata::new(
        "contract.widgets.create",
        Method::POST,
        RoutePath::new("/contract/widgets/{widget_id}")?,
    )?
    .body_limit(BodyLimitPolicy::new(NonZeroU64::new(128).ok_or_else(
        || SoapError::infrastructure("HTTP contract body limit must be non-zero"),
    )?));
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;

    let route_io = TypedJsonRouteIo::new(
        |request: &RouteRequest, body: WidgetBody| {
            let path: WidgetPath = request.decode_path()?;
            let query: WidgetQuery = request.decode_query()?;
            let tenant_id = request.required_header::<u64>("x-tenant-id")?;
            let traces = request.header_values::<String>("x-trace")?;
            Ok(CreateWidgetInput {
                widget_id: path.widget_id,
                display_name: body.name.trim().to_uppercase(),
                limit: query.limit,
                tenant_id,
                session: request.cookie("session").unwrap_or_default().to_owned(),
                trace_values: traces.len(),
            })
        },
        |output: CreatedWidget, _endpoint: &EndpointMetadata| {
            let effects = HttpResponseEffects::new()
                .status(StatusCode::CREATED)
                .cookie(ResponseCookie::new("contract_session", "rotated")?)?;
            JsonResponse::new(CreatedWidgetBody {
                id: output.id,
                display_name: output.display_name,
                limit: output.limit,
                tenant_id: output.tenant_id,
                session: output.session,
                trace_values: output.trace_values,
            })
            .header(
                HeaderName::from_static("x-contract-output"),
                HeaderValue::from_static("mapped"),
            )
            .effects(effects)
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(CreateWidget {
        calls: Arc::clone(&calls),
        events: Arc::clone(&events),
    }))
    .route_io(route_io)
    .middleware(RecordingMiddleware {
        name: "endpoint",
        events: Arc::clone(&events),
        applies_effect: false,
    });
    let app = SoapRouter::builder(catalog)
        .middleware(RecordingMiddleware {
            name: "global",
            events: Arc::clone(&events),
            applies_effect: true,
        })
        .hook(RecordingHook {
            events: Arc::clone(&events),
        })
        .bind("contract.widgets.create", binding)?
        .build()?;
    Ok(AxumHarness { app, calls, events })
}

fn record(events: &Mutex<Vec<String>>, event: &str) {
    if let Ok(mut events) = events.lock() {
        events.push(event.to_owned());
    }
}

#[tokio::test]
async fn axum_satisfies_the_shared_http_adapter_contract() {
    if let Err(error) = verify_http_adapter_contract(harness).await {
        panic!("shared HTTP adapter contract failed: {error}");
    }
}
