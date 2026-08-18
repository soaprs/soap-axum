//! Endpoint-catalog registration and Axum dispatch.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    sync::Arc,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::RawPathParams,
    http::{Request, header::COOKIE},
    response::Response,
    routing::{MethodFilter, on},
};
use http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE};
use http_body_util::LengthLimitError;
use percent_encoding::percent_decode_str;
use serde::Serialize;
use soaprs_core::{SoapError, SoapResult};
use soaprs_http::{
    EndpointCatalog, EndpointId, EndpointMetadata, HttpEnforcementCapability, HttpErrorBody,
    HttpErrorMapper, HttpErrorResponse, HttpResponseEffects, ResponseCookie, SameSite,
};

use crate::{
    EndpointBinding, EndpointGuard, EndpointGuardRejection, EndpointHook, EndpointMiddleware,
    EndpointNext, EndpointOutcome, HttpRejection, NormalizedRequestHead, PluginContext,
    ResponseView, RouteRequestHead, RouteResponse, RouterPlugin, plugin::RouterTransform,
    policy::apply_response_policies,
};

const DEFAULT_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Entry point for building an Axum router from a soaprs endpoint catalog.
#[derive(Debug, Clone, Copy, Default)]
pub struct SoapRouter;

impl SoapRouter {
    /// Creates a router builder for one validated endpoint catalog.
    pub fn builder(catalog: EndpointCatalog) -> SoapRouterBuilder {
        SoapRouterBuilder::new(catalog)
    }
}

/// Composition root for catalog bindings, guards, middleware, hooks, and plugins.
pub struct SoapRouterBuilder {
    catalog: EndpointCatalog,
    bindings: HashMap<EndpointId, EndpointBinding>,
    global_guards: Vec<Arc<dyn EndpointGuard>>,
    global_middleware: Vec<Arc<dyn EndpointMiddleware>>,
    global_hooks: Vec<Arc<dyn EndpointHook>>,
    endpoint_guards: HashMap<EndpointId, Vec<Arc<dyn EndpointGuard>>>,
    endpoint_middleware: HashMap<EndpointId, Vec<Arc<dyn EndpointMiddleware>>>,
    endpoint_hooks: HashMap<EndpointId, Vec<Arc<dyn EndpointHook>>>,
    router_augmentations: Vec<RouterTransform>,
    router_wrappers: Vec<RouterTransform>,
    router_enforcement_capabilities: HashSet<HttpEnforcementCapability>,
    allowed_unenforced_capabilities: HashMap<EndpointId, HashSet<HttpEnforcementCapability>>,
    plugins: HashSet<String>,
    error_mapper: Arc<dyn HttpErrorMapper>,
    max_body_bytes: usize,
}

