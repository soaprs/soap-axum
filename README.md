# soaprs-axum

`soaprs-axum` 0.5 turns a `soaprs-http` `EndpointCatalog` into an Axum 0.8
router. It keeps transport mapping at the HTTP boundary and lets application
use cases operate exclusively on typed input and output.

```text
Axum request
  → normalization
  → global and endpoint middleware
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

- `EndpointMiddleware` can inspect and enrich normalized requests, short-circuit
  processing, observe errors, and append response effects.
- Middleware registered on `SoapRouterBuilder` is global. Middleware attached to
  `EndpointBinding` runs only for that endpoint.
- `EndpointHook` observes request, outcome, and final response lifecycle events.
- `EndpointHook::on_normalization_rejection` observes matched requests rejected
  before a complete `RouteRequest` can be created.
- `RouterPlugin` installs middleware and hooks at build time and can transform
  the completed native router for preflight routes or outer telemetry layers.
  The adapter does not own server startup or shutdown.
- Auth, validation, rate limiting, security, and telemetry implementations live
  in separate packages and plug into these extension points.

Middleware declares the portable enforcement it provides. Router construction
fails when endpoint metadata requests authentication, validation, rate limiting,
CORS, or CSRF without a matching provider. CORS must be supplied by a
router-level plugin because endpoint middleware cannot serve unmatched
preflight `OPTIONS` requests. `allow_unenforced(endpoint_id, capability)` is an
explicit escape hatch for metadata enforced outside this router; it is never
applied implicitly.

Middleware runs in registration order before the endpoint and unwinds in the
opposite order afterwards. Global middleware wraps endpoint-local middleware.
An endpoint `timeout` wraps the complete middleware and target invocation; a
deadline is mapped through `SoapError::timeout` and remains observable by
outcome and response hooks. Request-body normalization has its own size cap and
happens before this invocation deadline.

## Optional authentication bridge

The `auth` feature composes the framework-neutral contracts from `soaprs-auth`
and `soaprs-auth-http` as ordinary endpoint middleware:

```toml
soaprs-axum = { version = "0.5", features = ["auth"] }
```

`AuthenticationMiddleware` delegates credential extraction and authentication
to an `HttpAuthenticationService`, enforces the authorization policy declared
in `EndpointMetadata`, and stores a typed `AuthContext<P>` in the normalized
request extensions. `RouteIo` can map that principal into use-case input, so
the use case remains independent of HTTP and Axum.

The default evaluator handles soaprs identity, role, permission, strategy, and
authenticated policies. Applications can provide `HttpAuthorization` when a
named policy needs resource or tenant context. In particular, the adapter does
not implement tokens, cryptography, sessions, user storage, or named-policy
business rules.

The middleware can be registered globally, by a `RouterPlugin`, or on a single
`EndpointBinding`, using the same ordering rules as every other extension.

## Optional capability bridges

The `validation` feature provides `ValidationMiddleware<V>`. It passes the
matched endpoint, normalized `HttpRequestView`, and already-buffered body to a
`soaprs-validation::HttpValidationService`. Contract resolution and the actual
validation engine remain application or provider code.

The `rate-limit` feature provides `RateLimitMiddleware<L>`. It maps an
endpoint's `RateLimitPolicy` to the runtime-neutral `soaprs-rate-limit` port.
Allowed decisions continue the pipeline; rejected decisions become
`SoapError::rate_limited`, HTTP 429, and a validated `Retry-After` response
effect.

`BuiltInRateLimitKeyResolver` handles global and trusted client-IP scopes.
Principal, API-key, and custom scopes require `HttpRateLimitKeyResolver`, which
can inspect application-owned typed request extensions such as `AuthContext`.
Key composition therefore remains explicit and credential secrets never need
to enter the neutral limiter.

```toml
soaprs-axum = {
  version = "0.5",
  features = ["auth", "validation", "rate-limit"]
}
```

The capability crates share the `0.5` contract line with `soaprs` and this
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
```

The default global encoded-body ceiling is 2 MiB. Endpoint `BodyLimitPolicy`
can lower it. Trusted proxy processing and request-ID generation must be
installed explicitly by boundary middleware.

## Vertical-slice acceptance test

`tests/reference_application.rs` composes catalog registration, normalized
path/query/header/body input, `RouteIo`, a pure `UseCase`, authentication,
application-owned validator and limiter implementations through typed plugins,
endpoint middleware, telemetry hooks, response effects, response policies, and
a deadline. Its four requests prove ordered short-circuit behavior for 401,
422, 201, and 429.

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
