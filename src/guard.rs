//! Ordered admission guards evaluated before request-body buffering.

use std::fmt;

use soaprs_core::{BoxFuture, SoapError};
use soaprs_http::{HttpEnforcementCapability, HttpResponseEffects};

use crate::RouteRequestHead;

/// A guard rejection together with transport-neutral response effects.
pub struct EndpointGuardRejection {
    error: SoapError,
    effects: HttpResponseEffects,
}

impl fmt::Debug for EndpointGuardRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointGuardRejection")
            .field("error", &self.error)
            .field("effects", &self.effects)
            .finish()
    }
}

impl EndpointGuardRejection {
    /// Rejects admission with an error mapped by the configured HTTP mapper.
    pub fn new(error: SoapError) -> Self {
        Self {
            error,
            effects: HttpResponseEffects::new(),
        }
    }

    /// Returns the rejection error.
    pub const fn error(&self) -> &SoapError {
        &self.error
    }

    /// Returns response effects attached to the rejection.
    pub const fn effects(&self) -> &HttpResponseEffects {
        &self.effects
    }

    /// Returns mutable response effects attached to the rejection.
    pub const fn effects_mut(&mut self) -> &mut HttpResponseEffects {
        &mut self.effects
    }

    /// Replaces response effects attached to the rejection.
    #[must_use]
    pub fn with_effects(mut self, effects: HttpResponseEffects) -> Self {
        self.effects = effects;
        self
    }
}

impl From<SoapError> for EndpointGuardRejection {
    fn from(error: SoapError) -> Self {
        Self::new(error)
    }
}

/// Result returned by a pre-body admission guard.
pub type EndpointGuardResult = Result<(), EndpointGuardRejection>;

/// Intercepts or rejects an endpoint after head normalization and before body reads.
///
/// Guards are intended for inexpensive admission decisions such as coarse rate
/// limiting, authentication, authorization, and request-context enrichment.
/// Body-dependent validation belongs in [`crate::EndpointMiddleware`].
pub trait EndpointGuard: Send + Sync {
    /// Declares portable endpoint policies actively enforced by this guard.
    fn enforcement_capabilities(&self) -> &'static [HttpEnforcementCapability] {
        &[]
    }

    /// Checks admission. Every configured guard runs in registration order
    /// until one rejects the request.
    fn check<'a>(&'a self, request: &'a mut RouteRequestHead)
    -> BoxFuture<'a, EndpointGuardResult>;
}
