//! Integration coverage for the first catalog-to-use-case vertical slice.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde::{Deserialize, Serialize};
use soaprs_axum::{
    EmptyRouteIo, EndpointBinding, EndpointHook, EndpointMiddleware, EndpointNext, EndpointOutcome,
    HttpHandler, JsonRouteIo, PluginContext, ResponseView, RouteRequest, RouteResponse,
    RouterPlugin, SoapRouter,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{
    EndpointCatalog, EndpointMetadata, HttpErrorBody, HttpErrorMapper, HttpErrorResponse,
    HttpRequestView, HttpResponseEffects, Redirect, ResponseCookie, RoutePath, Uri,
};
use tower::ServiceExt;

#[derive(Debug, Deserialize)]
struct CreateUserBody {
    name: String,
}

#[derive(Debug, PartialEq, Eq)]
struct CreateUserInput {
    user_id: String,
    name: String,
    tags: Vec<String>,
    tenant: String,
    cookie: String,
    repeated_headers: usize,
}

#[derive(Debug, Clone)]
struct User {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct UserBody {
    id: String,
    display_name: String,
}

struct CreateUser {
    seen: Arc<Mutex<Option<CreateUserInput>>>,
}

impl UseCase for CreateUser {
    type Input = CreateUserInput;
    type Output = User;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            let output = User {
                id: input.user_id.clone(),
                name: input.name.clone(),
            };
            if let Ok(mut seen) = self.seen.lock() {
                *seen = Some(input);
            }
            Ok(output)
        })
    }
}

#[derive(Clone)]
struct Tenant(String);

struct RecordingMiddleware {
    name: &'static str,
    events: Arc<Mutex<Vec<String>>>,
    tenant: Option<Tenant>,
}

impl EndpointMiddleware for RecordingMiddleware {
    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            if let Ok(mut events) = self.events.lock() {
                events.push(format!("{}.before", self.name));
            }
            if let Some(tenant) = &self.tenant {
                request.extensions_mut().insert(tenant.clone());
            }
            let mut outcome = next.run(request).await;
            if let Ok(mut events) = self.events.lock() {
                events.push(format!("{}.after", self.name));
            }
            outcome.effects_mut().headers.append(
                http::HeaderName::from_static("x-middleware"),
                HeaderValue::from_static("applied"),
            );
            outcome
        })
    }
}

struct RecordingHook {
    events: Arc<Mutex<Vec<String>>>,
}

impl EndpointHook for RecordingHook {
    fn on_request(&self, _request: &RouteRequest) {
        if let Ok(mut events) = self.events.lock() {
            events.push("hook.request".to_owned());
        }
    }

    fn on_outcome(&self, _request: &RouteRequest, outcome: &EndpointOutcome) {
        if let Ok(mut events) = self.events.lock() {
            events.push(format!("hook.outcome.{}", outcome.error().is_none()));
        }
    }

    fn on_response(&self, _request: &RouteRequest, response: ResponseView<'_>) {
        if let Ok(mut events) = self.events.lock() {
            events.push(format!("hook.response.{}", response.status().as_u16()));
        }
    }
}

struct TestPlugin {
    events: Arc<Mutex<Vec<String>>>,
}

impl RouterPlugin for TestPlugin {
    fn name(&self) -> &'static str {
        "test-plugin"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        context.middleware(RecordingMiddleware {
            name: "global",
            events: Arc::clone(&self.events),
            tenant: Some(Tenant("tenant-7".to_owned())),
        });
        context.hook(RecordingHook {
            events: Arc::clone(&self.events),
        });
        Ok(())
    }
}

fn create_endpoint() -> SoapResult<EndpointMetadata> {
    EndpointMetadata::new(
        "users.create",
        Method::POST,
        RoutePath::new("/users/{user_id}")?,
    )?
    .success_status(StatusCode::CREATED)
}

