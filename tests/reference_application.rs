//! Acceptance test composing the complete first vertical slice.

#![cfg(all(feature = "auth", feature = "rate-limit", feature = "validation"))]

use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, Response},
};
use http::{
    HeaderValue, Method, StatusCode,
    header::{
        AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
    },
};
use serde::{Deserialize, Serialize};
use soaprs_auth::{
    Authentication, Authenticator, AuthorizationPolicy, Credential, Principal, StandardPrincipal,
};
use soaprs_auth_http::{BearerTokenExtractor, HttpAuthenticationService};
use soaprs_axum::{
    AuthContext, AuthenticationGuard, EndpointBinding, EndpointHook, EndpointMiddleware,
    EndpointNext, EndpointOutcome, HttpRateLimitKeyResolver, JsonRouteIo, PluginContext,
    RateLimitGuard, ResponseView, RouteRequest, RouteRequestHead, RouteResponse, RouterPlugin,
    SoapRouter, ValidationMiddleware,
};
use soaprs_core::{BoxFuture, SoapError, SoapErrorKind, SoapResult, UseCase};
use soaprs_http::{
    AuthChallenge, BodyLimitPolicy, ContractId, EndpointCatalog, EndpointId, EndpointMetadata,
    HttpRequestView, HttpResponseEffects, MediaType, RateLimitPolicy, RateLimitScope,
    RequestContract, RequestContractLocation, ResponseCachePolicy, ResponseCookie, RoutePath,
};
use soaprs_rate_limit::{
    RateLimitDecision, RateLimitKey, RateLimitRequest, RateLimitService, RateLimiter,
};
use soaprs_validation::{HttpRequestContractValidator, HttpValidationInput, HttpValidationService};
use tower::ServiceExt;

const ENDPOINT_ID: &str = "users.create.reference";

#[derive(Debug, Deserialize)]
struct CreateUserBody {
    email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreateUserInput {
    principal_id: String,
    tenant: String,
    email: String,
    notify: bool,
    api_version: String,
}

#[derive(Debug)]
struct CreatedUser {
    id: &'static str,
    email: String,
    tenant: String,
}

#[derive(Debug, Serialize)]
struct CreatedUserBody {
    id: &'static str,
    email: String,
    tenant: String,
}

struct CreateUser {
    seen: Arc<Mutex<Vec<CreateUserInput>>>,
}

impl UseCase for CreateUser {
    type Input = CreateUserInput;
    type Output = CreatedUser;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            let output = CreatedUser {
                id: "created-1",
                email: input.email.clone(),
                tenant: input.tenant.clone(),
            };
            match self.seen.lock() {
                Ok(mut seen) => seen.push(input),
                Err(error) => {
                    return Err(SoapError::infrastructure(format!(
                        "reference input lock poisoned: {error}"
                    )));
                }
            }
            Ok(output)
        })
    }
}

struct ReferenceAuthenticator;

impl Authenticator<Credential, StandardPrincipal> for ReferenceAuthenticator {
    fn authenticate(
        &self,
        credential: Credential,
    ) -> BoxFuture<'_, SoapResult<Authentication<StandardPrincipal>>> {
        Box::pin(async move {
            if credential.secret().expose_secret() != "reference-token" {
                return Err(SoapError::unauthorized());
            }
            Authentication::new("reference", StandardPrincipal::new("user-42")?)
        })
    }
}

struct ValidationPlugin {
    calls: Arc<AtomicUsize>,
}

impl RouterPlugin for ValidationPlugin {
    fn name(&self) -> &'static str {
        "reference-validation"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        let endpoint_id = EndpointId::new(ENDPOINT_ID)?;
        let endpoint = context
            .catalog()
            .endpoint(&endpoint_id)
            .ok_or_else(|| SoapError::not_found("reference validation endpoint is missing"))?;
        let locations = endpoint
            .contracts
            .requests()
            .iter()
            .map(|contract| contract.location)
            .collect::<Vec<_>>();
        for required in [
            RequestContractLocation::Body,
            RequestContractLocation::Path,
            RequestContractLocation::Query,
            RequestContractLocation::Headers,
        ] {
            if !locations.contains(&required) {
                return Err(SoapError::validation(format!(
                    "reference endpoint lacks its {required:?} contract"
                )));
            }
        }
        context.endpoint_middleware(
            ENDPOINT_ID,
            ValidationMiddleware::new(HttpValidationService::new(ReferenceValidation {
                calls: Arc::clone(&self.calls),
            })),
        )
    }
}

