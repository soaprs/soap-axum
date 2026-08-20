# soaprs-axum

`soaprs-axum` turns a `soaprs-http` `EndpointCatalog` into an Axum 0.8
router. It keeps transport mapping at the HTTP boundary and lets application
use cases operate exclusively on typed input and output.

```text
Axum request
  → route match and request-head normalization
  → global and endpoint admission guards
  → bounded request-body buffering
  → global and endpoint post-body middleware
  → RouteIo::map_request
  → UseCase::execute
  → RouteIo::map_response
  → HttpResponseEffects
  → endpoint response policies
  → Axum response
```

## Binding a use case

```rust
# use std::sync::Arc;
# use soaprs_axum::{EndpointBinding, JsonResponse, RouteRequest, TypedJsonRouteIo};
# use soaprs_core::{BoxFuture, SoapResult, UseCase};
# use soaprs_http::EndpointMetadata;
# use serde::{Deserialize, Serialize};
# #[derive(Deserialize)] struct CreatePath { account_id: String }
# #[derive(Deserialize)] struct CreateBody { name: String }
# struct CreateInput { account_id: String, name: String, tenant_id: u64 }
# struct User { id: String }
# #[derive(Serialize)] struct CreatedBody { id: String }
# struct CreateUser;
# impl UseCase for CreateUser { type Input = CreateInput; type Output = User; fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> { Box::pin(async { Ok(User { id: "created".to_owned() }) }) } }
let route_io = TypedJsonRouteIo::new(
    |request: &RouteRequest, body: CreateBody| {
        let path: CreatePath = request.decode_path()?;
        Ok(CreateInput {
            account_id: path.account_id,
            name: body.name,
            tenant_id: request.required_header("x-tenant-id")?,
        })
    },
    |user: User, _endpoint: &EndpointMetadata| {
        Ok(JsonResponse::new(CreatedBody { id: user.id }))
    },
);

let binding = EndpointBinding::use_case(Arc::new(CreateUser)).route_io(route_io);
# let _ = binding;
```

`HttpHandler` is available for operations that genuinely require HTTP context.
For ordinary application operations, direct `UseCase` binding is preferred.

## Typed RouteIO and HTTP semantics

`RouteRequest::decode_path`, `decode_query`, `required_header`,
`optional_header`, `header_values`, and `decode_json` keep transport parsing in
the RouteIO mapper. `TypedJsonRouteIo` maps the use-case result into a typed
`JsonResponse<T>`, so serialization and response metadata also remain outside
business logic.

JSON request decoding accepts `application/json` and structured `+json` media
types. Typed JSON responses honor the `Accept` header. Protocol rejections are
mapped before the application error mapper: malformed request data is 400,
unacceptable response media is 406, an oversized encoded body is 413, and an
unsupported request media type is 415. Structurally valid input that does not
match the declared DTO remains a validation error (422).

## Extension model

- `EndpointGuard` receives `RouteRequestHead` after method, URI, path, query,
  headers, and cookies have been normalized, but before the body is polled. It
  is the fail-closed extension point for authentication, authorization, rate
  limiting, CSRF, and cheap request-context enrichment. Every guard runs in
  registration order until the first rejection.
- `EndpointMiddleware` can inspect and enrich normalized requests, short-circuit
  processing, observe errors, and append response effects after the bounded
  body is available. Body-dependent validation belongs in this phase.
- Guards and middleware registered on `SoapRouterBuilder` are global. Those
  attached to `EndpointBinding` run only for that endpoint.
- `EndpointHook` separately observes normalized heads, guard rejection, body
  rejection, post-body request/outcome/response, and whole-request timeout.
- `RouterPlugin` installs guards, middleware, and hooks at build time.
  `augment_router` adds preflight or other framework-level routes first; `wrap_router` then
  applies outer telemetry/policy layers around every catalog and contributed
  route. The adapter does not own server startup or shutdown.
- Auth, validation, rate limiting, security, and telemetry implementations live
  in separate packages and plug into these extension points.

Guards and middleware declare the portable enforcement they provide. Router
construction fails when endpoint metadata requests authentication, validation,
rate limiting, CORS, or CSRF without a matching provider. CORS must be supplied by a
router-level plugin because endpoint extensions cannot serve unmatched
preflight `OPTIONS` requests. Authentication, rate limiting, and CSRF require
a pre-body guard; request validation requires post-body middleware. A provider
in the wrong phase does not satisfy fail-closed coverage.
`allow_unenforced(endpoint_id, capability)` is an
explicit escape hatch for metadata enforced outside this router; it is never
applied implicitly.

Guards run in registration order and cannot skip later guards. Middleware runs
in registration order before the endpoint and unwinds in the
opposite order afterwards. Global middleware wraps endpoint-local middleware.
An endpoint `timeout` covers guard execution, bounded body buffering,
post-body middleware, RouteIO, and target invocation. A deadline is mapped
through `SoapError::timeout` and remains observable by `on_timeout` hooks.

## Optional authentication bridge

The `auth` feature composes the framework-neutral contracts from `soaprs-auth`
and `soaprs-auth-http` as a pre-body admission guard:

```toml
soaprs-axum = { version = "0.6.0", features = ["auth"] }
```

`AuthenticationGuard` delegates credential extraction and authentication
to an `HttpAuthenticationService`, enforces the authorization policy declared
in `EndpointMetadata`, and stores a typed `AuthContext<P>` in request
extensions before the body is read. `RouteIo` can map that principal into
use-case input, so the use case remains independent of HTTP and Axum.

The default evaluator handles soaprs identity, role, permission, strategy, and
authenticated policies. Applications can provide `HttpAuthorization` when a
named policy needs resource or tenant context. In particular, the adapter does
not implement tokens, cryptography, sessions, user storage, or named-policy
business rules.