#[tokio::test]
async fn maps_http_to_pure_use_case_and_output_back_to_http() {
    let seen = Arc::new(Mutex::new(None));
    let events = Arc::new(Mutex::new(Vec::new()));
    let endpoint = match create_endpoint() {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid endpoint fixture: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register endpoint fixture: {error}");
    }

    let route_io = JsonRouteIo::new(
        |request: &RouteRequest, body: CreateUserBody| {
            let tenant = request
                .extensions()
                .get::<Tenant>()
                .ok_or_else(SoapError::unauthorized)?;
            Ok(CreateUserInput {
                user_id: request
                    .path_parameter("user_id")
                    .ok_or_else(|| SoapError::validation("missing user_id"))?
                    .to_owned(),
                name: body.name.trim().to_owned(),
                tags: request.query_parameters("tag").unwrap_or_default().to_vec(),
                tenant: tenant.0.clone(),
                cookie: request.cookie("session").unwrap_or_default().to_owned(),
                repeated_headers: request.headers().get_all("x-trace").iter().count(),
            })
        },
        |user: User, _endpoint: &EndpointMetadata| {
            let effects = HttpResponseEffects::new()
                .header(
                    http::HeaderName::from_static("x-output"),
                    HeaderValue::from_static("mapped"),
                )
                .cookie(ResponseCookie::new("session", "rotated")?)?;
            RouteResponse::json(&UserBody {
                id: user.id,
                display_name: user.name.to_uppercase(),
            })?
            .effects(effects)
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(CreateUser {
        seen: Arc::clone(&seen),
    }))
    .route_io(route_io)
    .middleware(RecordingMiddleware {
        name: "endpoint",
        events: Arc::clone(&events),
        tenant: None,
    });
    let builder = match SoapRouter::builder(catalog).plugin(TestPlugin {
        events: Arc::clone(&events),
    }) {
        Ok(builder) => builder,
        Err(error) => panic!("install test plugin: {error}"),
    };
    let app = match builder
        .bind("users.create", binding)
        .and_then(|value| value.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build router: {error}"),
    };

    let request = Request::builder()
        .method(Method::POST)
        .uri("/users/user%2042?tag=one&tag=two")
        .header("content-type", "application/json")
        .header("cookie", "session=opaque")
        .header("x-trace", "first")
        .header("x-trace", "second")
        .body(Body::from(r#"{"name":" Ada "}"#));
    let Some(request) = request.ok() else {
        panic!("valid HTTP request fixture");
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("Axum response: {error}"),
    };

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers().get("x-output"),
        Some(&HeaderValue::from_static("mapped"))
    );
    assert_eq!(
        response.headers().get("x-middleware"),
        Some(&HeaderValue::from_static("applied"))
    );
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(set_cookie.contains("session=rotated"));
    assert!(set_cookie.contains("Secure"));
    assert!(set_cookie.contains("HttpOnly"));

    let body = match to_bytes(response.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read response body: {error}"),
    };
    let json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(error) => panic!("decode response JSON: {error}"),
    };
    assert_eq!(json["id"], "user 42");
    assert_eq!(json["display_name"], "ADA");

    let seen = match seen.lock() {
        Ok(seen) => seen,
        Err(error) => panic!("use-case input lock: {error}"),
    };
    let Some(input) = seen.as_ref() else {
        panic!("use case received input");
    };
    assert_eq!(input.user_id, "user 42");
    assert_eq!(input.name, "Ada");
    assert_eq!(input.tags, ["one", "two"]);
    assert_eq!(input.tenant, "tenant-7");
    assert_eq!(input.cookie, "opaque");
    assert_eq!(input.repeated_headers, 2);

    let events = match events.lock() {
        Ok(events) => events.clone(),
        Err(error) => panic!("event lock: {error}"),
    };
    assert_eq!(
        events,
        [
            "hook.request",
            "global.before",
            "endpoint.before",
            "endpoint.after",
            "global.after",
            "hook.outcome.true",
            "hook.response.201",
        ]
    );
}

struct FailingUseCase;

impl UseCase for FailingUseCase {
    type Input = ();
    type Output = String;

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async { Err(SoapError::conflict("duplicate user")) })
    }
}

struct CustomMapper;

impl HttpErrorMapper for CustomMapper {
    fn map_error(&self, error: &SoapError) -> HttpErrorResponse {
        let mut headers = HeaderMap::new();
        headers.insert("x-error-mapper", HeaderValue::from_static("custom"));
        HttpErrorResponse {
            status: StatusCode::BAD_REQUEST,
            body: HttpErrorBody {
                code: "mapped_error".to_owned(),
                message: error.message().to_owned(),
                diagnostic_id: None,
            },
            headers,
        }
    }
}