struct ReferenceValidation {
    calls: Arc<AtomicUsize>,
}

impl HttpRequestContractValidator for ReferenceValidation {
    fn validate<'a>(
        &'a self,
        contract: &'a RequestContract,
        input: HttpValidationInput<'a>,
    ) -> BoxFuture<'a, SoapResult<()>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match contract.location {
                RequestContractLocation::Body => {
                    if input.request().headers().get(CONTENT_TYPE)
                        != Some(&HeaderValue::from_static("application/json"))
                    {
                        return Err(SoapError::validation(
                            "reference body content type is invalid",
                        ));
                    }
                    let body = serde_json::from_slice::<serde_json::Value>(input.body()).map_err(
                        |error| {
                            SoapError::validation("reference body is not valid JSON")
                                .with_source(error)
                        },
                    )?;
                    if !body
                        .get("email")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|email| email.contains('@'))
                    {
                        return Err(SoapError::validation(
                            "reference email contract rejected the request",
                        ));
                    }
                }
                RequestContractLocation::Path => {
                    if input
                        .request()
                        .path_parameter("tenant")
                        .is_none_or(str::is_empty)
                    {
                        return Err(SoapError::validation(
                            "reference tenant contract rejected the request",
                        ));
                    }
                }
                RequestContractLocation::Query => {
                    if input
                        .request()
                        .query_parameters("notify")
                        .is_none_or(|values| values != ["true"])
                    {
                        return Err(SoapError::validation(
                            "reference query contract rejected the request",
                        ));
                    }
                }
                RequestContractLocation::Headers => {
                    if input.request().headers().get("x-api-version")
                        != Some(&HeaderValue::from_static("1"))
                    {
                        return Err(SoapError::validation(
                            "reference header contract rejected the request",
                        ));
                    }
                }
            }
            Ok(())
        })
    }
}

struct RateLimitPlugin {
    attempts: Arc<AtomicUsize>,
}

impl RouterPlugin for RateLimitPlugin {
    fn name(&self) -> &'static str {
        "reference-rate-limit"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        context.endpoint_guard(
            ENDPOINT_ID,
            RateLimitGuard::new(RateLimitService::new(ReferenceRateLimiter {
                attempts: Arc::clone(&self.attempts),
            }))
            .key_resolver(ReferencePrincipalKeyResolver),
        )
    }
}

struct ReferenceRateLimiter {
    attempts: Arc<AtomicUsize>,
}

impl RateLimiter for ReferenceRateLimiter {
    fn check<'a>(
        &'a self,
        request: RateLimitRequest<'a>,
    ) -> BoxFuture<'a, SoapResult<RateLimitDecision>> {
        Box::pin(async move {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= request.rule().limit.get() as usize {
                Ok(RateLimitDecision::allowed(
                    Some(request.rule().limit.get() - attempt as u32),
                    Some(request.rule().period),
                ))
            } else {
                RateLimitDecision::rejected(request.rule().period)
            }
        })
    }
}

struct ReferencePrincipalKeyResolver;

impl HttpRateLimitKeyResolver for ReferencePrincipalKeyResolver {
    fn resolve<'a>(
        &'a self,
        policy: &'a RateLimitPolicy,
        request: &'a RouteRequestHead,
    ) -> BoxFuture<'a, SoapResult<RateLimitKey>> {
        Box::pin(async move {
            if policy.scope != RateLimitScope::Principal {
                return Err(SoapError::validation(
                    "reference resolver requires principal scope",
                ));
            }
            let auth = request
                .extensions()
                .get::<AuthContext<StandardPrincipal>>()
                .ok_or_else(SoapError::unauthorized)?;
            RateLimitKey::new(format!(
                "http:endpoint={}:principal={}",
                request.endpoint().id,
                auth.principal().principal_id()
            ))
        })
    }
}