The guard can be registered globally, by a `RouterPlugin`, or on a single
`EndpointBinding`. Authorization which depends on decoded body data remains an
application/use-case concern and must run before the protected state change.

## Optional capability bridges

The `validation` feature provides `ValidationMiddleware<V>`. It passes the
matched endpoint, normalized `HttpRequestView`, and already-buffered body to a
`soaprs-validation::HttpValidationService`. Contract resolution and the actual
validation engine remain application or provider code.

The `rate-limit` feature provides `RateLimitGuard<L>`. It maps an
endpoint's `RateLimitPolicy` to the runtime-neutral `soaprs-rate-limit` port.
The decision is made before body buffering. Allowed decisions continue the
pipeline; rejected decisions become
`SoapError::rate_limited`, HTTP 429, and a validated `Retry-After` response
effect.

`BuiltInRateLimitKeyResolver` handles global and trusted client-IP scopes.
Principal, API-key, and custom scopes require `HttpRateLimitKeyResolver`, which
can inspect application-owned typed request extensions such as `AuthContext`.
Key composition therefore remains explicit and credential secrets never need
to enter the neutral limiter.

```toml
soaprs-axum = {
  version = "0.6.0",
  features = ["auth", "validation", "rate-limit"]
}
```

The capability crates share the `0.6` contract line with `soaprs` and this
adapter. Sibling path dependencies keep repository development atomic; packaged
releases resolve the same versions from the registry.

## Portable response policies

The adapter translates `SecurityHeadersPolicy` and `ResponseCachePolicy` from
endpoint metadata into final HTTP headers. This is deterministic protocol
mapping, not a security or cache implementation.

Security defaults (`nosniff`, frame denial, and no-referrer) cover successful,
application-error, and request-normalization responses. Declared CSP, HSTS,
frame, referrer, cache-control, and `Vary` values are also projected. Policy
values are applied after route and middleware response effects, so an endpoint
declaration wins on collisions. `without_security_headers()` delegates those
headers entirely to application code or external middleware.

## Running the example

```console
cargo run --example vertical_slice
curl -i -X POST http://127.0.0.1:3000/greetings/pl \
  -H 'content-type: application/json' \
  -d '{"name":"Ada"}'

cargo run --features auth --example auth
curl -i http://127.0.0.1:3001/profile \
  -H 'authorization: Bearer demo-token'

cargo run --example security_telemetry
curl -i -X OPTIONS http://127.0.0.1:3002/notes \
  -H 'origin: http://localhost:3001'
curl -i -X POST http://127.0.0.1:3002/notes \
  -H 'origin: http://localhost:3001' \
  -H 'content-type: application/json' \
  -H 'x-csrf-token: demo' \
  -d '{"text":"separated boundary"}'
```

The production-auth reference composes `soaprs-auth-password` and
`soaprs-auth-jwt` through `soaprs-auth-http` and this adapter. It loads its
HS256 key from the environment, returns short-lived bearer access tokens, and
keeps the rotating opaque refresh token in a `Secure`, `HttpOnly`,
`SameSite=Strict`, `/auth`-scoped cookie:

```console
SOAPRS_JWT_HS256_KEY='replace-with-at-least-32-secret-bytes' \
  cargo run --manifest-path reference/production-auth/Cargo.toml

curl -i -X POST http://127.0.0.1:3001/auth/login \
  -H 'content-type: application/json' \
  -d '{"username":"ada","password":"correct horse battery staple"}'
```

It exposes `POST /auth/login`, `/auth/refresh`, `/auth/logout`,
`/auth/logout-all`, plus `GET /me`, `/reports`, and `/ws-handshake`. The last
route demonstrates that the same bearer extractor, JWT authenticator, and
pre-body authorization guard can protect a later WebSocket upgrade. The
included in-memory identity and refresh stores are test fixtures, not
production persistence; replace them with application adapters implementing
the documented atomic ports.

`security_telemetry` keeps the example CORS/CSRF enforcement and console
telemetry in application-owned extensions. It demonstrates router-level
preflight/outer-response composition, endpoint-local enforcement, lifecycle
hooks, typed RouteIO, and a transport-independent use case without moving those
capabilities into `soaprs-axum`.

The default global encoded-body ceiling is 2 MiB. Endpoint `BodyLimitPolicy`
can lower it. Trusted proxy processing and request-ID generation must be
installed explicitly at the boundary or by an early guard before a client-IP
rate-limit guard.

## Vertical-slice acceptance test

`tests/reference_application.rs` composes catalog registration, normalized
path/query/header/body input, `RouteIo`, a pure `UseCase`, authentication,
application-owned validator and limiter implementations through typed plugins,
pre-body guards, endpoint middleware, telemetry hooks, response effects,
response policies, and a whole-request deadline. Its four requests prove
ordered short-circuit behavior for 401, 422, 201, and 429.

```console
cargo test --all-features --test reference_application
```

`tests/shared_http_contract.rs` also runs the framework-neutral conformance
suite from `soaprs-contract-tests` against a real Axum `Router`. This keeps the
observable HTTP semantics shared with future adapters while leaving their
framework-specific harness construction local:

```console
cargo test --test shared_http_contract
```

## Deliberate first-slice exclusions

The crate does not implement authentication mechanisms, validation engines,
rate-limit algorithms, CORS/CSRF enforcement, security engines, cache storage,
telemetry SDKs, OpenAPI, multipart, streaming bodies, or WebSockets. Optional
bridges compose contracts owned by the corresponding soaprs packages, while
portable response-policy translation remains adapter boundary work.