impl SoapRouterBuilder {
    fn new(catalog: EndpointCatalog) -> Self {
        Self {
            catalog,
            bindings: HashMap::new(),
            global_guards: Vec::new(),
            global_middleware: Vec::new(),
            global_hooks: Vec::new(),
            endpoint_guards: HashMap::new(),
            endpoint_middleware: HashMap::new(),
            endpoint_hooks: HashMap::new(),
            router_augmentations: Vec::new(),
            router_wrappers: Vec::new(),
            router_enforcement_capabilities: HashSet::new(),
            allowed_unenforced_capabilities: HashMap::new(),
            plugins: HashSet::new(),
            error_mapper: Arc::new(soaprs_http::DefaultHttpErrorMapper),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    /// Appends an admission guard evaluated before request-body reads.
    #[must_use]
    pub fn guard<G>(mut self, guard: G) -> Self
    where
        G: EndpointGuard + 'static,
    {
        self.global_guards.push(Arc::new(guard));
        self
    }

    /// Binds one declared endpoint identity to a typed target and route mapper.
    pub fn bind(mut self, endpoint_id: &str, binding: EndpointBinding) -> SoapResult<Self> {
        let id = EndpointId::new(endpoint_id)?;
        if self.catalog.endpoint(&id).is_none() {
            return Err(SoapError::not_found(format!(
                "endpoint `{endpoint_id}` is not declared"
            )));
        }
        if self.bindings.insert(id, binding).is_some() {
            return Err(SoapError::conflict(format!(
                "endpoint `{endpoint_id}` already has a binding"
            )));
        }
        Ok(self)
    }

    /// Appends middleware around every endpoint.
    #[must_use]
    pub fn middleware<M>(mut self, middleware: M) -> Self
    where
        M: EndpointMiddleware + 'static,
    {
        self.global_middleware.push(Arc::new(middleware));
        self
    }

    /// Appends an observational hook to every endpoint.
    #[must_use]
    pub fn hook<H>(mut self, hook: H) -> Self
    where
        H: EndpointHook + 'static,
    {
        self.global_hooks.push(Arc::new(hook));
        self
    }

    /// Installs one build-time router plugin.
    pub fn plugin<P>(mut self, plugin: P) -> SoapResult<Self>
    where
        P: RouterPlugin,
    {
        let name = plugin.name();
        if self.plugins.contains(name) {
            return Err(SoapError::conflict(format!(
                "router plugin `{name}` is already installed"
            )));
        }
        let mut context = PluginContext {
            catalog: &self.catalog,
            global_guards: &mut self.global_guards,
            global_middleware: &mut self.global_middleware,
            global_hooks: &mut self.global_hooks,
            endpoint_guards: &mut self.endpoint_guards,
            endpoint_middleware: &mut self.endpoint_middleware,
            endpoint_hooks: &mut self.endpoint_hooks,
            router_augmentations: &mut self.router_augmentations,
            router_wrappers: &mut self.router_wrappers,
            router_enforcement_capabilities: &mut self.router_enforcement_capabilities,
        };
        plugin.install(&mut context)?;
        self.plugins.insert(name.to_owned());
        Ok(self)
    }

    /// Replaces the default safe SOAP error mapper.
    #[must_use]
    pub fn error_mapper<M>(mut self, mapper: M) -> Self
    where
        M: HttpErrorMapper + 'static,
    {
        self.error_mapper = Arc::new(mapper);
        self
    }

    /// Sets the global encoded body safety cap.
    ///
    /// An endpoint body-limit policy may lower this cap. Increasing the cap for
    /// a particular endpoint requires increasing this global ceiling as well.
    pub fn max_body_bytes(mut self, max_body_bytes: usize) -> SoapResult<Self> {
        if max_body_bytes == 0 {
            return Err(SoapError::validation(
                "Axum maximum request body size must be greater than zero",
            ));
        }
        self.max_body_bytes = max_body_bytes;
        Ok(self)
    }

    /// Explicitly allows one declared endpoint policy to remain unenforced.
    ///
    /// This escape hatch is intended for contract metadata used only by
    /// documentation/schema tooling or for enforcement delegated outside this
    /// router. Security-sensitive applications should prefer installing a
    /// provider through a phase-appropriate guard, middleware, or router plugin.
    pub fn allow_unenforced(
        mut self,
        endpoint_id: &str,
        capability: HttpEnforcementCapability,
    ) -> SoapResult<Self> {
        let id = EndpointId::new(endpoint_id)?;
        if self.catalog.endpoint(&id).is_none() {
            return Err(SoapError::not_found(format!(
                "endpoint `{endpoint_id}` is not declared"
            )));
        }
        self.allowed_unenforced_capabilities
            .entry(id)
            .or_default()
            .insert(capability);
        Ok(self)
    }

    /// Validates binding completeness and creates the native Axum router.
    pub fn build(mut self) -> SoapResult<Router> {
        let missing = self
            .catalog
            .endpoints()
            .iter()
            .filter(|endpoint| !self.bindings.contains_key(&endpoint.id))
            .map(|endpoint| endpoint.id.to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(SoapError::validation(format!(
                "missing endpoint bindings: {}",
                missing.join(", ")
            )));
        }

        let mut router = Router::new();
        for endpoint in self.catalog.into_endpoints() {
            let endpoint_id = endpoint.id.clone();
            let Some(binding) = self.bindings.remove(&endpoint_id) else {
                return Err(SoapError::infrastructure(
                    "validated endpoint binding disappeared during router construction",
                ));
            };
            let filter = MethodFilter::try_from(endpoint.method.clone()).map_err(|error| {
                SoapError::unsupported(format!(
                    "Axum does not support endpoint method `{}`",
                    error.method()
                ))
            })?;
            let path = endpoint.path.as_str().to_owned();

            let mut guards = self.global_guards.clone();
            if let Some(installed) = self.endpoint_guards.remove(&endpoint_id) {
                guards.extend(installed);
            }
            guards.extend(binding.guards);

            let mut middleware = self.global_middleware.clone();
            if let Some(installed) = self.endpoint_middleware.remove(&endpoint_id) {
                middleware.extend(installed);
            }
            middleware.extend(binding.middleware);
            validate_enforcement_coverage(
                &endpoint,
                &guards,
                &middleware,
                &self.router_enforcement_capabilities,
                self.allowed_unenforced_capabilities.get(&endpoint_id),
            )?;

            let mut hooks = self.global_hooks.clone();
            if let Some(installed) = self.endpoint_hooks.remove(&endpoint_id) {
                hooks.extend(installed);
            }
            hooks.extend(binding.hooks);

            let runtime = Arc::new(EndpointRuntime {
                endpoint: Arc::new(endpoint),
                target: binding.target,
                guards,
                middleware,
                hooks,
                error_mapper: Arc::clone(&self.error_mapper),
                max_body_bytes: self.max_body_bytes,
            });
            router = router.route(
                &path,
                on(
                    filter,
                    move |params: RawPathParams, request: Request<Body>| {
                        let runtime = Arc::clone(&runtime);
                        async move { runtime.dispatch(params, request).await }
                    },
                ),
            );
        }
        for augmentation in self.router_augmentations {
            router = augmentation(router)?;
        }
        for wrapper in self.router_wrappers {
            router = wrapper(router)?;
        }
        Ok(router)
    }
}

fn validate_enforcement_coverage(
    endpoint: &EndpointMetadata,
    guards: &[Arc<dyn EndpointGuard>],
    middleware: &[Arc<dyn EndpointMiddleware>],
    router_capabilities: &HashSet<HttpEnforcementCapability>,
    allowed_unenforced: Option<&HashSet<HttpEnforcementCapability>>,
) -> SoapResult<()> {
    let missing = endpoint
        .required_enforcement_capabilities()
        .into_iter()
        .filter(|required| {
            if allowed_unenforced.is_some_and(|allowed| allowed.contains(required)) {
                return false;
            }
            let provided_at_router = router_capabilities.contains(required);
            let provided_at_endpoint = match required {
                HttpEnforcementCapability::Authentication
                | HttpEnforcementCapability::RateLimit
                | HttpEnforcementCapability::Csrf => guards
                    .iter()
                    .any(|guard| guard.enforcement_capabilities().contains(required)),
                HttpEnforcementCapability::RequestValidation => middleware
                    .iter()
                    .any(|middleware| middleware.enforcement_capabilities().contains(required)),
                HttpEnforcementCapability::Cors => false,
            };
            !provided_at_router && !provided_at_endpoint
        })
        .collect::<Vec<HttpEnforcementCapability>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(SoapError::validation(format!(
        "endpoint `{}` requires missing HTTP enforcement capabilities: {}",
        endpoint.id,
        missing
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

struct EndpointRuntime {
    endpoint: Arc<EndpointMetadata>,
    target: Arc<dyn crate::binding::EndpointTarget>,
    guards: Vec<Arc<dyn EndpointGuard>>,
    middleware: Vec<Arc<dyn EndpointMiddleware>>,
    hooks: Vec<Arc<dyn EndpointHook>>,
    error_mapper: Arc<dyn HttpErrorMapper>,
    max_body_bytes: usize,
}

impl EndpointRuntime {
    async fn dispatch(&self, params: RawPathParams, request: Request<Body>) -> Response {
        let pipeline = self.dispatch_pipeline(params, request);
        if let Some(timeout) = self.endpoint.timeout {
            match tokio::time::timeout(timeout, pipeline).await {
                Ok(response) => response,
                Err(_) => self.timeout_response(),
            }
        } else {
            pipeline.await
        }
    }

    async fn dispatch_pipeline(&self, params: RawPathParams, request: Request<Body>) -> Response {
        let (mut request_head, body) = match self.normalize_head(params, request) {
            Ok(request) => request,
            Err(error) => {
                let response = self.error_response_with_policies(&error);
                let view = ResponseView::new(response.status(), response.headers());
                for hook in &self.hooks {
                    hook.on_normalization_rejection(&self.endpoint, &error, view);
                }
                return response;
            }
        };

        for hook in &self.hooks {
            hook.on_request_head(&request_head);
        }
        for guard in &self.guards {
            if let Err(rejection) = guard.check(&mut request_head).await {
                return self.guard_rejection_response(&request_head, rejection);
            }
        }

        let body = match self.read_body(body).await {
            Ok(body) => body,
            Err(error) => {
                let response = self.error_response_with_policies(&error);
                let view = ResponseView::new(response.status(), response.headers());
                for hook in &self.hooks {
                    hook.on_body_rejection(&request_head, &error, view);
                }
                return response;
            }
        };
        let mut request = request_head.with_body(body);
        for hook in &self.hooks {
            hook.on_request(&request);
        }

        let invocation = EndpointNext {
            middleware: &self.middleware,
            target: self.target.as_ref(),
        }
        .run(&mut request);
        let outcome = invocation.await;

        for hook in &self.hooks {
            hook.on_outcome(&request, &outcome);
        }
        let mut response = outcome_response(
            self.error_mapper.as_ref(),
            self.endpoint.success_status,
            outcome,
        );
        if let Err(error) = apply_response_policies(&self.endpoint, response.headers_mut()) {
            response = error_response(self.error_mapper.as_ref(), &error);
        }
        let view = ResponseView::new(response.status(), response.headers());
        for hook in &self.hooks {
            hook.on_response(&request, view);
        }
        response
    }

    fn normalize_head(
        &self,
        params: RawPathParams,
        request: Request<Body>,
    ) -> SoapResult<(RouteRequestHead, Body)> {
        let (parts, body) = request.into_parts();
        let cookies = parse_cookies(&parts.headers)?;
        let query_parameters = parse_query(parts.uri.query())?;
        let path_parameters = params
            .iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<BTreeMap<_, _>>();
        let normalized = NormalizedRequestHead::new(
            parts.method,
            parts.uri,
            parts.headers,
            cookies,
            path_parameters,
            query_parameters,
        );
        Ok((
            RouteRequestHead::new(Arc::clone(&self.endpoint), normalized, parts.extensions),
            body,
        ))
    }

    async fn read_body(&self, body: Body) -> SoapResult<bytes::Bytes> {
        let endpoint_limit = self
            .endpoint
            .body_limit
            .map(|policy| usize::try_from(policy.max_bytes.get()).unwrap_or(usize::MAX))
            .unwrap_or(self.max_body_bytes);
        let limit = endpoint_limit.min(self.max_body_bytes);
        to_bytes(body, limit).await.map_err(|error| {
            if Error::source(&error).is_some_and(|source| source.is::<LengthLimitError>()) {
                HttpRejection::payload_too_large(format!(
                    "request body exceeds the {limit}-byte limit"
                ))
                .with_source(error)
                .into_error()
            } else {
                SoapError::infrastructure("failed to read HTTP request body").with_source(error)
            }
        })
    }

    fn guard_rejection_response(
        &self,
        request: &RouteRequestHead,
        rejection: EndpointGuardRejection,
    ) -> Response {
        let error = rejection.error();
        let mut response = error_response(self.error_mapper.as_ref(), error);
        if let Err(effect_error) = apply_effects(&mut response, rejection.effects()) {
            response = error_response(self.error_mapper.as_ref(), &effect_error);
        }
        if let Err(policy_error) = apply_response_policies(&self.endpoint, response.headers_mut()) {
            response = error_response(self.error_mapper.as_ref(), &policy_error);
        }
        let view = ResponseView::new(response.status(), response.headers());
        for hook in &self.hooks {
            hook.on_guard_rejection(request, error, view);
        }
        response
    }

    fn timeout_response(&self) -> Response {
        let error =
            SoapError::timeout(format!("endpoint `{}` request timed out", self.endpoint.id));
        let response = self.error_response_with_policies(&error);
        let view = ResponseView::new(response.status(), response.headers());
        for hook in &self.hooks {
            hook.on_timeout(&self.endpoint, &error, view);
        }
        response
    }

    fn error_response_with_policies(&self, error: &SoapError) -> Response {
        let mut response = error_response(self.error_mapper.as_ref(), error);
        if let Err(policy_error) = apply_response_policies(&self.endpoint, response.headers_mut()) {
            response = error_response(self.error_mapper.as_ref(), &policy_error);
        }
        response
    }
}

fn parse_query(query: Option<&str>) -> SoapResult<BTreeMap<String, Vec<String>>> {
    let mut parameters = BTreeMap::<String, Vec<String>>::new();
    if let Some(query) = query {
        validate_percent_encoding(query)?;
        percent_decode_str(query).decode_utf8().map_err(|error| {
            HttpRejection::bad_request("query string is not valid UTF-8")
                .with_source(error)
                .into_error()
        })?;
        for (name, value) in form_urlencoded::parse(query.as_bytes()) {
            parameters
                .entry(name.into_owned())
                .or_default()
                .push(value.into_owned());
        }
    }
    Ok(parameters)
}

fn validate_percent_encoding(value: &str) -> SoapResult<()> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && (index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit())
        {
            return Err(HttpRejection::bad_request(
                "query string contains invalid percent encoding",
            )
            .into_error());
        }
        index += if bytes[index] == b'%' { 3 } else { 1 };
    }
    Ok(())
}

fn parse_cookies(headers: &HeaderMap) -> SoapResult<BTreeMap<String, String>> {
    let mut cookies = BTreeMap::new();
    for header in headers.get_all(COOKIE) {
        let header = header.to_str().map_err(|error| {
            HttpRejection::bad_request("Cookie header is not valid visible text")
                .with_source(error)
                .into_error()
        })?;
        for pair in header.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((name, value)) = pair.split_once('=') else {
                return Err(HttpRejection::bad_request("malformed Cookie header").into_error());
            };
            let name = name.trim();
            let Some(value) = valid_cookie_value(value.trim()) else {
                return Err(HttpRejection::bad_request("invalid cookie name or value").into_error());
            };
            if !valid_cookie_name(name) {
                return Err(HttpRejection::bad_request("invalid cookie name or value").into_error());
            }
            if cookies.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err(HttpRejection::bad_request(format!(
                    "duplicate request cookie `{name}`"
                ))
                .into_error());
            }
        }
    }
    Ok(cookies)
}

fn valid_cookie_value(value: &str) -> Option<&str> {
    let value = if value.starts_with('"') || value.ends_with('"') {
        value.strip_prefix('"')?.strip_suffix('"')?
    } else {
        value
    };
    value.bytes().all(cookie_octet).then_some(value)
}

const fn cookie_octet(byte: u8) -> bool {
    matches!(
        byte,
        0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e
    )
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '!' | '#'
                        | '$'
                        | '%'
                        | '&'
                        | '\''
                        | '*'
                        | '+'
                        | '-'
                        | '.'
                        | '^'
                        | '_'
                        | '`'
                        | '|'
                        | '~'
                )
        })
}