#[derive(Default)]
struct TelemetryState {
    endpoint_ids: Mutex<Vec<String>>,
    outcomes: Mutex<Vec<Option<SoapErrorKind>>>,
    responses: Mutex<Vec<(StatusCode, bool)>>,
}

struct TelemetryPlugin {
    state: Arc<TelemetryState>,
}

impl RouterPlugin for TelemetryPlugin {
    fn name(&self) -> &'static str {
        "reference-telemetry"
    }

    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()> {
        context.hook(ReferenceTelemetry {
            state: Arc::clone(&self.state),
        });
        Ok(())
    }
}

struct ReferenceTelemetry {
    state: Arc<TelemetryState>,
}

impl EndpointHook for ReferenceTelemetry {
    fn on_request_head(&self, request: &RouteRequestHead) {
        match self.state.endpoint_ids.lock() {
            Ok(mut endpoint_ids) => endpoint_ids.push(request.endpoint().id.to_string()),
            Err(error) => panic!("telemetry request lock poisoned: {error}"),
        }
    }

    fn on_guard_rejection(
        &self,
        _request: &RouteRequestHead,
        error: &SoapError,
        response: ResponseView<'_>,
    ) {
        match self.state.outcomes.lock() {
            Ok(mut outcomes) => outcomes.push(Some(error.kind())),
            Err(lock_error) => panic!("telemetry outcome lock poisoned: {lock_error}"),
        }
        self.record_response(response);
    }

    fn on_outcome(&self, _request: &RouteRequest, outcome: &EndpointOutcome) {
        match self.state.outcomes.lock() {
            Ok(mut outcomes) => outcomes.push(outcome.error().map(|error| error.kind())),
            Err(error) => panic!("telemetry outcome lock poisoned: {error}"),
        }
    }

    fn on_response(&self, _request: &RouteRequest, response: ResponseView<'_>) {
        self.record_response(response);
    }
}

impl ReferenceTelemetry {
    fn record_response(&self, response: ResponseView<'_>) {
        let has_secure_default = response
            .headers()
            .get(X_CONTENT_TYPE_OPTIONS)
            .is_some_and(|value| value == "nosniff");
        match self.state.responses.lock() {
            Ok(mut responses) => responses.push((response.status(), has_secure_default)),
            Err(error) => panic!("telemetry response lock poisoned: {error}"),
        }
    }
}

struct LocalHttpMiddleware {
    calls: Arc<AtomicUsize>,
}

impl EndpointMiddleware for LocalHttpMiddleware {
    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut outcome = next.run(request).await;
            outcome.effects_mut().headers.insert(
                http::HeaderName::from_static("x-local-middleware"),
                HeaderValue::from_static("applied"),
            );
            outcome
        })
    }
}

struct ReferenceApplication {
    router: axum::Router,
    validation_calls: Arc<AtomicUsize>,
    rate_limit_attempts: Arc<AtomicUsize>,
    local_calls: Arc<AtomicUsize>,
    seen_inputs: Arc<Mutex<Vec<CreateUserInput>>>,
    telemetry: Arc<TelemetryState>,
}