#[tokio::test]
async fn failed_use_case_bypasses_output_mapper() {
    let output_mapper_called = Arc::new(AtomicBool::new(false));
    let marker = Arc::clone(&output_mapper_called);
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        move |_output: String, _endpoint: &EndpointMetadata| {
            marker.store(true, Ordering::SeqCst);
            RouteResponse::json(&serde_json::json!({ "unexpected": true }))
        },
    );
    let endpoint = match EndpointMetadata::new(
        "users.fail",
        Method::GET,
        match RoutePath::new("/fail") {
            Ok(path) => path,
            Err(error) => panic!("valid path: {error}"),
        },
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register endpoint: {error}");
    }
    let binding = EndpointBinding::use_case(Arc::new(FailingUseCase)).route_io(route_io);
    let app = match SoapRouter::builder(catalog)
        .error_mapper(CustomMapper)
        .bind("users.fail", binding)
        .and_then(|value| value.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build router: {error}"),
    };
    let request = match Request::builder().uri("/fail").body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("valid request: {error}"),
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("Axum response: {error}"),
    };

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers().get("x-error-mapper"),
        Some(&HeaderValue::from_static("custom"))
    );
    assert!(!output_mapper_called.load(Ordering::SeqCst));
    let body = match to_bytes(response.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read error body: {error}"),
    };
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("mapped_error"));
    assert!(body.contains("duplicate user"));
}

#[test]
fn build_rejects_missing_unknown_duplicate_bindings_and_plugins() {
    let endpoint = match create_endpoint() {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register endpoint: {error}");
    }
    assert!(SoapRouter::builder(catalog.clone()).build().is_err());

    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |_output: String, _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
    );
    let binding = EndpointBinding::use_case(Arc::new(FailingUseCase)).route_io(route_io);
    assert!(
        SoapRouter::builder(catalog.clone())
            .bind("unknown", binding)
            .is_err()
    );

    let events = Arc::new(Mutex::new(Vec::new()));
    let builder = match SoapRouter::builder(catalog).plugin(TestPlugin {
        events: Arc::clone(&events),
    }) {
        Ok(builder) => builder,
        Err(error) => panic!("first plugin install: {error}"),
    };
    assert!(builder.plugin(TestPlugin { events }).is_err());
}

struct HeaderAwareHandler;

impl HttpHandler for HeaderAwareHandler {
    type Input = String;
    type Output = String;

    fn handle<'a>(
        &'a self,
        request: &'a RouteRequest,
        input: Self::Input,
    ) -> BoxFuture<'a, SoapResult<Self::Output>> {
        Box::pin(async move {
            let prefix = request
                .headers()
                .get("x-prefix")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            Ok(format!("{prefix}{input}"))
        })
    }
}

#[tokio::test]
async fn explicitly_http_aware_handler_can_read_transport_context() {
    let endpoint = match EndpointMetadata::new(
        "echo.get",
        Method::GET,
        match RoutePath::new("/echo/{value}") {
            Ok(path) => path,
            Err(error) => panic!("valid path: {error}"),
        },
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register endpoint: {error}");
    }
    let route_io = EmptyRouteIo::new(
        |request: &RouteRequest| {
            request
                .path_parameter("value")
                .map(str::to_owned)
                .ok_or_else(|| SoapError::validation("missing echo value"))
        },
        |output: String, _endpoint: &EndpointMetadata| RouteResponse::json(&output),
    );
    let binding = EndpointBinding::handler(Arc::new(HeaderAwareHandler)).route_io(route_io);
    let app = match SoapRouter::builder(catalog)
        .bind("echo.get", binding)
        .and_then(|builder| builder.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build handler router: {error}"),
    };
    let request = match Request::builder()
        .uri("/echo/world")
        .header("x-prefix", "hello-")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid handler request: {error}"),
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("handler response: {error}"),
    };
    let body = match to_bytes(response.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read handler response: {error}"),
    };
    assert_eq!(body.as_ref(), br#""hello-world""#);
}

struct NeverCalled {
    called: Arc<AtomicBool>,
}

impl UseCase for NeverCalled {
    type Input = ();
    type Output = ();

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        self.called.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct ShortCircuit;

impl EndpointMiddleware for ShortCircuit {
    fn handle<'a>(
        &'a self,
        _request: &'a mut RouteRequest,
        _next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async {
            let response = match RouteResponse::json(&serde_json::json!({ "cached": true })) {
                Ok(response) => response.status(StatusCode::ACCEPTED),
                Err(error) => return EndpointOutcome::failure(error),
            };
            EndpointOutcome::success(response)
        })
    }
}

