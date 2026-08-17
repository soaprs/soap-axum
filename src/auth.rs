//! Optional composition bridge for `soaprs-auth-http`.

use std::{fmt, sync::Arc};

use soaprs_auth::{
    Authentication, Authenticator, Credential, DefaultAuthorizationEvaluator, Principal,
};
use soaprs_auth_http::{HttpAuthenticationService, HttpCredentialExtractor, unauthorized_effects};
use soaprs_core::{BoxFuture, SoapError, SoapErrorKind, SoapResult};
use soaprs_http::AuthChallenge;

use crate::{EndpointMiddleware, EndpointNext, EndpointOutcome, RouteRequest};

/// Typed authentication stored in request extensions by
/// [`AuthenticationMiddleware`].
pub struct AuthContext<P> {
    authentication: Arc<Authentication<P>>,
}

impl<P> Clone for AuthContext<P> {
    fn clone(&self) -> Self {
        Self {
            authentication: Arc::clone(&self.authentication),
        }
    }
}

impl<P> AuthContext<P> {
    fn new(authentication: Authentication<P>) -> Self {
        Self {
            authentication: Arc::new(authentication),
        }
    }

    /// Returns the complete typed authentication.
    pub fn authentication(&self) -> &Authentication<P> {
        &self.authentication
    }

    /// Returns the authenticated principal.
    pub fn principal(&self) -> &P {
        self.authentication.principal()
    }
}

impl<P> fmt::Debug for AuthContext<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthContext([REDACTED])")
    }
}

/// Enforces an endpoint's authorization policy after authentication.
///
/// Applications can replace the built-in implementation to resolve named
/// policies with resource context while keeping that logic outside the Axum
/// adapter.
pub trait HttpAuthorization<P>: Send + Sync {
    /// Authorizes one normalized request.
    fn authorize<'a>(
        &'a self,
        authentication: Option<&'a Authentication<P>>,
        request: &'a RouteRequest,
    ) -> BoxFuture<'a, SoapResult<()>>;
}

/// Uses soaprs' built-in role, permission, strategy, and identity evaluation.
/// Named policies remain explicitly unsupported until replaced by an
/// application [`HttpAuthorization`] implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltInAuthorization;

impl<P> HttpAuthorization<P> for BuiltInAuthorization
where
    P: Principal,
{
    fn authorize<'a>(
        &'a self,
        authentication: Option<&'a Authentication<P>>,
        request: &'a RouteRequest,
    ) -> BoxFuture<'a, SoapResult<()>> {
        Box::pin(async move {
            DefaultAuthorizationEvaluator
                .evaluate(authentication, &request.endpoint().authorization)?
                .enforce()
        })
    }
}

/// Authenticates and authorizes requests through framework-neutral soaprs
/// ports, then exposes [`AuthContext`] to route I/O and HTTP handlers.
pub struct AuthenticationMiddleware<E, A, P> {
    service: Arc<HttpAuthenticationService<E, A, P>>,
    authorization: Arc<dyn HttpAuthorization<P>>,
    challenges: Vec<AuthChallenge>,
}

impl<E, A, P> AuthenticationMiddleware<E, A, P> {
    /// Creates middleware using built-in authorization evaluation.
    pub fn new(service: HttpAuthenticationService<E, A, P>) -> Self
    where
        P: Principal + 'static,
    {
        Self {
            service: Arc::new(service),
            authorization: Arc::new(BuiltInAuthorization),
            challenges: Vec::new(),
        }
    }

    /// Appends a challenge applied when authentication returns `Unauthorized`.
    #[must_use]
    pub fn challenge(mut self, challenge: AuthChallenge) -> Self {
        self.challenges.push(challenge);
        self
    }

    /// Replaces built-in policy evaluation, for example for named resource
    /// policies owned by the application.
    #[must_use]
    pub fn authorization<Z>(mut self, authorization: Z) -> Self
    where
        Z: HttpAuthorization<P> + 'static,
    {
        self.authorization = Arc::new(authorization);
        self
    }

    fn failure(&self, error: SoapError) -> EndpointOutcome {
        let is_unauthorized = error.kind() == SoapErrorKind::Unauthorized;
        let mut outcome = EndpointOutcome::failure(error);
        if is_unauthorized && !self.challenges.is_empty() {
            match unauthorized_effects(self.challenges.iter()) {
                Ok(effects) => *outcome.effects_mut() = effects,
                Err(error) => return EndpointOutcome::failure(error),
            }
        }
        outcome
    }
}

impl<E, A, P> Clone for AuthenticationMiddleware<E, A, P> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            authorization: Arc::clone(&self.authorization),
            challenges: self.challenges.clone(),
        }
    }
}

impl<E, A, P> fmt::Debug for AuthenticationMiddleware<E, A, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticationMiddleware")
            .field("challenges", &self.challenges)
            .finish_non_exhaustive()
    }
}

impl<E, A, P> EndpointMiddleware for AuthenticationMiddleware<E, A, P>
where
    E: HttpCredentialExtractor + Send + Sync + 'static,
    A: Authenticator<Credential, P> + Send + Sync + 'static,
    P: Principal + Send + Sync + 'static,
{
    fn enforcement_capabilities(&self) -> &'static [soaprs_http::HttpEnforcementCapability] {
        &[soaprs_http::HttpEnforcementCapability::Authentication]
    }

    fn handle<'a>(
        &'a self,
        request: &'a mut RouteRequest,
        next: EndpointNext<'a>,
    ) -> BoxFuture<'a, EndpointOutcome> {
        Box::pin(async move {
            let authentication = match self
                .service
                .authenticate(request, &request.endpoint().authorization)
                .await
            {
                Ok(authentication) => authentication,
                Err(error) => return self.failure(error),
            };
            if let Err(error) = self
                .authorization
                .authorize(authentication.as_ref(), request)
                .await
            {
                return self.failure(error);
            }
            if let Some(authentication) = authentication {
                request
                    .extensions_mut()
                    .insert(AuthContext::new(authentication));
            }
            next.run(request).await
        })
    }
}