fn application() -> SoapResult<ReferenceApplication> {
    let body_limit = NonZeroU64::new(4096)
        .ok_or_else(|| SoapError::infrastructure("reference body limit is zero"))?;
    let cache = ResponseCachePolicy::private(Duration::from_secs(30))?.vary(vec![AUTHORIZATION]);
    let rate_limit = RateLimitPolicy::new(
        NonZeroU32::new(2)
            .ok_or_else(|| SoapError::infrastructure("reference rate limit is zero"))?,
        Duration::from_secs(60),
    )?
    .scope(RateLimitScope::Principal);
    let endpoint = EndpointMetadata::new(
        ENDPOINT_ID,
        Method::POST,
        RoutePath::new("/tenants/{tenant}/users")?,
    )?
    .success_status(StatusCode::CREATED)?
    .authorize(AuthorizationPolicy::Authenticated)?
    .body_limit(BodyLimitPolicy::new(body_limit))
    .rate_limit(rate_limit)
    .timeout(Duration::from_secs(1))?
    .response_cache(cache)?
    .request_contract(
        RequestContract::new(
            ContractId::new("users.create.body")?,
            RequestContractLocation::Body,
        )
        .content_type(MediaType::json()),
    )
    .request_contract(RequestContract::new(
        ContractId::new("users.create.path")?,
        RequestContractLocation::Path,
    ))
    .request_contract(RequestContract::new(
        ContractId::new("users.create.query")?,
        RequestContractLocation::Query,
    ))
    .request_contract(RequestContract::new(
        ContractId::new("users.create.headers")?,
        RequestContractLocation::Headers,
    ));
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;

    let seen_inputs = Arc::new(Mutex::new(Vec::new()));
    let route_io = JsonRouteIo::new(
        |request: &RouteRequest, body: CreateUserBody| {
            let auth = request
                .extensions()
                .get::<AuthContext<StandardPrincipal>>()
                .ok_or_else(SoapError::unauthorized)?;
            Ok(CreateUserInput {
                principal_id: auth.principal().principal_id().as_str().to_owned(),
                tenant: request
                    .path_parameter("tenant")
                    .ok_or_else(|| SoapError::validation("missing tenant"))?
                    .to_owned(),
                email: body.email.trim().to_owned(),
                notify: request
                    .query_parameters("notify")
                    .and_then(|values| values.first())
                    .is_some_and(|value| value == "true"),
                api_version: request
                    .headers()
                    .get("x-api-version")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned(),
            })
        },
        |output: CreatedUser, _endpoint: &EndpointMetadata| {
            let effects = HttpResponseEffects::new()
                .header(
                    http::HeaderName::from_static("x-route-io"),
                    HeaderValue::from_static("mapped"),
                )
                .cookie(ResponseCookie::new("created_user", output.id)?)?;
            RouteResponse::json(&CreatedUserBody {
                id: output.id,
                email: output.email,
                tenant: output.tenant,
            })?
            .effects(effects)
        },
    );
    let local_calls = Arc::new(AtomicUsize::new(0));
    let binding = EndpointBinding::use_case(Arc::new(CreateUser {
        seen: Arc::clone(&seen_inputs),
    }))
    .route_io(route_io)
    .middleware(LocalHttpMiddleware {
        calls: Arc::clone(&local_calls),
    });

    let extractor = BearerTokenExtractor::new("reference")?;
    let auth = AuthenticationGuard::new(HttpAuthenticationService::new(
        extractor,
        ReferenceAuthenticator,
    ))
    .challenge(AuthChallenge::new("Bearer")?.realm("reference-application")?);
    let validation_calls = Arc::new(AtomicUsize::new(0));
    let rate_limit_attempts = Arc::new(AtomicUsize::new(0));
    let telemetry = Arc::new(TelemetryState::default());
    let builder = SoapRouter::builder(catalog)
        .guard(auth)
        .plugin(ValidationPlugin {
            calls: Arc::clone(&validation_calls),
        })?
        .plugin(RateLimitPlugin {
            attempts: Arc::clone(&rate_limit_attempts),
        })?
        .plugin(TelemetryPlugin {
            state: Arc::clone(&telemetry),
        })?;
    let router = builder.bind(ENDPOINT_ID, binding)?.build()?;
    Ok(ReferenceApplication {
        router,
        validation_calls,
        rate_limit_attempts,
        local_calls,
        seen_inputs,
        telemetry,
    })
}

