//! Contract tests for the optional `soaprs-auth-http` composition bridge.

#![cfg(feature = "auth")]

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, header::AUTHORIZATION},
};
use http::{HeaderValue, Method, StatusCode};
use soaprs_auth::{
    Authentication, Authenticator, AuthorizationPolicy, Credential, Principal, StandardPrincipal,
};
use soaprs_auth_http::{BearerTokenExtractor, HttpAuthenticationService};
use soaprs_axum::{
    AuthContext, AuthenticationMiddleware, EmptyRouteIo, EndpointBinding, HttpAuthorization,
    RouteRequest, RouteResponse, SoapRouter,
};
use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
use soaprs_http::{AuthChallenge, EndpointCatalog, EndpointMetadata, HttpRequestView, RoutePath};
use tower::ServiceExt;

struct TestAuthenticator;

impl Authenticator<Credential, StandardPrincipal> for TestAuthenticator {
    fn authenticate(
        &self,
        credential: Credential,
    ) -> BoxFuture<'_, SoapResult<Authentication<StandardPrincipal>>> {
        Box::pin(async move {
            let token = credential.secret().expose_secret();
            let principal = match token {
                "valid-token" => StandardPrincipal::new("user-42")?,
                "admin-token" => StandardPrincipal::new("admin-1")?.role("admin")?,
                _ => return Err(SoapError::unauthorized()),
            };
            Authentication::new("jwt", principal)
        })
    }
}

struct EchoPrincipal;

impl UseCase for EchoPrincipal {
    type Input = String;
    type Output = String;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move { Ok(input) })
    }
}

fn auth_middleware()
-> SoapResult<AuthenticationMiddleware<BearerTokenExtractor, TestAuthenticator, StandardPrincipal>>
{
    let extractor = BearerTokenExtractor::new("jwt")?;
    let service = HttpAuthenticationService::new(extractor, TestAuthenticator);
    Ok(AuthenticationMiddleware::new(service)
        .challenge(AuthChallenge::new("Bearer")?.realm("soaprs-api")?))
}

fn application(
    id: &str,
    path: &str,
    policy: AuthorizationPolicy,
    middleware: AuthenticationMiddleware<
        BearerTokenExtractor,
        TestAuthenticator,
        StandardPrincipal,
    >,
) -> SoapResult<axum::Router> {
    let endpoint =
        EndpointMetadata::new(id, Method::GET, RoutePath::new(path)?)?.authorize(policy)?;
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;
    let route_io = EmptyRouteIo::new(
        |request: &RouteRequest| {
            request
                .extensions()
                .get::<AuthContext<StandardPrincipal>>()
                .map(|context| context.principal().principal_id().as_str().to_owned())
                .ok_or_else(SoapError::unauthorized)
        },
        |principal_id: String, _endpoint: &EndpointMetadata| {
            RouteResponse::json(&serde_json::json!({ "principal_id": principal_id }))
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(EchoPrincipal)).route_io(route_io);
    SoapRouter::builder(catalog)
        .middleware(middleware)
        .bind(id, binding)?
        .build()
}

#[tokio::test]
async fn required_auth_exposes_typed_context_and_challenges_missing_credentials() {
    let middleware = match auth_middleware() {
        Ok(middleware) => middleware,
        Err(error) => panic!("valid auth middleware: {error}"),
    };
    let app = match application(
        "users.me",
        "/me",
        AuthorizationPolicy::Authenticated,
        middleware,
    ) {
        Ok(app) => app,
        Err(error) => panic!("build authenticated app: {error}"),
    };
    let valid = match Request::builder()
        .uri("/me")
        .header(AUTHORIZATION, "Bearer valid-token")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid request: {error}"),
    };
    let missing = match Request::builder().uri("/me").body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("missing-credential request: {error}"),
    };
    let valid_response = match app.clone().oneshot(valid).await {
        Ok(response) => response,
        Err(error) => panic!("valid auth response: {error}"),
    };
    let missing_response = match app.oneshot(missing).await {
        Ok(response) => response,
        Err(error) => panic!("missing auth response: {error}"),
    };

    assert_eq!(valid_response.status(), StatusCode::OK);
    let body = match to_bytes(valid_response.into_body(), 4096).await {
        Ok(body) => body,
        Err(error) => panic!("read valid auth body: {error}"),
    };
    assert!(String::from_utf8_lossy(&body).contains("user-42"));

    assert_eq!(missing_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        missing_response.headers().get("www-authenticate"),
        Some(&HeaderValue::from_static("Bearer realm=\"soaprs-api\""))
    );
}