#[tokio::test]
async fn middleware_can_short_circuit_before_input_mapping_and_use_case() {
    let called = Arc::new(AtomicBool::new(false));
    let endpoint = match EndpointMetadata::new(
        "short.get",
        Method::GET,
        match RoutePath::new("/short") {
            Ok(path) => path,
            Err(error) => panic!("valid path: {error}"),
        },
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register endpoint: {error}");
    }
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |_output: (), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
    );
    let binding = EndpointBinding::use_case(Arc::new(NeverCalled {
        called: Arc::clone(&called),
    }))
    .route_io(route_io)
    .middleware(ShortCircuit);
    let app = match SoapRouter::builder(catalog)
        .bind("short.get", binding)
        .and_then(|builder| builder.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build short-circuit router: {error}"),
    };
    let request = match Request::builder().uri("/short").body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("valid request: {error}"),
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("short-circuit response: {error}"),
    };
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(!called.load(Ordering::SeqCst));
}

struct SuccessfulUnitUseCase;

impl UseCase for SuccessfulUnitUseCase {
    type Input = ();
    type Output = ();

    fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn applies_validated_redirect_response_effects() {
    let endpoint = match EndpointMetadata::new(
        "redirect.get",
        Method::GET,
        match RoutePath::new("/redirect") {
            Ok(path) => path,
            Err(error) => panic!("valid path: {error}"),
        },
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register(endpoint) {
        panic!("register endpoint: {error}");
    }
    let route_io = EmptyRouteIo::new(
        |_request: &RouteRequest| Ok(()),
        |_output: (), _endpoint: &EndpointMetadata| {
            let redirect = Redirect::new(StatusCode::SEE_OTHER, Uri::from_static("/target"))?;
            let effects = HttpResponseEffects::new().redirect(redirect)?;
            RouteResponse::empty().effects(effects)
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(SuccessfulUnitUseCase)).route_io(route_io);
    let app = match SoapRouter::builder(catalog)
        .bind("redirect.get", binding)
        .and_then(|builder| builder.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build redirect router: {error}"),
    };
    let request = match Request::builder().uri("/redirect").body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("valid request: {error}"),
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("redirect response: {error}"),
    };
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers().get(http::header::LOCATION),
        Some(&HeaderValue::from_static("/target"))
    );
}

#[tokio::test]
async fn registers_multiple_catalog_methods_on_one_axum_path() {
    let path = match RoutePath::new("/shared") {
        Ok(path) => path,
        Err(error) => panic!("valid shared path: {error}"),
    };
    let get = match EndpointMetadata::new("shared.get", Method::GET, path.clone()) {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid GET endpoint: {error}"),
    };
    let post = match EndpointMetadata::new("shared.post", Method::POST, path)
        .and_then(|endpoint| endpoint.success_status(StatusCode::CREATED))
    {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("valid POST endpoint: {error}"),
    };
    let mut catalog = EndpointCatalog::new();
    if let Err(error) = catalog.register_all([get, post]) {
        panic!("register shared endpoints: {error}");
    }
    let get_binding =
        EndpointBinding::use_case(Arc::new(SuccessfulUnitUseCase)).route_io(EmptyRouteIo::new(
            |_request: &RouteRequest| Ok(()),
            |_output: (), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
        ));
    let post_binding =
        EndpointBinding::use_case(Arc::new(SuccessfulUnitUseCase)).route_io(EmptyRouteIo::new(
            |_request: &RouteRequest| Ok(()),
            |_output: (), _endpoint: &EndpointMetadata| Ok(RouteResponse::empty()),
        ));
    let app = match SoapRouter::builder(catalog)
        .bind("shared.get", get_binding)
        .and_then(|builder| builder.bind("shared.post", post_binding))
        .and_then(|builder| builder.build())
    {
        Ok(app) => app,
        Err(error) => panic!("build shared-path router: {error}"),
    };
    let get_request = match Request::builder()
        .method(Method::GET)
        .uri("/shared")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid GET request: {error}"),
    };
    let post_request = match Request::builder()
        .method(Method::POST)
        .uri("/shared")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid POST request: {error}"),
    };
    let get_response = match app.clone().oneshot(get_request).await {
        Ok(response) => response,
        Err(error) => panic!("GET response: {error}"),
    };
    let post_response = match app.oneshot(post_request).await {
        Ok(response) => response,
        Err(error) => panic!("POST response: {error}"),
    };
    assert_eq!(get_response.status(), StatusCode::OK);
    assert_eq!(post_response.status(), StatusCode::CREATED);
}
