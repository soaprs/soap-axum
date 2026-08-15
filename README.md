# soaprs-axum

`soaprs-axum` 0.4 turns a `soaprs-http` `EndpointCatalog` into an Axum 0.8
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
  → Axum response
```

## Binding a use case

```rust
# use std::sync::Arc;
# use soaprs_axum::{EndpointBinding, JsonRouteIo, RouteRequest, RouteResponse};
# use soaprs_core::{BoxFuture, SoapResult, UseCase};
# use soaprs_http::{EndpointMetadata, HttpRequestView};
# use serde::Deserialize;
# struct CreateBody { name: String }
# impl<'de> Deserialize<'de> for CreateBody { fn deserialize<D>(_d: D) -> Result<Self, D::Error> where D: serde::Deserializer<'de> { unimplemented!() } }
# struct CreateInput { name: String, tenant: String }
# struct User;
# struct CreateUser;
# impl UseCase for CreateUser { type Input = CreateInput; type Output = User; fn execute(&self, _input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> { Box::pin(async { Ok(User) }) } }
let route_io = JsonRouteIo::new(
    |request: &RouteRequest, body: CreateBody| {
        Ok(CreateInput {
            name: body.name,
            tenant: request
                .headers()
                .get("x-tenant")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("public")
                .to_owned(),
        })
    },
    |_user: User, _endpoint: &EndpointMetadata| RouteResponse::json(&"created"),
);

let binding = EndpointBinding::use_case(Arc::new(CreateUser)).route_io(route_io);
# let _ = binding;
```

`HttpHandler` is available for operations that genuinely require HTTP context.
For ordinary application operations, direct `UseCase` binding is preferred.

## Extension model

- `EndpointMiddleware` can inspect and enrich normalized requests, short-circuit
  processing, observe errors, and append response effects.
- Middleware registered on `SoapRouterBuilder` is global. Middleware attached to
  `EndpointBinding` runs only for that endpoint.
- `EndpointHook` observes request, outcome, and final response lifecycle events.
- `RouterPlugin` installs middleware and hooks at build time. The adapter does
  not own server startup or shutdown.
- Auth, validation, rate limiting, security, and telemetry implementations live
  in separate packages and plug into these extension points.

Middleware runs in registration order before the endpoint and unwinds in the
opposite order afterwards. Global middleware wraps endpoint-local middleware.

## Optional authentication bridge

The `auth` feature composes the framework-neutral contracts from `soaprs-auth`
and `soaprs-auth-http` as ordinary endpoint middleware:

```toml
soaprs-axum = { version = "0.4", features = ["auth"] }
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

## Deliberate first-slice exclusions

The crate does not implement authentication mechanisms, validation engines,
rate-limit algorithms, CORS/CSRF policy, security headers, telemetry SDKs,
OpenAPI, multipart, streaming bodies, or WebSockets. Optional bridges only
compose contracts owned by the corresponding soaprs packages.
