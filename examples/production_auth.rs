//! Production-authentication reference composition.
//!
//! The example deliberately keeps HTTP concerns in route I/O while password
//! verification, access-token validation, refresh rotation, and revocation use
//! framework-neutral ports. The in-memory identity and refresh stores are
//! fixtures only; replace them with transactional application adapters.

#[cfg(not(feature = "auth"))]
fn main() {
    eprintln!("run with: cargo run --features auth --example production_auth");
}

#[cfg(feature = "auth")]
mod enabled {
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{Duration, SystemTime},
    };

    use http::{Method, StatusCode};
    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey};
    use rand_core::OsRng;
    use serde::{Deserialize, Serialize};
    use soaprs_auth::{
        Authenticator, AuthorizationPolicy, Credential, Principal, SecretString, StandardPrincipal,
        TokenPair, TokenService,
    };
    use soaprs_auth_http::{AuthCookieConfig, BearerTokenExtractor, HttpAuthenticationService};
    use soaprs_auth_jwt::{
        AccessToken, ClaimsMapper, JwtAccessService, JwtAuthenticator, JwtPolicy, JwtTokenService,
        KeySource, MemoryKeySource, MemoryRefreshTokenStore, RefreshToken, RefreshTokenPolicy,
        SigningKey, VerificationKey,
    };
    use soaprs_auth_password::{
        Argon2idPasswordService, ExactIdentifier, NoPepper, NormalizedIdentifier, OsSaltGenerator,
        PasswordAuthenticator, PasswordIdentity, PasswordIdentityRepository, PasswordPolicy,
        TokioPasswordVerifier,
    };
    use soaprs_axum::{
        AuthContext, AuthenticationGuard, EmptyRouteIo, EndpointBinding, JsonRouteIo, RouteRequest,
        RouteResponse, SoapRouter,
    };
    use soaprs_core::{BoxFuture, SoapError, SoapResult, UseCase};
    use soaprs_http::{
        AuthChallenge, EndpointCatalog, EndpointMetadata, HttpRequestView, RoutePath, SameSite,
    };

    const REFRESH_COOKIE: &str = "soap_refresh";
    const JWT_ISSUER: &str = "https://auth.example.test";
    const JWT_AUDIENCE: &str = "soaprs-reference-api";

    #[derive(Clone, Serialize, Deserialize)]
    struct Grants {
        roles: Vec<String>,
        permissions: Vec<String>,
    }

    struct GrantMapper;

    impl ClaimsMapper<StandardPrincipal, Grants> for GrantMapper {
        fn to_claims(&self, principal: &StandardPrincipal) -> SoapResult<Grants> {
            Ok(Grants {
                roles: principal.roles().iter().map(ToString::to_string).collect(),
                permissions: principal
                    .permissions()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
        }

        fn resolve_principal(
            &self,
            subject: &str,
            grants: Grants,
        ) -> SoapResult<StandardPrincipal> {
            let principal = grants
                .roles
                .into_iter()
                .try_fold(StandardPrincipal::new(subject)?, |value, role| {
                    value.role(role)
                })?;
            grants
                .permissions
                .into_iter()
                .try_fold(principal, |value, permission| value.permission(permission))
        }
    }

    struct ExampleIdentities {
        values: HashMap<String, PasswordIdentity<StandardPrincipal>>,
    }

    impl PasswordIdentityRepository<StandardPrincipal> for ExampleIdentities {
        fn find_by_identifier<'a>(
            &'a self,
            identifier: &'a NormalizedIdentifier,
        ) -> BoxFuture<'a, SoapResult<Option<PasswordIdentity<StandardPrincipal>>>> {
            Box::pin(async move { Ok(self.values.get(identifier.as_str()).cloned()) })
        }
    }

    type PasswordPort = Arc<dyn Authenticator<Credential, StandardPrincipal>>;
    type AccessService =
        JwtAccessService<StandardPrincipal, Grants, GrantMapper, fn() -> SystemTime>;
    type Tokens = JwtTokenService<
        StandardPrincipal,
        Grants,
        GrantMapper,
        fn() -> SystemTime,
        OsRng,
        MemoryRefreshTokenStore<StandardPrincipal>,
        fn() -> SystemTime,
    >;

    #[derive(Deserialize)]
    struct LoginBody {
        username: String,
        password: String,
    }

    struct Login {
        passwords: PasswordPort,
        tokens: Arc<Tokens>,
    }

    impl UseCase for Login {
        type Input = LoginBody;
        type Output = TokenPair<AccessToken, RefreshToken>;

        fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
            Box::pin(async move {
                let credential = Credential::password("password", input.username, input.password)
                    .map_err(|_| SoapError::unauthorized())?;
                let authentication = self
                    .passwords
                    .authenticate(credential)
                    .await
                    .map_err(|_| SoapError::unauthorized())?;
                self.tokens.issue(authentication.principal()).await
            })
        }
    }

    struct Refresh {
        tokens: Arc<Tokens>,
    }

    impl UseCase for Refresh {
        type Input = RefreshToken;
        type Output = TokenPair<AccessToken, RefreshToken>;

        fn execute(&self, token: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
            self.tokens.refresh(token)
        }
    }

    struct Logout {
        tokens: Arc<Tokens>,
    }

    impl UseCase for Logout {
        type Input = RefreshToken;
        type Output = ();

        fn execute(&self, token: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
            self.tokens.revoke(token)
        }
    }

    struct LogoutAll {
        tokens: Arc<Tokens>,
    }

    impl UseCase for LogoutAll {
        type Input = StandardPrincipal;
        type Output = ();

        fn execute(&self, principal: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
            Box::pin(async move { self.tokens.revoke_all(&principal).await })
        }
    }

    struct ReadPrincipal;

    impl UseCase for ReadPrincipal {
        type Input = StandardPrincipal;
        type Output = StandardPrincipal;

        fn execute(&self, principal: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
            Box::pin(async move { Ok(principal) })
        }
    }

    #[derive(Serialize)]
    struct AccessBody<'a> {
        access_token: &'a str,
        token_type: &'static str,
        expires_in: u64,
    }

    #[derive(Serialize)]
    struct PrincipalBody {
        subject: String,
        roles: Vec<String>,
        permissions: Vec<String>,
    }

    #[derive(Serialize)]
    struct PermissionBody {
        subject: String,
        report: &'static str,
    }

    #[derive(Serialize)]
    struct HandshakeBody {
        subject: String,
        ready_for_websocket_upgrade: bool,
    }

    fn principal_from(request: &RouteRequest) -> SoapResult<StandardPrincipal> {
        request
            .extensions()
            .get::<AuthContext<StandardPrincipal>>()
            .map(|context| context.principal().clone())
            .ok_or_else(SoapError::unauthorized)
    }

    fn refresh_from(request: &RouteRequest) -> SoapResult<RefreshToken> {
        request
            .cookie(REFRESH_COOKIE)
            .ok_or_else(SoapError::unauthorized)
            .and_then(RefreshToken::new)
    }

    fn token_response(
        pair: TokenPair<AccessToken, RefreshToken>,
        cookie: &AuthCookieConfig,
    ) -> SoapResult<RouteResponse> {
        let effects = cookie.issue_effects(pair.refresh_token.expose_secret())?;
        RouteResponse::json(&AccessBody {
            access_token: pair.access_token.expose_secret(),
            token_type: "Bearer",
            expires_in: 300,
        })?
        .effects(effects)
    }

    fn register(
        catalog: &mut EndpointCatalog,
        id: &str,
        method: Method,
        path: &str,
        policy: AuthorizationPolicy,
        status: StatusCode,
    ) -> SoapResult<()> {
        catalog.register(
            EndpointMetadata::new(id, method, RoutePath::new(path)?)?
                .authorize(policy)?
                .success_status(status)?,
        )
    }

    fn password_port() -> SoapResult<PasswordPort> {
        let policy = PasswordPolicy::default();
        let hashing = Arc::new(Argon2idPasswordService::new(
            policy.clone(),
            OsSaltGenerator,
            NoPepper,
        ));
        let ada = StandardPrincipal::new("user-ada")?
            .role("admin")?
            .permission("reports:read")?;
        let bob = StandardPrincipal::new("user-bob")?;
        let repository = ExampleIdentities {
            values: HashMap::from([
                (
                    "ada".to_owned(),
                    PasswordIdentity::new(
                        ada,
                        hashing
                            .hash_password(&SecretString::new("correct horse battery staple")?)?,
                    ),
                ),
                (
                    "bob".to_owned(),
                    PasswordIdentity::new(
                        bob,
                        hashing.hash_password(&SecretString::new("bob reference password")?)?,
                    ),
                ),
            ]),
        };
        let dummy_hash = hashing.hash_password(&SecretString::new("dummy reference password")?)?;
        let verifier = Arc::new(TokioPasswordVerifier::new(hashing));
        Ok(Arc::new(PasswordAuthenticator::new(
            "password",
            policy.max_identifier_bytes(),
            Arc::new(repository),
            Arc::new(ExactIdentifier),
            verifier,
            dummy_hash,
        )?))
    }

    fn access_service(key_material: &[u8]) -> SoapResult<Arc<AccessService>> {
        if key_material.len() < 32 {
            return Err(SoapError::validation(
                "SOAPRS_JWT_HS256_KEY must contain at least 32 bytes",
            ));
        }
        let signing = SigningKey::new(
            "reference-2026-01",
            Algorithm::HS256,
            EncodingKey::from_secret(key_material),
        )?;
        let verification = VerificationKey::new(
            "reference-2026-01",
            Algorithm::HS256,
            DecodingKey::from_secret(key_material),
        )?;
        let keys: Arc<dyn KeySource> = Arc::new(MemoryKeySource::new(signing, [verification])?);
        let policy = JwtPolicy::new(
            JWT_ISSUER,
            JWT_AUDIENCE,
            Duration::from_secs(300),
            Duration::from_secs(300),
            Duration::from_secs(30),
            [Algorithm::HS256],
        )?;
        let clock: fn() -> SystemTime = SystemTime::now;
        Ok(Arc::new(JwtAccessService::with_os_rng(
            policy,
            keys,
            Arc::new(GrantMapper),
            clock,
        )))
    }

    pub fn application(key_material: &[u8]) -> SoapResult<axum::Router> {
        let access = access_service(key_material)?;
        let refresh_policy = RefreshTokenPolicy::new(Duration::from_secs(8 * 60 * 60))?;
        let clock: fn() -> SystemTime = SystemTime::now;
        let tokens = Arc::new(JwtTokenService::with_os_rng(
            Arc::clone(&access),
            refresh_policy,
            Arc::new(MemoryRefreshTokenStore::new()),
            clock,
        ));
        let refresh_cookie = AuthCookieConfig::new(REFRESH_COOKIE)?
            .path("/auth")?
            .same_site(SameSite::Strict)?
            .max_age(Duration::from_secs(8 * 60 * 60))?;

        let mut catalog = EndpointCatalog::new();
        register(
            &mut catalog,
            "auth.login",
            Method::POST,
            "/auth/login",
            AuthorizationPolicy::Public,
            StatusCode::OK,
        )?;
        register(
            &mut catalog,
            "auth.refresh",
            Method::POST,
            "/auth/refresh",
            AuthorizationPolicy::Public,
            StatusCode::OK,
        )?;
        register(
            &mut catalog,
            "auth.logout",
            Method::POST,
            "/auth/logout",
            AuthorizationPolicy::Public,
            StatusCode::NO_CONTENT,
        )?;
        register(
            &mut catalog,
            "auth.logout_all",
            Method::POST,
            "/auth/logout-all",
            AuthorizationPolicy::Authenticated,
            StatusCode::NO_CONTENT,
        )?;
        register(
            &mut catalog,
            "users.me",
            Method::GET,
            "/me",
            AuthorizationPolicy::Authenticated,
            StatusCode::OK,
        )?;
        register(
            &mut catalog,
            "reports.read",
            Method::GET,
            "/reports",
            AuthorizationPolicy::all_permissions(["reports:read"])?,
            StatusCode::OK,
        )?;
        register(
            &mut catalog,
            "websocket.handshake",
            Method::GET,
            "/ws-handshake",
            AuthorizationPolicy::Authenticated,
            StatusCode::OK,
        )?;

        let login_cookie = refresh_cookie.clone();
        let login = EndpointBinding::use_case(Arc::new(Login {
            passwords: password_port()?,
            tokens: Arc::clone(&tokens),
        }))
        .route_io(JsonRouteIo::new(
            |_request: &RouteRequest, body: LoginBody| Ok(body),
            move |pair, _endpoint: &EndpointMetadata| token_response(pair, &login_cookie),
        ));

        let rotated_cookie = refresh_cookie.clone();
        let refresh = EndpointBinding::use_case(Arc::new(Refresh {
            tokens: Arc::clone(&tokens),
        }))
        .route_io(EmptyRouteIo::new(
            refresh_from,
            move |pair, _endpoint: &EndpointMetadata| token_response(pair, &rotated_cookie),
        ));

        let logout_cookie = refresh_cookie.clone();
        let logout = EndpointBinding::use_case(Arc::new(Logout {
            tokens: Arc::clone(&tokens),
        }))
        .route_io(EmptyRouteIo::new(
            refresh_from,
            move |(), _endpoint: &EndpointMetadata| {
                RouteResponse::empty().effects(logout_cookie.clear_effects()?)
            },
        ));

        let logout_all_cookie = refresh_cookie;
        let logout_all = EndpointBinding::use_case(Arc::new(LogoutAll {
            tokens: Arc::clone(&tokens),
        }))
        .route_io(EmptyRouteIo::new(
            principal_from,
            move |(), _endpoint: &EndpointMetadata| {
                RouteResponse::empty().effects(logout_all_cookie.clear_effects()?)
            },
        ));

        let me = EndpointBinding::use_case(Arc::new(ReadPrincipal)).route_io(EmptyRouteIo::new(
            principal_from,
            |principal: StandardPrincipal, _endpoint: &EndpointMetadata| {
                RouteResponse::json(&PrincipalBody {
                    subject: principal.principal_id().as_str().to_owned(),
                    roles: principal.roles().iter().map(ToString::to_string).collect(),
                    permissions: principal
                        .permissions()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                })
            },
        ));

        let reports =
            EndpointBinding::use_case(Arc::new(ReadPrincipal)).route_io(EmptyRouteIo::new(
                principal_from,
                |principal: StandardPrincipal, _endpoint: &EndpointMetadata| {
                    RouteResponse::json(&PermissionBody {
                        subject: principal.principal_id().as_str().to_owned(),
                        report: "quarterly",
                    })
                },
            ));

        // A real application replaces this response mapper with Axum's upgrade
        // response. Authentication and authorization have already completed in
        // the same pre-body guard used by ordinary HTTP endpoints.
        let websocket_handshake =
            EndpointBinding::use_case(Arc::new(ReadPrincipal)).route_io(EmptyRouteIo::new(
                principal_from,
                |principal: StandardPrincipal, _endpoint: &EndpointMetadata| {
                    RouteResponse::json(&HandshakeBody {
                        subject: principal.principal_id().as_str().to_owned(),
                        ready_for_websocket_upgrade: true,
                    })
                },
            ));

        let bearer = BearerTokenExtractor::new("jwt")?;
        let authenticator = JwtAuthenticator::new("jwt", access)?;
        let auth = AuthenticationGuard::new(HttpAuthenticationService::new(bearer, authenticator))
            .challenge(AuthChallenge::new("Bearer")?.realm("soaprs-reference-api")?);

        SoapRouter::builder(catalog)
            .guard(auth)
            .bind("auth.login", login)?
            .bind("auth.refresh", refresh)?
            .bind("auth.logout", logout)?
            .bind("auth.logout_all", logout_all)?
            .bind("users.me", me)?
            .bind("reports.read", reports)?
            .bind("websocket.handshake", websocket_handshake)?
            .build()
    }

    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let key = std::env::var("SOAPRS_JWT_HS256_KEY")
            .map_err(|_| "set SOAPRS_JWT_HS256_KEY to at least 32 bytes from a secret manager")?;
        let key = SecretString::new(key)?;
        let app = application(key.expose_secret().as_bytes())?;
        if std::env::args_os().any(|argument| argument == "--check") {
            println!("production auth reference router built successfully");
            return Ok(());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:3001").await?;
        println!("soaprs auth reference listening on http://127.0.0.1:3001");
        axum::serve(listener, app).await?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use axum::{
            body::{Body, to_bytes},
            http::Request,
            response::Response,
        };
        use http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, SET_COOKIE};
        use serde::Deserialize;
        use tower::ServiceExt;

        use super::*;

        const TEST_KEY: &[u8] = b"reference-test-only-key-material-32-bytes";

        #[derive(Deserialize)]
        struct AccessResponse {
            access_token: String,
        }

        async fn send(app: &axum::Router, request: Request<Body>) -> Response {
            match app.clone().oneshot(request).await {
                Ok(response) => response,
                Err(error) => panic!("request failed: {error}"),
            }
        }

        fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
            match Request::builder().method(method).uri(uri).body(body) {
                Ok(request) => request,
                Err(error) => panic!("request fixture: {error}"),
            }
        }

        async fn response_body(response: Response) -> Vec<u8> {
            match to_bytes(response.into_body(), 64 * 1024).await {
                Ok(body) => body.to_vec(),
                Err(error) => panic!("response body: {error}"),
            }
        }

        async fn login(app: &axum::Router, username: &str, password: &str) -> (String, String) {
            let payload = serde_json::json!({"username": username, "password": password});
            let mut request = request(Method::POST, "/auth/login", Body::from(payload.to_string()));
            request.headers_mut().insert(
                CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
            let response = send(app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            let cookie = response
                .headers()
                .get(SET_COOKIE)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| panic!("missing refresh cookie"));
            assert!(cookie.contains("; Secure"));
            assert!(cookie.contains("; HttpOnly"));
            assert!(cookie.contains("; SameSite=Strict"));
            assert!(cookie.contains("; Path=/auth"));
            let cookie_pair = cookie
                .split(';')
                .next()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| panic!("invalid refresh cookie"));
            let body = response_body(response).await;
            let access: AccessResponse = serde_json::from_slice(&body)
                .unwrap_or_else(|error| panic!("access response: {error}"));
            (access.access_token, cookie_pair)
        }

        fn bearer_request(method: Method, uri: &str, token: &str) -> Request<Body> {
            let mut request = request(method, uri, Body::empty());
            let value = http::HeaderValue::from_str(&format!("Bearer {token}"))
                .unwrap_or_else(|error| panic!("bearer header: {error}"));
            request.headers_mut().insert(AUTHORIZATION, value);
            request
        }

        fn cookie_request(uri: &str, cookie: &str) -> Request<Body> {
            let mut request = request(Method::POST, uri, Body::empty());
            let value = http::HeaderValue::from_str(cookie)
                .unwrap_or_else(|error| panic!("cookie header: {error}"));
            request.headers_mut().insert(COOKIE, value);
            request
        }

        fn issued_cookie(response: &Response) -> String {
            response
                .headers()
                .get(SET_COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| panic!("missing issued cookie"))
        }

        #[tokio::test]
        async fn complete_authentication_lifecycle_is_enforced_without_secret_leaks() {
            let app = application(TEST_KEY)
                .unwrap_or_else(|error| panic!("reference application: {error}"));

            let missing = send(&app, request(Method::GET, "/me", Body::empty())).await;
            assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
            assert!(missing.headers().contains_key("www-authenticate"));

            let rejected_password = "definitely-not-the-password";
            let payload =
                serde_json::json!({"username": "missing-user", "password": rejected_password});
            let mut bad_login =
                request(Method::POST, "/auth/login", Body::from(payload.to_string()));
            bad_login.headers_mut().insert(
                CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
            let bad_login = send(&app, bad_login).await;
            assert_eq!(bad_login.status(), StatusCode::UNAUTHORIZED);
            let bad_body = String::from_utf8_lossy(&response_body(bad_login).await).into_owned();
            assert!(!bad_body.contains("missing-user"));
            assert!(!bad_body.contains(rejected_password));

            let (bob_access, _) = login(&app, "bob", "bob reference password").await;
            let forbidden = send(&app, bearer_request(Method::GET, "/reports", &bob_access)).await;
            assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
            assert!(!forbidden.headers().contains_key("www-authenticate"));

            let (ada_access, refresh_cookie) =
                login(&app, "ada", "correct horse battery staple").await;
            for uri in ["/me", "/reports", "/ws-handshake"] {
                let response = send(&app, bearer_request(Method::GET, uri, &ada_access)).await;
                assert_eq!(response.status(), StatusCode::OK, "protected URI {uri}");
            }

            let rotated = send(&app, cookie_request("/auth/refresh", &refresh_cookie)).await;
            assert_eq!(rotated.status(), StatusCode::OK);
            let rotated_cookie = issued_cookie(&rotated);
            assert_ne!(rotated_cookie, refresh_cookie);
            let rotated_body = String::from_utf8_lossy(&response_body(rotated).await).into_owned();
            assert!(!rotated_body.contains(&refresh_cookie));

            let reuse = send(&app, cookie_request("/auth/refresh", &refresh_cookie)).await;
            assert_eq!(reuse.status(), StatusCode::UNAUTHORIZED);
            let family_revoked = send(&app, cookie_request("/auth/refresh", &rotated_cookie)).await;
            assert_eq!(family_revoked.status(), StatusCode::UNAUTHORIZED);

            let (_, logout_cookie) = login(&app, "ada", "correct horse battery staple").await;
            let logout = send(&app, cookie_request("/auth/logout", &logout_cookie)).await;
            assert_eq!(logout.status(), StatusCode::NO_CONTENT);
            assert!(
                logout
                    .headers()
                    .get(SET_COOKIE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(
                        |value| value.contains("Max-Age=0") && value.contains("Path=/auth")
                    )
            );
            let revoked = send(&app, cookie_request("/auth/refresh", &logout_cookie)).await;
            assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

            let (access, all_cookie) = login(&app, "ada", "correct horse battery staple").await;
            let logout_all = send(
                &app,
                bearer_request(Method::POST, "/auth/logout-all", &access),
            )
            .await;
            assert_eq!(logout_all.status(), StatusCode::NO_CONTENT);
            let all_revoked = send(&app, cookie_request("/auth/refresh", &all_cookie)).await;
            assert_eq!(all_revoked.status(), StatusCode::UNAUTHORIZED);
        }
    }
}

#[cfg(feature = "auth")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    enabled::run().await
}
