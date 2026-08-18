//! Ordered post-body HTTP middleware around endpoint dispatch.

use std::{fmt, sync::Arc};

use soaprs_core::{BoxFuture, SoapError, SoapResult};
use soaprs_http::{HttpEnforcementCapability, HttpResponseEffects};

use crate::{RouteRequest, RouteResponse, binding::EndpointTarget};

/// Outcome propagated while middleware unwinds.
pub struct EndpointOutcome {
    result: SoapResult<RouteResponse>,
    effects: HttpResponseEffects,
}

impl fmt::Debug for EndpointOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointOutcome")
            .field("is_success", &self.result.is_ok())
            .field("effects", &self.effects)
            .finish_non_exhaustive()
    }
}

impl EndpointOutcome {
    /// Creates a successful outcome.
    pub fn success(response: RouteResponse) -> Self {
        Self {
            result: Ok(response),
            effects: HttpResponseEffects::new(),
        }
    }

    /// Creates a failed outcome that will be mapped by `HttpErrorMapper`.
    pub fn failure(error: SoapError) -> Self {
        Self {
            result: Err(error),
            effects: HttpResponseEffects::new(),
        }
    }

    pub(crate) fn from_result(result: SoapResult<RouteResponse>) -> Self {
        Self {
            result,
            effects: HttpResponseEffects::new(),
        }
    }

    /// Returns the successful route response, if present.
    pub fn response(&self) -> Option<&RouteResponse> {
        self.result.as_ref().ok()
    }

    /// Returns the application or adapter error, if present.
    pub fn error(&self) -> Option<&SoapError> {
        self.result.as_ref().err()
    }

    /// Returns response effects accumulated independently from success/failure.
    pub fn effects(&self) -> &HttpResponseEffects {
        &self.effects
    }

    /// Returns mutable response effects for middleware post-processing.
    pub fn effects_mut(&mut self) -> &mut HttpResponseEffects {
        &mut self.effects
    }

    pub(crate) fn into_parts(self) -> (SoapResult<RouteResponse>, HttpResponseEffects) {
        (self.result, self.effects)
    }
}

/// Remaining middleware followed by the mapped endpoint target.
pub struct EndpointNext<'a> {
    pub(crate) middleware: &'a [Arc<dyn EndpointMiddleware>],
    pub(crate) target: &'a dyn EndpointTarget,
}

impl fmt::Debug for EndpointNext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointNext")
            .field("remaining_middleware", &self.middleware.len())
            .finish_non_exhaustive()
    }
}

impl<'a> EndpointNext<'a> {
    /// Continues the pipeline with the next middleware or endpoint target.
    pub fn run(self, request: &'a mut RouteRequest) -> BoxFuture<'a, EndpointOutcome> {
        if let Some((middleware, remaining)) = self.middleware.split_first() {
            middleware.handle(
                request,
                Self {
                    middleware: remaining,
                    target: self.target,
                },
            )
        } else {
            self.target.call(request)
        }
    }
}

/// Intercepts, delegates, or short-circuits after bounded body buffering.
pub trait EndpointMiddleware: Send + Sync {
    /// Declares portable endpoint policies actively enforced by this
    /// middleware.
    ///
    /// The router uses this declaration to reject configurations that would
    /// otherwise silently serve an endpoint without required enforcement.
    fn enforcement_capabilities(&self) -> &'static [HttpEnforcementCapability] {
        &[]
    }

    /// Handles a request around the remaining endpoint pipeline.
    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome>;
}