fn outcome_response(
    mapper: &dyn HttpErrorMapper,
    success_status: StatusCode,
    outcome: EndpointOutcome,
) -> Response {
    let (result, middleware_effects) = outcome.into_parts();
    if let Err(error) = middleware_effects.validate() {
        return error_response(mapper, &error);
    }
    let mut response = match result {
        Ok(route_response) => match success_response(success_status, route_response) {
            Ok(response) => response,
            Err(error) => return error_response(mapper, &error),
        },
        Err(error) => error_response(mapper, &error),
    };
    if let Err(error) = apply_effects(&mut response, &middleware_effects) {
        return error_response(mapper, &error);
    }
    response
}

fn success_response(
    success_status: StatusCode,
    route_response: RouteResponse,
) -> SoapResult<Response> {
    let (status, headers, body, effects) = route_response.into_parts();
    effects.validate()?;
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status.unwrap_or(success_status);
    *response.headers_mut() = headers;
    apply_effects(&mut response, &effects)?;
    Ok(response)
}

#[derive(Serialize)]
struct SerializableErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    diagnostic_id: Option<&'a str>,
}

fn error_response(mapper: &dyn HttpErrorMapper, error: &SoapError) -> Response {
    let mapped = if let Some(rejection) = HttpRejection::find(error) {
        HttpErrorResponse {
            status: rejection.status(),
            body: HttpErrorBody {
                code: rejection.code().to_owned(),
                message: rejection.message().to_owned(),
                diagnostic_id: error
                    .diagnostic_id()
                    .map(|diagnostic_id| diagnostic_id.as_str().to_owned()),
            },
            headers: HeaderMap::new(),
        }
    } else {
        mapper.map_error(error)
    };
    let body = serialize_error_body(&mapped.body);
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = mapped.status;
    *response.headers_mut() = mapped.headers;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn serialize_error_body(body: &HttpErrorBody) -> Vec<u8> {
    serde_json::to_vec(&SerializableErrorBody {
        code: &body.code,
        message: &body.message,
        diagnostic_id: body.diagnostic_id.as_deref(),
    })
    .unwrap_or_else(|_| b"{\"code\":\"internal_error\",\"message\":\"failed to encode error response\",\"diagnostic_id\":null}".to_vec())
}

fn apply_effects(response: &mut Response, effects: &HttpResponseEffects) -> SoapResult<()> {
    effects.validate()?;
    if let Some(status) = effects.status {
        *response.status_mut() = status;
    }
    replace_headers(response.headers_mut(), &effects.headers);
    for cookie in &effects.cookies {
        let encoded = encode_cookie(cookie)?;
        response.headers_mut().append(SET_COOKIE, encoded);
    }
    Ok(())
}

fn replace_headers(target: &mut HeaderMap, source: &HeaderMap) {
    for name in source.keys() {
        target.remove(name);
        for value in source.get_all(name) {
            target.append(name.clone(), value.clone());
        }
    }
}

fn encode_cookie(cookie: &ResponseCookie) -> SoapResult<HeaderValue> {
    cookie.validate()?;
    let mut encoded = format!("{}={}", cookie.name, cookie.value);
    if let Some(path) = &cookie.path {
        encoded.push_str("; Path=");
        encoded.push_str(path);
    }
    if let Some(domain) = &cookie.domain {
        encoded.push_str("; Domain=");
        encoded.push_str(domain);
    }
    if let Some(max_age) = cookie.max_age {
        encoded.push_str("; Max-Age=");
        encoded.push_str(&max_age.as_secs().to_string());
    }
    if cookie.secure {
        encoded.push_str("; Secure");
    }
    if cookie.http_only {
        encoded.push_str("; HttpOnly");
    }
    if let Some(same_site) = cookie.same_site {
        encoded.push_str("; SameSite=");
        encoded.push_str(match same_site {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        });
    }
    HeaderValue::from_str(&encoded).map_err(|error| {
        SoapError::validation("response cookie cannot be encoded").with_source(error)
    })
}