#[tokio::test]
async fn built_in_authorization_enforces_roles_without_challenging_forbidden_requests() {
    let middleware = match auth_middleware() {
        Ok(middleware) => middleware,
        Err(error) => panic!("valid auth middleware: {error}"),
    };
    let policy = match AuthorizationPolicy::any_role(["admin"]) {
        Ok(policy) => policy,
        Err(error) => panic!("valid role policy: {error}"),
    };
    let app = match application("admin.get", "/admin", policy, middleware) {
        Ok(app) => app,
        Err(error) => panic!("build role app: {error}"),
    };
    let user = match Request::builder()
        .uri("/admin")
        .header(AUTHORIZATION, "Bearer valid-token")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid user request: {error}"),
    };
    let admin = match Request::builder()
        .uri("/admin")
        .header(AUTHORIZATION, "Bearer admin-token")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid admin request: {error}"),
    };
    let user_response = match app.clone().oneshot(user).await {
        Ok(response) => response,
        Err(error) => panic!("user response: {error}"),
    };
    let admin_response = match app.oneshot(admin).await {
        Ok(response) => response,
        Err(error) => panic!("admin response: {error}"),
    };
    assert_eq!(user_response.status(), StatusCode::FORBIDDEN);
    assert!(user_response.headers().get("www-authenticate").is_none());
    assert_eq!(admin_response.status(), StatusCode::OK);
}

struct NamedOwnerAuthorization;

impl HttpAuthorization<StandardPrincipal> for NamedOwnerAuthorization {
    fn authorize<'a>(
        &'a self,
        authentication: Option<&'a Authentication<StandardPrincipal>>,
        request: &'a RouteRequest,
    ) -> BoxFuture<'a, SoapResult<()>> {
        Box::pin(async move {
            let authentication = authentication.ok_or_else(SoapError::unauthorized)?;
            let owner = request
                .query_parameters("owner")
                .and_then(|values| values.first());
            if owner
                .is_some_and(|owner| owner == authentication.principal().principal_id().as_str())
            {
                Ok(())
            } else {
                Err(SoapError::forbidden())
            }
        })
    }
}

#[tokio::test]
async fn application_authorization_can_resolve_named_resource_policy() {
    let middleware = match auth_middleware() {
        Ok(middleware) => middleware.authorization(NamedOwnerAuthorization),
        Err(error) => panic!("valid auth middleware: {error}"),
    };
    let policy = match AuthorizationPolicy::named("resource.owner") {
        Ok(policy) => policy,
        Err(error) => panic!("valid named policy: {error}"),
    };
    let app = match application("owner.get", "/owner", policy, middleware) {
        Ok(app) => app,
        Err(error) => panic!("build named-policy app: {error}"),
    };
    let allowed = match Request::builder()
        .uri("/owner?owner=user-42")
        .header(AUTHORIZATION, "Bearer valid-token")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid owner request: {error}"),
    };
    let denied = match Request::builder()
        .uri("/owner?owner=other")
        .header(AUTHORIZATION, "Bearer valid-token")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => panic!("valid non-owner request: {error}"),
    };
    let allowed_response = match app.clone().oneshot(allowed).await {
        Ok(response) => response,
        Err(error) => panic!("allowed owner response: {error}"),
    };
    let denied_response = match app.oneshot(denied).await {
        Ok(response) => response,
        Err(error) => panic!("denied owner response: {error}"),
    };
    assert_eq!(allowed_response.status(), StatusCode::OK);
    assert_eq!(denied_response.status(), StatusCode::FORBIDDEN);
}
