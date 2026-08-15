//! Optional composition bridge for `soaprs-validation`.

use std::{fmt, sync::Arc};

use soaprs_core::BoxFuture;
use soaprs_validation::{HttpRequestContractValidator, HttpValidationInput, HttpValidationService};

use crate::{EndpointMiddleware, EndpointNext, EndpointOutcome, RouteRequest};

/// Validates the endpoint's logical request contracts before route I/O.
///
/// The configured validator owns contract resolution and validation behavior;
/// this middleware only presents the already-normalized request and buffered
/// encoded body.
pub struct ValidationMiddleware<V> {
    service: Arc<HttpValidationService<V>>,
}

impl<V> ValidationMiddleware<V> {
    /// Creates middleware around a framework-neutral validation service.
    pub fn new(service: HttpValidationService<V>) -> Self {
        Self {
            service: Arc::new(service),
        }
    }
}

impl<V> Clone for ValidationMiddleware<V> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
        }
    }
}

impl<V> fmt::Debug for ValidationMiddleware<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidationMiddleware")
            .finish_non_exhaustive()
    }
}

impl<V> EndpointMiddleware for ValidationMiddleware<V>
where
    V: HttpRequestContractValidator + 'static,
{
    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            let input = HttpValidationInput::new(request.endpoint(), request, request.body());
            if let Err(error) = self.service.validate(input).await {
                return EndpointOutcome::failure(error);
            }
            next.run(request).await
        })
    }
}
