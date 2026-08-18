//! Typed endpoint targets and their type-erased router bindings.

use std::sync::Arc;

use soaprs_core::{BoxFuture, SoapResult, UseCase};

use crate::{
    EndpointGuard, EndpointHook, EndpointMiddleware, EndpointOutcome, RouteIo, RouteRequest,
};

pub(crate) trait EndpointTarget: Send + Sync {
    fn call<'a>(&'a self, request: &'a RouteRequest) -> BoxFuture<'a, EndpointOutcome>;
}

struct UseCaseTarget<U, R> {
    use_case: Arc<U>,
    route_io: R,
}

impl<U, R> EndpointTarget for UseCaseTarget<U, R>
where
    U: UseCase + 'static,
    U::Input: Send + 'static,
    U::Output: Send + 'static,
    R: RouteIo<U::Input, U::Output> + 'static,
{
    fn call<'a>(&'a self, request: &'a RouteRequest) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            let input = match self.route_io.map_request(request) {
                Ok(input) => input,
                Err(error) => return EndpointOutcome::failure(error),
            };
            match self.use_case.execute(input).await {
                Ok(output) => {
                    EndpointOutcome::from_result(self.route_io.map_response_for(output, request))
                }
                Err(error) => EndpointOutcome::failure(error),
            }
        })
    }
}

/// An HTTP-aware typed target for operations that genuinely need transport
/// context in addition to their mapped input.
pub trait HttpHandler: Send + Sync {
    /// Input produced by route I/O.
    type Input: Send;
    /// Successful output consumed by route I/O.
    type Output: Send;

    /// Handles one request after transport data has been mapped to typed input.
    fn handle<'a>(
        &'a self,
        request: &'a RouteRequest,
        input: Self::Input,
    ) -> BoxFuture<'a, SoapResult<Self::Output>>;
}

struct HandlerTarget<H, R> {
    handler: Arc<H>,
    route_io: R,
}

impl<H, R> EndpointTarget for HandlerTarget<H, R>
where
    H: HttpHandler + 'static,
    H::Input: Send + 'static,
    H::Output: Send + 'static,
    R: RouteIo<H::Input, H::Output> + 'static,
{
    fn call<'a>(&'a self, request: &'a RouteRequest) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            let input = match self.route_io.map_request(request) {
                Ok(input) => input,
                Err(error) => return EndpointOutcome::failure(error),
            };
            match self.handler.handle(request, input).await {
                Ok(output) => {
                    EndpointOutcome::from_result(self.route_io.map_response_for(output, request))
                }
                Err(error) => EndpointOutcome::failure(error),
            }
        })
    }
}

/// Type-erased endpoint target plus endpoint-local guards, middleware, and hooks.
pub struct EndpointBinding {
    pub(crate) target: Arc<dyn EndpointTarget>,
    pub(crate) guards: Vec<Arc<dyn EndpointGuard>>,
    pub(crate) middleware: Vec<Arc<dyn EndpointMiddleware>>,
    pub(crate) hooks: Vec<Arc<dyn EndpointHook>>,
}

impl EndpointBinding {
    /// Starts a binding that invokes a transport-independent use case directly.
    pub fn use_case<U>(use_case: Arc<U>) -> UseCaseBinding<U>
    where
        U: UseCase + 'static,
    {
        UseCaseBinding { use_case }
    }

    /// Starts a binding for an explicitly HTTP-aware handler.
    pub fn handler<H>(handler: Arc<H>) -> HandlerBinding<H>
    where
        H: HttpHandler + 'static,
    {
        HandlerBinding { handler }
    }

    /// Appends an endpoint-local admission guard evaluated before body reads.
    #[must_use]
    pub fn guard<G>(mut self, guard: G) -> Self
    where
        G: EndpointGuard + 'static,
    {
        self.guards.push(Arc::new(guard));
        self
    }

    /// Appends endpoint-local middleware.
    #[must_use]
    pub fn middleware<M>(mut self, middleware: M) -> Self
    where
        M: EndpointMiddleware + 'static,
    {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Appends an endpoint-local observational hook.
    #[must_use]
    pub fn hook<H>(mut self, hook: H) -> Self
    where
        H: EndpointHook + 'static,
    {
        self.hooks.push(Arc::new(hook));
        self
    }
}

/// Incomplete use-case binding awaiting its route I/O mapper.
pub struct UseCaseBinding<U> {
    use_case: Arc<U>,
}

impl<U> UseCaseBinding<U>
where
    U: UseCase + 'static,
    U::Input: Send + 'static,
    U::Output: Send + 'static,
{
    /// Completes the binding with request/input and output/response mapping.
    pub fn route_io<R>(self, route_io: R) -> EndpointBinding
    where
        R: RouteIo<U::Input, U::Output> + 'static,
    {
        EndpointBinding {
            target: Arc::new(UseCaseTarget {
                use_case: self.use_case,
                route_io,
            }),
            guards: Vec::new(),
            middleware: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

/// Incomplete HTTP-handler binding awaiting its route I/O mapper.
pub struct HandlerBinding<H> {
    handler: Arc<H>,
}

impl<H> HandlerBinding<H>
where
    H: HttpHandler + 'static,
    H::Input: Send + 'static,
    H::Output: Send + 'static,
{
    /// Completes the binding with request/input and output/response mapping.
    pub fn route_io<R>(self, route_io: R) -> EndpointBinding
    where
        R: RouteIo<H::Input, H::Output> + 'static,
    {
        EndpointBinding {
            target: Arc::new(HandlerTarget {
                handler: self.handler,
                route_io,
            }),
            guards: Vec::new(),
            middleware: Vec::new(),
            hooks: Vec::new(),
        }
    }
}