fn request(token: Option<&str>, email: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/tenants/acme/users?notify=true")
        .header(CONTENT_TYPE, "application/json")
        .header("x-api-version", "1");
    if let Some(token) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    match builder.body(Body::from(format!(r#"{{"email":"{email}"}}"#))) {
        Ok(request) => request,
        Err(error) => panic!("build reference request: {error}"),
    }
}

async fn respond(router: &axum::Router, request: Request<Body>) -> Response<Body> {
    match router.clone().oneshot(request).await {
        Ok(response) => response,
        Err(error) => panic!("reference response: {error}"),
    }
}

#[tokio::test]
async fn composes_the_complete_vertical_slice_without_owning_external_capabilities() {
    let application = match application() {
        Ok(application) => application,
        Err(error) => panic!("build reference application: {error}"),
    };

    let unauthorized = respond(&application.router, request(None, "ada@example.test")).await;
    let invalid = respond(
        &application.router,
        request(Some("reference-token"), "invalid-email"),
    )
    .await;
    let created = respond(
        &application.router,
        request(Some("reference-token"), "ada@example.test"),
    )
    .await;
    let limited = respond(
        &application.router,
        request(Some("reference-token"), "ada@example.test"),
    )
    .await;

    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized.headers().get("www-authenticate"),
        Some(&HeaderValue::from_static(
            "Bearer realm=\"reference-application\""
        ))
    );
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        created.headers().get("x-route-io"),
        Some(&HeaderValue::from_static("mapped"))
    );
    assert_eq!(
        created.headers().get("x-local-middleware"),
        Some(&HeaderValue::from_static("applied"))
    );
    assert_eq!(
        created.headers().get(CACHE_CONTROL),
        Some(&HeaderValue::from_static("private, max-age=30"))
    );
    assert!(created.headers().contains_key(SET_COOKIE));
    let created_body = match to_bytes(created.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read created body: {error}"),
    };
    let created_json = match serde_json::from_slice::<serde_json::Value>(&created_body) {
        Ok(body) => body,
        Err(error) => panic!("decode created body: {error}"),
    };
    assert_eq!(created_json["id"], "created-1");
    assert_eq!(created_json["email"], "ada@example.test");
    assert_eq!(created_json["tenant"], "acme");

    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited.headers().get(RETRY_AFTER),
        Some(&HeaderValue::from_static("60"))
    );
    assert_eq!(application.validation_calls.load(Ordering::SeqCst), 5);
    assert_eq!(application.rate_limit_attempts.load(Ordering::SeqCst), 3);
    assert_eq!(application.local_calls.load(Ordering::SeqCst), 1);

    let seen_inputs = match application.seen_inputs.lock() {
        Ok(seen_inputs) => seen_inputs.clone(),
        Err(error) => panic!("seen input lock poisoned: {error}"),
    };
    assert_eq!(
        seen_inputs,
        [CreateUserInput {
            principal_id: "user-42".to_owned(),
            tenant: "acme".to_owned(),
            email: "ada@example.test".to_owned(),
            notify: true,
            api_version: "1".to_owned(),
        }]
    );

    let endpoint_ids = match application.telemetry.endpoint_ids.lock() {
        Ok(endpoint_ids) => endpoint_ids.clone(),
        Err(error) => panic!("telemetry request lock poisoned: {error}"),
    };
    assert_eq!(endpoint_ids, [ENDPOINT_ID; 4]);
    let outcomes = match application.telemetry.outcomes.lock() {
        Ok(outcomes) => outcomes.clone(),
        Err(error) => panic!("telemetry outcome lock poisoned: {error}"),
    };
    assert_eq!(
        outcomes,
        [
            Some(SoapErrorKind::Unauthorized),
            Some(SoapErrorKind::Validation),
            None,
            Some(SoapErrorKind::RateLimited),
        ]
    );
    let responses = match application.telemetry.responses.lock() {
        Ok(responses) => responses.clone(),
        Err(error) => panic!("telemetry response lock poisoned: {error}"),
    };
    assert_eq!(
        responses,
        [
            (StatusCode::UNAUTHORIZED, true),
            (StatusCode::UNPROCESSABLE_ENTITY, true),
            (StatusCode::CREATED, true),
            (StatusCode::TOO_MANY_REQUESTS, true),
        ]
    );
}
