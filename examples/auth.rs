//! Optional `soaprs-auth-http` bridge without authentication logic in the adapter.

#[cfg(not(feature = "auth"))]
fn main() {
    eprintln!("run this example with: cargo run --features auth --example auth");
}

#[cfg(feature = "auth")]
mod enabled {
    use std::sync::Arc;

    use http::{Method, StatusCode};
    use soaprs_auth::{
        Authentication, Authenticator, AuthorizationPolicy, Credential, Principal,
        StandardPrincipal,
    };
    use soaprs_auth_http::{BearerTokenExtractor, HttpAuthenticationService};
    use soaprs_axum::{
        AuthContext, AuthenticationGuard, EmptyRouteIo, EndpointBinding, RouteRequest,
        RouteResponse, SoapRouter,
    };
    use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
    use soaprs_http::{AuthChallenge, EndpointCatalog, EndpointMetadata, RoutePath};

    struct ExampleAuthenticator;

    impl Authenticator<Credential, StandardPrincipal> for ExampleAuthenticator {
        fn authenticate(
            &self,
            credential: Credential,
        ) -> BoxFuture<'_, SoapResult<Authentication<StandardPrincipal>>> {
            Box::pin(async move {
                if credential.secret().expose_secret() != "demo-token" {
                    return Err(SoapError::unauthorized());
                }
                Authentication::new("example", StandardPrincipal::new("user-42")?)
            })
        }
    }

    struct GetProfile;

    impl UseCase for GetProfile {
        type Input = String;
        type Output = String;

        fn execute(&self, principal_id: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
            Box::pin(async move { Ok(format!("profile for {principal_id}")) })
        }
    }

    fn application() -> SoapResult<axum::Router> {
        let endpoint =
            EndpointMetadata::new("profile.get", Method::GET, RoutePath::new("/profile")?)?
                .authorize(AuthorizationPolicy::Authenticated)?
                .success_status(StatusCode::OK)?;
        let mut catalog = EndpointCatalog::new();
        catalog.register(endpoint)?;

        let route_io = EmptyRouteIo::new(
            |request: &RouteRequest| {
                request
                    .extensions()
                    .get::<AuthContext<StandardPrincipal>>()
                    .map(|auth| auth.principal().principal_id().as_str().to_owned())
                    .ok_or_else(SoapError::unauthorized)
            },
            |profile: String, _endpoint: &EndpointMetadata| {
                RouteResponse::json(&serde_json::json!({ "profile": profile }))
            },
        );
        let binding = EndpointBinding::use_case(Arc::new(GetProfile)).route_io(route_io);

        let extractor = BearerTokenExtractor::new("example")?;
        let service = HttpAuthenticationService::new(extractor, ExampleAuthenticator);
        let auth = AuthenticationGuard::new(service)
            .challenge(AuthChallenge::new("Bearer")?.realm("soaprs-example")?);

        SoapRouter::builder(catalog)
            .guard(auth)
            .bind("profile.get", binding)?
            .build()
    }

    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let app = application()?;
        if std::env::args_os().any(|argument| argument == "--check") {
            println!("authenticated vertical slice router built successfully");
            return Ok(());
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:3001").await?;
        println!("GET http://127.0.0.1:3001/profile with Bearer demo-token");
        axum::serve(listener, app).await?;
        Ok(())
    }
}

#[cfg(feature = "auth")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    enabled::run().await
}
