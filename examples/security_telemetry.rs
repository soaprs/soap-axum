//! Application-owned security and telemetry through adapter extension points.

use std::sync::Arc;

use axum::{
    body::Body,
    http::Request,
    middleware::{Next, from_fn},
    response::{IntoResponse, Response},
    routing::options,
};
use http::{
    HeaderName, HeaderValue, Method, StatusCode,
    header::{
        ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        CONTENT_TYPE, ORIGIN,
    },
};
use serde::{Deserialize, Serialize};
use soaprs_axum::{
    EndpointBinding, EndpointHook, EndpointMiddleware, EndpointNext, EndpointOutcome, JsonResponse,
    PluginContext, ResponseView, RouteRequest, RouterPlugin, SoapRouter, TypedJsonRouteIo,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{
    CorsPolicy, CsrfPolicy, EndpointCatalog, EndpointId, EndpointMetadata,
    HttpEnforcementCapability, HttpRequestView, RoutePath,
};

const ENDPOINT_ID: &str = "notes.create";
const ALLOWED_ORIGIN: &str = "http://localhost:3001";
const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");

#[derive(Deserialize)]
struct CreateNoteBody {
    text: String,
}

struct CreateNoteInput {
    text: String,
}

struct CreatedNote {
    id: &'static str,
    text: String,
}

#[derive(Serialize)]
struct CreatedNoteBody {
    id: &'static str,
    text: String,
}

struct CreateNote;

impl UseCase for CreateNote {
    type Input = CreateNoteInput;
    type Output = CreatedNote;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            Ok(CreatedNote {
                id: "note-1",
                text: input.text,
            })
        })
    }
}

struct DemoCsrf;

impl EndpointMiddleware for DemoCsrf {
    fn enforcement_capabilities(&self) -> &'static [HttpEnforcementCapability] {
        &[HttpEnforcementCapability::Csrf]
    }

    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            if request.headers().get(&CSRF_HEADER) != Some(&HeaderValue::from_static("demo")) {
                return EndpointOutcome::failure(SoapError::forbidden());
            }
            next.run(request).await
        })
    }
}

struct ConsoleTelemetry;

impl EndpointHook for ConsoleTelemetry {
    fn on_normalization_rejection(
        &self,
        endpoint: &EndpointMetadata,
        error: &SoapError,
        response: ResponseView<'_>,
    ) {
        eprintln!(
            "endpoint={} normalization_error={:?} status={}",
            endpoint.id,
            error.kind(),
            response.status()
        );
    }

    fn on_response(&self, request: &RouteRequest, response: ResponseView<'_>) {
        println!(
            "endpoint={} status={}",
            request.endpoint().id,
            response.status()
        );
    }
}

struct DemoSecurityPlugin;

impl RouterPlugin for DemoSecurityPlugin {
    fn name(&self) -> &'static str {
        "example-security"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        let endpoint_id = EndpointId::new(ENDPOINT_ID)?;
        let endpoint = context
            .catalog()
            .endpoint(&endpoint_id)
            .ok_or_else(|| SoapError::not_found("security example endpoint is missing"))?;
        if endpoint.cors.is_none() || endpoint.csrf != CsrfPolicy::Required {
            return Err(SoapError::validation(
                "security example requires declared CORS and CSRF policies",
            ));
        }

        context.endpoint_middleware(ENDPOINT_ID, DemoCsrf)?;
        context.hook(ConsoleTelemetry);
        context.router_enforcement_capability(HttpEnforcementCapability::Cors);
        context.augment_router(|router| Ok(router.route("/notes", options(preflight))));
        context.wrap_router(|router| Ok(router.layer(from_fn(cors_guard))));
        Ok(())
    }
}

async fn cors_guard(request: Request<Body>, next: Next) -> Response {
    let origin = request.headers().get(ORIGIN).cloned();
    if origin
        .as_ref()
        .is_some_and(|origin| origin != HeaderValue::from_static(ALLOWED_ORIGIN))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    if origin.is_some() {
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static(ALLOWED_ORIGIN),
        );
    }
    response
}

async fn preflight() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            (
                ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("POST"),
            ),
            (
                ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("content-type, x-csrf-token"),
            ),
        ],
    )
}

fn application() -> SoapResult<axum::Router> {
    let cors = CorsPolicy::exact([ALLOWED_ORIGIN], vec![Method::POST])?
        .allow_headers(vec![CONTENT_TYPE, CSRF_HEADER.clone()]);
    let endpoint = EndpointMetadata::new(ENDPOINT_ID, Method::POST, RoutePath::new("/notes")?)?
        .success_status(StatusCode::CREATED)?
        .cors(cors)
        .require_csrf();
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;

    let route_io = TypedJsonRouteIo::new(
        |_request: &RouteRequest, body: CreateNoteBody| {
            let text = body.text.trim();
            if text.is_empty() {
                return Err(SoapError::validation("note text cannot be empty"));
            }
            Ok(CreateNoteInput {
                text: text.to_owned(),
            })
        },
        |output: CreatedNote, _endpoint: &EndpointMetadata| {
            Ok(JsonResponse::new(CreatedNoteBody {
                id: output.id,
                text: output.text,
            }))
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(CreateNote)).route_io(route_io);
    SoapRouter::builder(catalog)
        .plugin(DemoSecurityPlugin)?
        .bind(ENDPOINT_ID, binding)?
        .build()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = application()?;
    if std::env::args_os().any(|argument| argument == "--check") {
        println!("security and telemetry extension router built successfully");
        return Ok(());
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3002").await?;
    println!("POST http://127.0.0.1:3002/notes with Origin, JSON, and x-csrf-token: demo");
    axum::serve(listener, app).await?;
    Ok(())
}
